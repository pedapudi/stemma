//! Target derivation from BIRD gold SQL (docs/design/06-evaluation.md), plus
//! the denotation-verified dataset builder (docs/design/07-eval-harness.md).
//!
//! Two outputs share the extraction machinery:
//! - `derive` — the original per-question target dump (tables, raw value
//!   targets, parse errors) for corpus statistics;
//! - `dataset` — the harness input: string value targets are DENOTATION-
//!   VERIFIED against the actual database instance (the gold predicate must
//!   select rows; Zhong 2020's lesson applied at derivation time), the gold
//!   `(table, column, rowid set)` is recorded per target, and each question
//!   is assigned a tier by mechanical rule.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use sqlparser::ast::{
    visit_expressions, visit_relations, BinaryOperator, Expr, TableFactor, Value, Visit, Visitor,
};
use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::Parser as SqlParser;

use crate::dataset::{EvalQuestion, Target, MAX_GOLD_ROWIDS};

/// One BIRD question as shipped in dev.json.
#[derive(Deserialize)]
pub struct BirdQuestion {
    pub question_id: u32,
    pub db_id: String,
    pub question: String,
    #[serde(default)]
    pub evidence: String,
    #[serde(rename = "SQL")]
    pub sql: String,
}

/// A literal a mention must resolve to: `column op literal` in the gold SQL.
#[derive(Serialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ValueTarget {
    pub column: String,
    /// The alias or table qualifying the column in the gold SQL (`T1.city` →
    /// "t1", lowercased), resolved against the statement's alias map during
    /// verification. None for bare columns.
    pub qualifier: Option<String>,
    pub op: String,
    pub literal: String,
    /// True for string literals — the value-linking subset stemma targets
    /// first; numeric/date literals need type-aware handling later.
    pub is_string: bool,
}

#[derive(Serialize)]
pub struct QuestionTargets {
    pub question_id: u32,
    pub db_id: String,
    pub question: String,
    /// BIRD's human evidence, kept only as reference for the
    /// evidence-reconstruction metric — never shown to the resolver.
    pub evidence: String,
    pub tables: BTreeSet<String>,
    pub value_targets: Vec<ValueTarget>,
    /// Gold SQL failed to parse; question kept for bookkeeping.
    pub parse_error: Option<String>,
}

pub fn load_questions(path: &Path, db_ids: &[String]) -> anyhow::Result<Vec<BirdQuestion>> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let all: Vec<BirdQuestion> = serde_json::from_str(&raw).context("parsing questions JSON")?;
    let filter: BTreeSet<_> = db_ids.iter().cloned().collect();
    Ok(all
        .into_iter()
        .filter(|q| filter.is_empty() || filter.contains(&q.db_id))
        .collect())
}

/// The original `derive` subcommand: raw targets + corpus stats.
pub fn derive(questions: PathBuf, out: PathBuf, db_ids: Vec<String>) -> anyhow::Result<()> {
    let all = load_questions(&questions, &db_ids)?;
    let mut results = Vec::new();
    let mut parse_failures = 0usize;
    for q in all {
        let (tables, value_targets, parse_error) = match derive_from_sql(&q.sql) {
            Ok((t, v, _aliases)) => (t, v, None),
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

/// Why a raw value target did not become a dataset target. Every discard is
/// counted; silent drops would bias the set toward what verification finds
/// easy.
#[derive(Debug, Default, Serialize, Clone)]
pub struct VerifyStats {
    pub verified: usize,
    /// Non-string or non-equality/LIKE/IN targets: derived, not yet scored.
    pub skipped_kind: usize,
    /// The gold predicate selects zero rows in the instance — the literal
    /// does not denote (data changed, or the gold SQL encodes a workaround).
    pub no_rows: usize,
    /// Unqualified column present in >1 referenced table with matching rows.
    pub ambiguous_table: usize,
    /// Column not found in any referenced table.
    pub missing_column: usize,
    pub sql_error: usize,
}

#[derive(Serialize)]
struct DatasetHeader<'a> {
    r#type: &'static str,
    dataset: String,
    version: u32,
    source: &'a str,
    counts: BTreeMap<String, usize>,
    questions_total: usize,
    questions_kept: usize,
    parse_failures: usize,
    verify: VerifyStats,
}

/// The `dataset` subcommand: derive → verify → tier → JSONL per corpus.
pub fn dataset(
    questions: PathBuf,
    db_root: PathBuf,
    out_dir: PathBuf,
    db_ids: Vec<String>,
    source: String,
) -> anyhow::Result<()> {
    let all = load_questions(&questions, &db_ids)?;
    anyhow::ensure!(!all.is_empty(), "no questions selected");
    std::fs::create_dir_all(&out_dir).with_context(|| format!("creating {}", out_dir.display()))?;

    let mut by_db: BTreeMap<String, Vec<BirdQuestion>> = BTreeMap::new();
    for q in all {
        by_db.entry(q.db_id.clone()).or_default().push(q);
    }

    for (db_id, qs) in by_db {
        let db_path = db_root.join(&db_id).join(format!("{db_id}.sqlite"));
        anyhow::ensure!(
            db_path.exists(),
            "database not found: {}",
            db_path.display()
        );
        let conn = open_readonly(&db_path)?;

        let mut stats = VerifyStats::default();
        let mut parse_failures = 0usize;
        let mut kept: Vec<EvalQuestion> = Vec::new();
        let total = qs.len();
        for q in qs {
            let (tables, raw_targets, aliases) = match derive_from_sql(&q.sql) {
                Ok(v) => v,
                Err(_) => {
                    parse_failures += 1;
                    continue;
                }
            };
            let mut targets: Vec<Target> = Vec::new();
            for vt in &raw_targets {
                match verify_target(&conn, vt, &tables, &aliases, &mut stats)? {
                    Some(t) => targets.push(t),
                    None => {}
                }
            }
            if targets.is_empty() {
                continue; // not a value-linking question
            }
            let (tier, tier_rule) = assign_tier(&targets, &tables);
            kept.push(EvalQuestion {
                id: format!("bird/{db_id}/{}", q.question_id),
                corpus: db_id.clone(),
                question: q.question,
                tier,
                nil: false,
                targets,
                provenance: serde_json::json!({
                    "source": source,
                    "question_id": q.question_id,
                    "gold_sql": q.sql,
                    // Reference only for evidence reconstruction —
                    // never shown to the resolver.
                    "evidence": q.evidence,
                    "tier_rule": tier_rule,
                    "verification": "denotation",
                }),
            });
        }

        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for q in &kept {
            *counts.entry(q.tier.clone()).or_default() += 1;
        }
        let header = DatasetHeader {
            r#type: "header",
            dataset: format!("bird-{db_id}"),
            version: 1,
            source: &source,
            counts: counts.clone(),
            questions_total: total,
            questions_kept: kept.len(),
            parse_failures,
            verify: stats.clone(),
        };
        let out_path = out_dir.join(format!("bird-{db_id}.jsonl"));
        let mut lines = vec![serde_json::to_string(&header)?];
        for q in &kept {
            lines.push(serde_json::to_string(q)?);
        }
        std::fs::write(&out_path, lines.join("\n") + "\n")
            .with_context(|| format!("writing {}", out_path.display()))?;
        println!(
            "{db_id}: {}/{} questions kept ({} parse failures), tiers {:?}, \
             targets verified {} / no-rows {} / ambiguous {} / missing-col {} / skipped-kind {} / sql-err {} -> {}",
            kept.len(),
            total,
            parse_failures,
            counts,
            stats.verified,
            stats.no_rows,
            stats.ambiguous_table,
            stats.missing_column,
            stats.skipped_kind,
            stats.sql_error,
            out_path.display()
        );
    }
    Ok(())
}

pub fn open_readonly(path: &Path) -> anyhow::Result<rusqlite::Connection> {
    rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("opening {}", path.display()))
}

/// Denotation-verify one raw target: run the gold predicate against the
/// instance and record the rows it selects. Returns None (with `stats`
/// updated) when the target is out of scope or fails verification.
fn verify_target(
    conn: &rusqlite::Connection,
    vt: &ValueTarget,
    referenced_tables: &BTreeSet<String>,
    aliases: &BTreeMap<String, String>,
    stats: &mut VerifyStats,
) -> anyhow::Result<Option<Target>> {
    let (op_sql, match_mode) = match (vt.op.as_str(), vt.is_string) {
        ("=", true) | ("in", true) => ("=", "exact"),
        ("like", true) => ("LIKE", "like"),
        _ => {
            stats.skipped_kind += 1;
            return Ok(None);
        }
    };

    // Resolve the table: alias/table qualifier first, else every referenced
    // table that has the column.
    let candidates: Vec<String> = match &vt.qualifier {
        Some(q) => match aliases.get(&q.to_lowercase()) {
            Some(t) => vec![t.clone()],
            None => {
                stats.missing_column += 1;
                return Ok(None);
            }
        },
        None => referenced_tables
            .iter()
            .filter(|t| table_has_column(conn, t, &vt.column))
            .cloned()
            .collect(),
    };
    if candidates.is_empty() {
        stats.missing_column += 1;
        return Ok(None);
    }

    let mut matches: Vec<(String, Vec<i64>, bool)> = Vec::new();
    for t in &candidates {
        if !table_has_column(conn, t, &vt.column) {
            stats.missing_column += 1;
            return Ok(None);
        }
        let sql = format!(
            "SELECT rowid FROM \"{}\" WHERE \"{}\" {} ?1 LIMIT {}",
            t.replace('"', "\"\""),
            vt.column.replace('"', "\"\""),
            op_sql,
            MAX_GOLD_ROWIDS + 1
        );
        let rows: Vec<i64> = match conn.prepare(&sql).and_then(|mut s| {
            s.query_map([&vt.literal], |r| r.get(0))?
                .collect::<Result<Vec<i64>, _>>()
        }) {
            Ok(rows) => rows,
            Err(_) => {
                stats.sql_error += 1;
                return Ok(None);
            }
        };
        if !rows.is_empty() {
            let truncated = rows.len() > MAX_GOLD_ROWIDS;
            matches.push((
                t.clone(),
                rows.into_iter().take(MAX_GOLD_ROWIDS).collect(),
                truncated,
            ));
        }
    }
    match matches.len() {
        0 => {
            stats.no_rows += 1;
            Ok(None)
        }
        1 => {
            stats.verified += 1;
            let (table, rowids, rowids_truncated) = matches.remove(0);
            Ok(Some(Target {
                table,
                column: vt.column.clone(),
                literal: vt.literal.clone(),
                match_mode: match_mode.into(),
                rowids,
                rowids_truncated,
            }))
        }
        _ => {
            stats.ambiguous_table += 1;
            Ok(None)
        }
    }
}

fn table_has_column(conn: &rusqlite::Connection, table: &str, column: &str) -> bool {
    conn.query_row(
        "SELECT count(*) FROM pragma_table_info(?1) WHERE lower(name) = lower(?2)",
        rusqlite::params![table, column],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}

/// Mechanical tier assignment (07-eval-harness.md):
/// - L4 when the gold rows span ≥2 distinct tables (cross-record co-answer);
/// - L3 when there are ≥2 value targets and the gold SQL joins ≥2 tables
///   (the join path is what disambiguates the mentions);
/// - anchor otherwise. paraphrase and absent never come from BIRD — they are constructed
///   by the synthetic generator with their own mechanical verification.
fn assign_tier(targets: &[Target], schema_tables: &BTreeSet<String>) -> (String, &'static str) {
    let target_tables: BTreeSet<&str> = targets.iter().map(|t| t.table.as_str()).collect();
    if target_tables.len() >= 2 {
        return ("cross-record".into(), "gold rows in >=2 distinct tables");
    }
    if targets.len() >= 2 && schema_tables.len() >= 2 {
        return ("join".into(), ">=2 mentions, gold SQL joins across tables");
    }
    ("anchor".into(), "default: lexical anchor")
}

/// Collects table aliases: `FROM frpm AS T1` → {"t1": "frpm", "frpm": "frpm"}.
struct AliasCollector {
    map: BTreeMap<String, String>,
}

impl Visitor for AliasCollector {
    type Break = ();
    fn pre_visit_table_factor(&mut self, tf: &TableFactor) -> ControlFlow<()> {
        if let TableFactor::Table { name, alias, .. } = tf {
            if let Some(last) = name.0.last() {
                let table = strip_quotes(&last.to_string());
                if let Some(a) = alias {
                    self.map.insert(
                        strip_quotes(&a.name.to_string()).to_lowercase(),
                        table.clone(),
                    );
                }
                self.map.insert(table.to_lowercase(), table);
            }
        }
        ControlFlow::Continue(())
    }
}

/// Extracts referenced tables, column/literal predicates, and the alias map
/// from one gold SQL.
pub fn derive_from_sql(
    sql: &str,
) -> anyhow::Result<(BTreeSet<String>, Vec<ValueTarget>, BTreeMap<String, String>)> {
    let statements = SqlParser::parse_sql(&SQLiteDialect {}, sql)?;

    let mut tables = BTreeSet::new();
    let mut values = BTreeSet::new();
    let mut aliases = AliasCollector {
        map: BTreeMap::new(),
    };
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
        let _ = stmt.visit(&mut aliases);
    }
    Ok((tables, values.into_iter().collect(), aliases.map))
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
            if let (Some((col, qual)), Some((lit, is_string))) = (column_name(left), literal(right))
            {
                out.insert(ValueTarget {
                    column: col,
                    qualifier: qual,
                    op: op_str.into(),
                    literal: lit,
                    is_string,
                });
            } else if let (Some((col, qual)), Some((lit, is_string))) =
                (column_name(right), literal(left))
            {
                out.insert(ValueTarget {
                    column: col,
                    qualifier: qual,
                    op: op_str.into(),
                    literal: lit,
                    is_string,
                });
            }
        }
        Expr::Like { expr, pattern, .. } | Expr::ILike { expr, pattern, .. } => {
            if let (Some((col, qual)), Some((lit, _))) = (column_name(expr), literal(pattern)) {
                out.insert(ValueTarget {
                    column: col,
                    qualifier: qual,
                    op: "like".into(),
                    literal: lit,
                    is_string: true,
                });
            }
        }
        Expr::InList { expr, list, .. } => {
            if let Some((col, qual)) = column_name(expr) {
                for item in list {
                    if let Some((lit, is_string)) = literal(item) {
                        out.insert(ValueTarget {
                            column: col.clone(),
                            qualifier: qual.clone(),
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

/// The column name (and optional qualifier) of a plain column reference.
fn column_name(expr: &Expr) -> Option<(String, Option<String>)> {
    match expr {
        Expr::Identifier(id) => Some((strip_quotes(&id.to_string()), None)),
        Expr::CompoundIdentifier(parts) => {
            let col = parts.last().map(|p| strip_quotes(&p.to_string()))?;
            let qual = if parts.len() >= 2 {
                Some(strip_quotes(&parts[parts.len() - 2].to_string()))
            } else {
                None
            };
            Some((col, qual))
        }
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
    s.trim_matches(|c| c == '`' || c == '"' || c == '\'')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_tables_and_values() {
        let (tables, values, aliases) = derive_from_sql(
            "SELECT T1.name FROM offices AS T1 INNER JOIN people AS T2 \
             ON T1.id = T2.office_id \
             WHERE T1.city = 'Seattle' AND T2.name LIKE '%Chen%' AND T1.id IN (3, 4)",
        )
        .unwrap();
        assert_eq!(
            tables,
            BTreeSet::from(["offices".to_string(), "people".to_string()])
        );
        assert_eq!(aliases.get("t1"), Some(&"offices".to_string()));
        assert_eq!(aliases.get("t2"), Some(&"people".to_string()));
        let strings: Vec<_> = values
            .iter()
            .filter(|v| v.is_string)
            .map(|v| (v.column.as_str(), v.literal.as_str()))
            .collect();
        assert!(strings.contains(&("city", "Seattle")));
        assert!(strings.contains(&("name", "%Chen%")));
        let city = values.iter().find(|v| v.column == "city").unwrap();
        assert_eq!(city.qualifier.as_deref(), Some("T1"));
        let nums: Vec<_> = values
            .iter()
            .filter(|v| !v.is_string)
            .map(|v| v.literal.as_str())
            .collect();
        assert!(nums.contains(&"3") && nums.contains(&"4"));
    }

    #[test]
    fn join_keys_are_not_value_targets() {
        let (_, values, _) =
            derive_from_sql("SELECT * FROM a JOIN b ON a.id = b.a_id WHERE a.x = 'y'").unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].column, "x");
    }

    fn test_conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE offices(id INTEGER PRIMARY KEY, city TEXT);
             CREATE TABLE people(id INTEGER PRIMARY KEY, name TEXT, office_id INTEGER);
             INSERT INTO offices VALUES (17, 'Seattle'), (18, 'Portland');
             INSERT INTO people VALUES (1, 'Wei Chen', 17), (2, 'Dana Chen', 18);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn denotation_verification_records_gold_rows() {
        let conn = test_conn();
        let tables: BTreeSet<String> = ["offices".to_string(), "people".to_string()].into();
        let aliases: BTreeMap<String, String> = [("t1".to_string(), "offices".to_string())].into();
        let mut stats = VerifyStats::default();
        let vt = ValueTarget {
            column: "city".into(),
            qualifier: Some("T1".into()),
            op: "=".into(),
            literal: "Seattle".into(),
            is_string: true,
        };
        let t = verify_target(&conn, &vt, &tables, &aliases, &mut stats)
            .unwrap()
            .expect("verifies");
        assert_eq!(t.table, "offices");
        assert_eq!(t.rowids, vec![17]);
        assert_eq!(stats.verified, 1);
    }

    #[test]
    fn denotation_failure_is_flagged_not_kept() {
        let conn = test_conn();
        let tables: BTreeSet<String> = ["offices".to_string()].into();
        let mut stats = VerifyStats::default();
        let vt = ValueTarget {
            column: "city".into(),
            qualifier: None,
            op: "=".into(),
            literal: "Atlantis".into(),
            is_string: true,
        };
        let t = verify_target(&conn, &vt, &tables, &BTreeMap::new(), &mut stats).unwrap();
        assert!(t.is_none());
        assert_eq!(stats.no_rows, 1);
    }

    #[test]
    fn like_targets_verify_with_wildcards() {
        let conn = test_conn();
        let tables: BTreeSet<String> = ["people".to_string()].into();
        let mut stats = VerifyStats::default();
        let vt = ValueTarget {
            column: "name".into(),
            qualifier: None,
            op: "like".into(),
            literal: "%Chen%".into(),
            is_string: true,
        };
        let t = verify_target(&conn, &vt, &tables, &BTreeMap::new(), &mut stats)
            .unwrap()
            .expect("verifies");
        assert_eq!(t.match_mode, "like");
        assert_eq!(t.rowids, vec![1, 2]);
    }

    fn tgt(table: &str, rowids: Vec<i64>) -> Target {
        Target {
            table: table.into(),
            column: "c".into(),
            literal: "x".into(),
            match_mode: "exact".into(),
            rowids,
            rowids_truncated: false,
        }
    }

    #[test]
    fn tier_rules() {
        let two_tables: BTreeSet<String> = ["a".to_string(), "b".to_string()].into();
        let one_table: BTreeSet<String> = ["a".to_string()].into();
        // Single target, single table: L1.
        assert_eq!(assign_tier(&[tgt("a", vec![1])], &one_table).0, "anchor");
        // Two targets in one table, gold SQL joins: L3.
        assert_eq!(
            assign_tier(&[tgt("a", vec![1]), tgt("a", vec![2])], &two_tables).0,
            "join"
        );
        // Gold rows across two tables: L4.
        assert_eq!(
            assign_tier(&[tgt("a", vec![1]), tgt("b", vec![2])], &two_tables).0,
            "cross-record"
        );
    }
}
