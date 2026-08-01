//! stemma-eval: derives resolution targets from BIRD and scores resolver output.
//!
//! BIRD's leaderboard numbers are conditional on human-written "evidence"
//! (pre-solved entity/value linking). stemma's eval protocol runs in the
//! no-evidence setting and asks: how much of that linking can we reconstruct?
//! Ground truth is derived from the gold SQL, not hand-labeled:
//!   - value targets: column/literal predicates in WHERE-class clauses
//!   - schema targets: tables referenced by the gold SQL
//!
//! Milestone 1 ships `derive` (targets + corpus stats). Scoring resolver
//! output against these targets lands with the first resolver (milestone 2).

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use sqlparser::ast::{visit_expressions, visit_relations, BinaryOperator, Expr, Value};
use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::Parser as SqlParser;

#[derive(Parser)]
#[command(name = "stemma-eval", about = "BIRD no-evidence evaluation harness")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Derive resolution targets from a BIRD-format question file.
    Derive {
        /// Path to dev.json (BIRD format).
        #[arg(long)]
        questions: PathBuf,
        /// Where to write derived targets (JSON).
        #[arg(long)]
        out: PathBuf,
        /// Restrict to these db_ids (repeatable). Empty = all.
        #[arg(long = "db-id")]
        db_ids: Vec<String>,
    },
}

/// One BIRD question as shipped in dev.json.
#[derive(Deserialize)]
struct BirdQuestion {
    question_id: u32,
    db_id: String,
    question: String,
    #[serde(default)]
    evidence: String,
    #[serde(rename = "SQL")]
    sql: String,
}

/// A literal a mention must resolve to: `column op literal` in the gold SQL.
#[derive(Serialize, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ValueTarget {
    column: String,
    op: String,
    literal: String,
    /// True for string literals — the value-linking subset stemma targets
    /// first; numeric/date literals need type-aware handling later.
    is_string: bool,
}

#[derive(Serialize)]
struct QuestionTargets {
    question_id: u32,
    db_id: String,
    question: String,
    /// BIRD's human evidence, kept only as reference for the
    /// evidence-reconstruction metric — never shown to the resolver.
    evidence: String,
    tables: BTreeSet<String>,
    value_targets: Vec<ValueTarget>,
    /// Gold SQL failed to parse; question kept for bookkeeping.
    parse_error: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    match args.cmd {
        Cmd::Derive {
            questions,
            out,
            db_ids,
        } => derive(questions, out, db_ids),
    }
}

fn derive(questions: PathBuf, out: PathBuf, db_ids: Vec<String>) -> anyhow::Result<()> {
    let raw = std::fs::read_to_string(&questions)
        .with_context(|| format!("reading {}", questions.display()))?;
    let all: Vec<BirdQuestion> = serde_json::from_str(&raw).context("parsing questions JSON")?;
    let filter: BTreeSet<_> = db_ids.into_iter().collect();

    let mut results = Vec::new();
    let mut parse_failures = 0usize;
    for q in all {
        if !filter.is_empty() && !filter.contains(&q.db_id) {
            continue;
        }
        let (tables, value_targets, parse_error) = match derive_from_sql(&q.sql) {
            Ok((t, v)) => (t, v, None),
            Err(e) => {
                parse_failures += 1;
                (BTreeSet::new(), Vec::new(), Some(e.to_string()))
            }
        };
        results.push(QuestionTargets {
            question_id: q.question_id,
            db_id: q.db_id,
            question: q.question,
            evidence: q.evidence,
            tables,
            value_targets,
            parse_error,
        });
    }

    let total = results.len();
    let with_string_values = results
        .iter()
        .filter(|r| r.value_targets.iter().any(|v| v.is_string))
        .count();
    let total_value_targets: usize = results.iter().map(|r| r.value_targets.len()).sum();

    std::fs::write(&out, serde_json::to_string_pretty(&results)?)
        .with_context(|| format!("writing {}", out.display()))?;

    println!("questions:                {total}");
    println!("gold-SQL parse failures:  {parse_failures}");
    println!("total value targets:      {total_value_targets}");
    println!(
        "questions with >=1 string value target (value-linking subset): {with_string_values} ({:.1}%)",
        if total > 0 { 100.0 * with_string_values as f64 / total as f64 } else { 0.0 }
    );
    println!("targets written to {}", out.display());
    Ok(())
}

/// Extracts referenced tables and column/literal predicates from one gold SQL.
fn derive_from_sql(sql: &str) -> anyhow::Result<(BTreeSet<String>, Vec<ValueTarget>)> {
    let statements = SqlParser::parse_sql(&SQLiteDialect {}, sql)?;

    let mut tables = BTreeSet::new();
    let mut values = BTreeSet::new();
    for stmt in &statements {
        let _ = visit_relations(stmt, |rel| {
            if let Some(last) = rel.0.last() {
                tables.insert(strip_quotes(&last.to_string()));
            }
            std::ops::ControlFlow::<()>::Continue(())
        });
        let _ = visit_expressions(stmt, |expr| {
            collect_value_targets(expr, &mut values);
            std::ops::ControlFlow::<()>::Continue(())
        });
    }
    Ok((tables, values.into_iter().collect()))
}

fn collect_value_targets(expr: &Expr, out: &mut BTreeSet<ValueTarget>) {
    match expr {
        Expr::BinaryOp { left, op, right } => {
            let op_str = match op {
                BinaryOperator::Eq => "=",
                BinaryOperator::NotEq => "!=",
                BinaryOperator::Gt => ">",
                BinaryOperator::GtEq => ">=",
                BinaryOperator::Lt => "<",
                BinaryOperator::LtEq => "<=",
                _ => return,
            };
            if let (Some(col), Some((lit, is_string))) = (column_name(left), literal(right)) {
                out.insert(ValueTarget {
                    column: col,
                    op: op_str.into(),
                    literal: lit,
                    is_string,
                });
            } else if let (Some(col), Some((lit, is_string))) = (column_name(right), literal(left))
            {
                out.insert(ValueTarget {
                    column: col,
                    op: op_str.into(),
                    literal: lit,
                    is_string,
                });
            }
        }
        Expr::Like { expr, pattern, .. } | Expr::ILike { expr, pattern, .. } => {
            if let (Some(col), Some((lit, _))) = (column_name(expr), literal(pattern)) {
                out.insert(ValueTarget {
                    column: col,
                    op: "like".into(),
                    literal: lit,
                    is_string: true,
                });
            }
        }
        Expr::InList { expr, list, .. } => {
            if let Some(col) = column_name(expr) {
                for item in list {
                    if let Some((lit, is_string)) = literal(item) {
                        out.insert(ValueTarget {
                            column: col.clone(),
                            op: "in".into(),
                            literal: lit,
                            is_string,
                        });
                    }
                }
            }
        }
        _ => {}
    }
}

/// The column name of a plain (possibly qualified) column reference.
fn column_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Identifier(id) => Some(strip_quotes(&id.to_string())),
        Expr::CompoundIdentifier(parts) => parts.last().map(|p| strip_quotes(&p.to_string())),
        _ => None,
    }
}

/// A literal value and whether it is a string.
fn literal(expr: &Expr) -> Option<(String, bool)> {
    match expr {
        Expr::Value(v) => match &v.value {
            Value::SingleQuotedString(s) | Value::DoubleQuotedString(s) => Some((s.clone(), true)),
            Value::Number(n, _) => Some((n.clone(), false)),
            _ => None,
        },
        _ => None,
    }
}

fn strip_quotes(s: &str) -> String {
    s.trim_matches(|c| c == '`' || c == '"' || c == '\'').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_tables_and_values() {
        let (tables, values) = derive_from_sql(
            "SELECT T1.name FROM offices AS T1 INNER JOIN people AS T2 \
             ON T1.id = T2.office_id \
             WHERE T1.city = 'Seattle' AND T2.name LIKE '%Chen%' AND T1.id IN (3, 4)",
        )
        .unwrap();
        assert_eq!(
            tables,
            BTreeSet::from(["offices".to_string(), "people".to_string()])
        );
        let strings: Vec<_> = values
            .iter()
            .filter(|v| v.is_string)
            .map(|v| (v.column.as_str(), v.literal.as_str()))
            .collect();
        assert!(strings.contains(&("city", "Seattle")));
        assert!(strings.contains(&("name", "%Chen%")));
        let nums: Vec<_> = values
            .iter()
            .filter(|v| !v.is_string)
            .map(|v| v.literal.as_str())
            .collect();
        assert!(nums.contains(&"3") && nums.contains(&"4"));
    }

    #[test]
    fn join_keys_are_not_value_targets() {
        let (_, values) =
            derive_from_sql("SELECT * FROM a JOIN b ON a.id = b.a_id WHERE a.x = 'y'").unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].column, "x");
    }
}
