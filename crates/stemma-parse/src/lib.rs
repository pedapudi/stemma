//! Grounded, read-only SQL proposal and validation.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;

use serde::{Deserialize, Serialize};
use sqlparser::ast::{visit_expressions, visit_relations, Expr, Statement, Value};
use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::Parser;
use stemma_lm::{ChatMessage, LmBackend};
use stemma_resolve::Trace;
use stemmadb::{StemmaDb, SRC_SCHEMA};

const MAX_PROPOSALS: usize = 3;
const MAX_RESPONSE_BYTES: usize = 32 * 1024;
const MAX_SQL_BYTES: usize = 8 * 1024;
const MAX_VALUE_BYTES: usize = 256;
const ASSUMPTIONS: &[&str] = &[
    "current_time",
    "timezone",
    "calendar",
    "unit",
    "result_granularity",
];

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("proposal service: {0}")]
    Backend(#[from] stemma_lm::Error),
    #[error("proposal response: {0}")]
    Response(String),
    #[error("no valid proposal: {0}")]
    Invalid(String),
    #[error("schema inspection: {0}")]
    Schema(#[from] stemmadb::rusqlite::Error),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ParameterValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroundingKind {
    Table,
    Column,
    Parameter,
    Join,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroundingUse {
    pub kind: GroundingKind,
    pub name: String,
    pub span_id: Option<usize>,
    pub candidate_index: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Proposal {
    sql: String,
    #[serde(default)]
    parameters: Vec<ParameterValue>,
    #[serde(default)]
    grounding: Vec<GroundingUse>,
    #[serde(default)]
    assumptions: Vec<String>,
}

#[derive(Deserialize)]
struct ProposalSet {
    proposals: Vec<Proposal>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ParsedQuery {
    pub sql: String,
    pub parameters: Vec<ParameterValue>,
    pub grounding: Vec<GroundingUse>,
    pub assumptions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ParseResult {
    pub proposals: Vec<ParsedQuery>,
    pub repaired: bool,
}

/// Proposes and mechanically admits at most three grounded read-only queries.
pub fn parse(trace: &Trace, db: &StemmaDb, lm: &dyn LmBackend) -> Result<ParseResult, ParseError> {
    let schema = relevant_schema(trace, db)?;
    if schema.is_empty() {
        return Err(ParseError::Invalid("no grounded tables".into()));
    }
    let prompt = prompt(trace, &schema);
    let schema_json = response_schema();
    let first = request(lm, &prompt, &schema_json)?;
    let (mut valid, failures) = decode(&first)
        .map(|set| validate_set(set, trace, &schema))
        .unwrap_or_else(|e| (Vec::new(), vec![e]));
    let mut repaired = false;
    if valid.is_empty() {
        repaired = true;
        let correction = format!(
            "{prompt}\nPrevious proposals were rejected: {}. Return corrected JSON only.",
            failures.join(", ")
        );
        let reply = request(lm, &correction, &schema_json)?;
        valid = validate_set(
            decode(&reply).map_err(ParseError::Response)?,
            trace,
            &schema,
        )
        .0;
    }
    if valid.is_empty() {
        return Err(ParseError::Invalid(failures.join(", ")));
    }
    Ok(ParseResult {
        proposals: valid,
        repaired,
    })
}

fn request(
    lm: &dyn LmBackend,
    prompt: &str,
    schema: &serde_json::Value,
) -> Result<String, ParseError> {
    let raw = lm.chat(&[
        ChatMessage::system("Propose safe read-only SQLite queries. Use ?1, ?2 placeholders for all user values. Return only the requested JSON."),
        ChatMessage::user(prompt),
    ], Some(schema))?;
    if raw.len() > MAX_RESPONSE_BYTES {
        return Err(ParseError::Response("response too large".into()));
    }
    Ok(raw)
}

fn decode(raw: &str) -> Result<ProposalSet, String> {
    serde_json::from_str(raw).map_err(|e| e.to_string())
}

fn relevant_schema(
    trace: &Trace,
    db: &StemmaDb,
) -> Result<BTreeMap<String, BTreeSet<String>>, ParseError> {
    let tables: BTreeSet<_> = trace
        .spans
        .iter()
        .flat_map(|s| &s.candidates)
        .map(|c| c.table.clone())
        .collect();
    let mut out = BTreeMap::new();
    for table in tables {
        let escaped = table.replace('"', "\"\"");
        let mut stmt = db
            .conn()
            .prepare(&format!("PRAGMA {SRC_SCHEMA}.table_info(\"{escaped}\")"))?;
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<BTreeSet<_>, _>>()?;
        if !columns.is_empty() {
            out.insert(table, columns);
        }
    }
    Ok(out)
}

fn prompt(trace: &Trace, schema: &BTreeMap<String, BTreeSet<String>>) -> String {
    let evidence: Vec<_> = trace.mentions.iter().filter_map(|&id| trace.spans.get(id)).map(|span| {
        let candidates: Vec<_> = span.candidates.iter().enumerate().filter(|(_, c)| c.selected || span.ambiguous).take(3)
            .map(|(candidate, c)| serde_json::json!({"candidate":candidate,"table":c.table,"column":c.column,"value":clip(&c.value)})).collect();
        serde_json::json!({"span":span.id,"text":span.text,"candidates":candidates})
    }).collect();
    serde_json::json!({"query":trace.query,"grounding":evidence,"schema":schema,"limit":MAX_PROPOSALS}).to_string()
}

fn clip(value: &str) -> &str {
    if value.len() <= MAX_VALUE_BYTES {
        return value;
    }
    let mut end = MAX_VALUE_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn validate_set(
    set: ProposalSet,
    trace: &Trace,
    schema: &BTreeMap<String, BTreeSet<String>>,
) -> (Vec<ParsedQuery>, Vec<String>) {
    let mut valid = Vec::new();
    let mut seen = BTreeSet::new();
    let mut failures = Vec::new();
    for proposal in set.proposals.into_iter().take(MAX_PROPOSALS) {
        match validate(proposal, trace, schema) {
            Ok(query) if seen.insert(query.sql.clone()) => valid.push(query),
            Ok(_) => {}
            Err(e) => failures.push(e),
        }
    }
    (valid, failures)
}

fn validate(
    p: Proposal,
    trace: &Trace,
    schema: &BTreeMap<String, BTreeSet<String>>,
) -> Result<ParsedQuery, String> {
    if p.sql.len() > MAX_SQL_BYTES {
        return Err("SQL too large".into());
    }
    if p.assumptions
        .iter()
        .any(|a| !ASSUMPTIONS.contains(&a.as_str()))
    {
        return Err("unknown assumption".into());
    }
    let mut statements = Parser::parse_sql(&SQLiteDialect {}, &p.sql).map_err(|e| e.to_string())?;
    if statements.len() != 1 {
        return Err("multiple statements".into());
    }
    let statement = statements.pop().unwrap();
    if !matches!(statement, Statement::Query(_)) {
        return Err("not a query".into());
    }

    let mut relations = BTreeSet::new();
    let _ = visit_relations(&statement, |name| {
        relations.insert(name.to_string().trim_matches('"').to_string());
        ControlFlow::<()>::Continue(())
    });
    if relations.is_empty()
        || relations
            .iter()
            .any(|t| !schema.contains_key(t.rsplit('.').next().unwrap_or(t)))
    {
        return Err("ungrounded table".into());
    }
    if relations.len() > 1 && !has_verified_join(&p.grounding, trace) {
        return Err("unverified join".into());
    }

    let all_columns: BTreeSet<_> = relations
        .iter()
        .filter_map(|t| schema.get(t.rsplit('.').next().unwrap_or(t)))
        .flat_map(|c| c.iter().map(String::as_str))
        .collect();
    let mut placeholders = BTreeSet::new();
    let mut bad_column = false;
    let mut raw_literal = false;
    let _ = visit_expressions(&statement, |expr| {
        match expr {
            Expr::Identifier(id) => bad_column |= !all_columns.contains(id.value.as_str()),
            Expr::CompoundIdentifier(ids) if ids.len() >= 2 => {
                bad_column |= !all_columns.contains(ids.last().unwrap().value.as_str())
            }
            Expr::Value(v) => match &v.value {
                Value::Placeholder(s) => {
                    placeholders.insert(s.clone());
                }
                Value::Null | Value::Boolean(_) => {}
                _ => raw_literal = true,
            },
            _ => {}
        }
        ControlFlow::<()>::Continue(())
    });
    if bad_column {
        return Err("ungrounded column".into());
    }
    if raw_literal {
        return Err("raw literal".into());
    }
    let expected: BTreeSet<_> = (1..=p.parameters.len()).map(|i| format!("?{i}")).collect();
    if placeholders != expected {
        return Err("parameter mismatch".into());
    }
    validate_grounding(
        &p.grounding,
        trace,
        schema,
        &relations,
        &all_columns,
        &p.parameters,
    )?;

    let sql = statement.to_string();
    Ok(ParsedQuery {
        sql,
        parameters: p.parameters,
        grounding: p.grounding,
        assumptions: p.assumptions,
    })
}

fn validate_grounding(
    uses: &[GroundingUse],
    trace: &Trace,
    schema: &BTreeMap<String, BTreeSet<String>>,
    relations: &BTreeSet<String>,
    columns: &BTreeSet<&str>,
    parameters: &[ParameterValue],
) -> Result<(), String> {
    let parameter_count = parameters.len();
    for use_ in uses {
        if let (Some(span), Some(candidate)) = (use_.span_id, use_.candidate_index) {
            let c = trace
                .spans
                .get(span)
                .and_then(|s| s.candidates.get(candidate))
                .ok_or("bad grounding reference")?;
            let matches = match use_.kind {
                GroundingKind::Table => c.table == use_.name,
                GroundingKind::Column => c.column == use_.name,
                GroundingKind::Parameter => {
                    let Some(position) = use_
                        .name
                        .strip_prefix('?')
                        .and_then(|n| n.parse::<usize>().ok())
                    else {
                        return Err("invalid parameter grounding".into());
                    };
                    position > 0 && position <= parameter_count
                }
                GroundingKind::Join => c.coherence.as_deref() == Some(&use_.name),
            };
            if !matches {
                return Err("grounding reference mismatch".into());
            }
        } else {
            let schema_backed = match use_.kind {
                GroundingKind::Table => relations
                    .iter()
                    .any(|t| t.rsplit('.').next() == Some(&use_.name)),
                GroundingKind::Column => columns.contains(use_.name.as_str()),
                _ => false,
            };
            if !schema_backed {
                return Err("missing grounding reference".into());
            }
        }
    }
    let grounded_parameters: BTreeSet<_> = uses
        .iter()
        .filter(|u| u.kind == GroundingKind::Parameter)
        .map(|u| u.name.as_str())
        .collect();
    let expected_parameters: BTreeSet<_> = (1..=parameter_count).map(|i| format!("?{i}")).collect();
    if grounded_parameters
        != expected_parameters
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
    {
        return Err("ungrounded parameter".into());
    }
    for use_ in uses.iter().filter(|u| u.kind == GroundingKind::Parameter) {
        let position = use_.name[1..]
            .parse::<usize>()
            .map_err(|_| "invalid parameter grounding")?;
        let candidate_index = use_.candidate_index.ok_or("missing candidate")?;
        let candidate = trace
            .spans
            .get(use_.span_id.ok_or("missing span")?)
            .and_then(|s| s.candidates.get(candidate_index))
            .ok_or("bad grounding reference")?;
        if !parameter_matches(&candidate.value, &parameters[position - 1]) {
            return Err("parameter value mismatch".into());
        }
    }
    for table in relations {
        let table = table.rsplit('.').next().unwrap_or(table);
        if !uses
            .iter()
            .any(|u| u.kind == GroundingKind::Table && u.name == table)
        {
            return Err("ungrounded table use".into());
        }
    }
    let _ = schema;
    Ok(())
}

fn parameter_matches(candidate: &str, parameter: &ParameterValue) -> bool {
    match parameter {
        ParameterValue::Null => candidate.eq_ignore_ascii_case("null"),
        ParameterValue::Integer(value) => candidate.parse::<i64>() == Ok(*value),
        ParameterValue::Real(value) => candidate.parse::<f64>().is_ok_and(|v| v == *value),
        ParameterValue::Text(value) => candidate == value,
    }
}

fn has_verified_join(uses: &[GroundingUse], trace: &Trace) -> bool {
    uses.iter().any(|use_| {
        let candidate = use_
            .span_id
            .zip(use_.candidate_index)
            .and_then(|(span, candidate)| {
                trace
                    .spans
                    .get(span)
                    .and_then(|span| span.candidates.get(candidate))
            });
        use_.kind == GroundingKind::Join
            && candidate.and_then(|candidate| candidate.coherence.as_deref())
                == Some(use_.name.as_str())
    })
}

fn response_schema() -> serde_json::Value {
    serde_json::json!({
        "type":"object","additionalProperties":false,"required":["proposals"],
        "properties":{"proposals":{"type":"array","maxItems":MAX_PROPOSALS,"items":{
            "type":"object","additionalProperties":false,"required":["sql","parameters","grounding","assumptions"],
            "properties":{
                "sql":{"type":"string"},
                "parameters":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["type"],"properties":{"type":{"enum":["null","integer","real","text"]},"value":{} }}},
                "grounding":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["kind","name","span_id","candidate_index"],"properties":{"kind":{"enum":["table","column","parameter","join"]},"name":{"type":"string"},"span_id":{"type":["integer","null"]},"candidate_index":{"type":["integer","null"]}}}},
                "assumptions":{"type":"array","items":{"type":"string"}}
            }
        }}}
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use stemma_lm::{LmIdentity, Result as LmResult};
    use stemma_resolve::{Candidate, Span};

    use super::*;

    struct Scripted {
        replies: Mutex<Vec<String>>,
        calls: Mutex<usize>,
    }

    impl Scripted {
        fn new(replies: &[&str]) -> Self {
            Self {
                replies: Mutex::new(replies.iter().rev().map(|s| s.to_string()).collect()),
                calls: Mutex::new(0),
            }
        }
    }

    impl LmBackend for Scripted {
        fn chat(&self, _: &[ChatMessage], _: Option<&serde_json::Value>) -> LmResult<String> {
            *self.calls.lock().unwrap() += 1;
            Ok(self.replies.lock().unwrap().pop().unwrap())
        }

        fn native_structured_output(&self) -> bool {
            true
        }

        fn identity(&self) -> LmIdentity {
            LmIdentity {
                backend: "scripted".into(),
                model: "fixture".into(),
            }
        }
    }

    fn fixture() -> (StemmaDb, Trace) {
        let db = StemmaDb::open_in_memory().unwrap();
        db.conn()
            .execute_batch(
                "CREATE TABLE src.people(name TEXT, city TEXT);\
             INSERT INTO src.people VALUES ('Ada', 'Seattle')",
            )
            .unwrap();
        let candidate = Candidate {
            table: "people".into(),
            column: "city".into(),
            rowid: 1,
            value: "Seattle".into(),
            value_truncated: false,
            score: 1.0,
            channels: Vec::new(),
            selected: true,
            reject_reason: None,
            is_doc: false,
            snippet: None,
            adjudicated: false,
            coherence: None,
            row_count: 1,
            dense_confidence: None,
            sample_rowids: vec![1],
            reach: 1,
        };
        let trace = Trace {
            query: "people in Seattle".into(),
            tokens: Vec::new(),
            spans: vec![Span {
                id: 0,
                text: "Seattle".into(),
                start: 10,
                end: 17,
                status: "selected".into(),
                candidates: vec![candidate],
                kg_alias: false,
                ambiguous: false,
                divergence: 0.0,
                admitted_by: None,
            }],
            mentions: vec![0],
            clarification: None,
            elapsed_ms: 0.0,
        };
        (db, trace)
    }

    fn proposal(sql: &str, value: &str) -> String {
        serde_json::json!({"proposals":[{
            "sql":sql,
            "parameters":[{"type":"text","value":value}],
            "grounding":[
                {"kind":"table","name":"people","span_id":0,"candidate_index":0},
                {"kind":"column","name":"name","span_id":null,"candidate_index":null},
                {"kind":"column","name":"city","span_id":0,"candidate_index":0},
                {"kind":"parameter","name":"?1","span_id":0,"candidate_index":0}
            ],
            "assumptions":[]
        }]})
        .to_string()
    }

    #[test]
    fn admits_grounded_parameterized_query() {
        let (db, trace) = fixture();
        let backend = Scripted::new(&[&proposal(
            "SELECT name FROM people WHERE city = ?1",
            "Seattle",
        )]);
        let result = parse(&trace, &db, &backend).unwrap();
        assert!(!result.repaired);
        assert_eq!(result.proposals.len(), 1);
        assert_eq!(
            result.proposals[0].parameters,
            [ParameterValue::Text("Seattle".into())]
        );
    }

    #[test]
    fn repairs_malformed_first_response_once() {
        let (db, trace) = fixture();
        let corrected = proposal("SELECT name FROM people WHERE city = ?1", "Seattle");
        let backend = Scripted::new(&["not json", &corrected]);
        assert!(parse(&trace, &db, &backend).unwrap().repaired);
        assert_eq!(*backend.calls.lock().unwrap(), 2);
    }

    #[test]
    fn rejects_writes_and_raw_literals() {
        let (db, trace) = fixture();
        for sql in [
            "DELETE FROM people WHERE city = ?1",
            "SELECT name FROM people WHERE city = 'Seattle'",
            "SELECT name FROM people; SELECT city FROM people",
        ] {
            let reply = proposal(sql, "Seattle");
            let backend = Scripted::new(&[&reply, &reply]);
            assert!(matches!(
                parse(&trace, &db, &backend),
                Err(ParseError::Invalid(_))
            ));
        }
    }

    #[test]
    fn rejects_parameter_not_equal_to_grounded_value() {
        let (db, trace) = fixture();
        let reply = proposal("SELECT name FROM people WHERE city = ?1", "Portland");
        let backend = Scripted::new(&[&reply, &reply]);
        assert!(matches!(
            parse(&trace, &db, &backend),
            Err(ParseError::Invalid(_))
        ));
    }

    #[test]
    fn deduplicates_normalized_queries() {
        let (db, trace) = fixture();
        let item = serde_json::from_str::<serde_json::Value>(&proposal(
            "SELECT name FROM people WHERE city = ?1",
            "Seattle",
        ))
        .unwrap()["proposals"][0]
            .clone();
        let reply = serde_json::json!({"proposals":[item.clone(), item]}).to_string();
        let backend = Scripted::new(&[&reply]);
        assert_eq!(parse(&trace, &db, &backend).unwrap().proposals.len(), 1);
    }
}
