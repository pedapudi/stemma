//! stemma-server: gRPC front door for the Resolve API.
//!
//! Serves Resolve (selected mentions with evidence) and Explain (the full
//! resolution trajectory, near-misses included) over tonic. On startup each
//! registered database gets its lexical index built in the .stemmadb store.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Context;
use clap::Parser;
use stemma_proto::v1::resolve_service_server::{ResolveService, ResolveServiceServer};
use stemma_proto::v1::{
    query_parameter, DeleteFeedbackRequest, DeleteFeedbackResponse, ExplainResponse, Feedback,
    FeedbackCategory, FeedbackList, FeedbackRequest, FeedbackScope, GroundingUse,
    ListFeedbackRequest, ParseRequest, ParseResponse, ParseStatus, QueryParameter, ResolveRequest,
    ResolveResponse, ValidationFailure,
};
use stemmadb::StemmaDb;
use tonic::{Request, Response, Status};

#[derive(Parser, Debug)]
#[command(name = "stemma-server", about = "stemma resolution service")]
struct Args {
    /// Path to a stemma config.json; flags below override its fields.
    /// Configuration comes from the file and flags only — never from
    /// environment variables.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Address to listen on (default 127.0.0.1:50051).
    #[arg(long)]
    listen: Option<SocketAddr>,

    /// Dense candidate source: exact (default) or usearch.
    #[arg(long)]
    dense_search: Option<String>,

    /// Databases to serve, as name=path/to/user.db (repeatable). The sidecar
    /// store is created next to the user DB as <path>.stemmadb.
    #[arg(long = "db", value_parser = parse_db_spec)]
    dbs: Vec<(String, PathBuf)>,

    /// OpenAI-compatible /v1/embeddings base URL enabling the dense channel
    /// (e.g. http://host:8081/v1). Absent = lexical + kg only.
    #[arg(long)]
    embed_endpoint: Option<String>,

    /// Model name for --embed-endpoint.
    #[arg(long)]
    embed_model: Option<String>,

    /// Query-side template for the embedder, with a "{query}" placeholder
    /// (e.g. "Instruct: ...\nQuery: {query}"). Endpoint, model and template
    /// must agree, so all three are configurable together; overrides
    /// server.embedder.query_template in the config file — but a template a
    /// served store's model_registry has on record outranks both, and a
    /// disagreement with it refuses at startup. Unset (and unrecorded), the
    /// template is chosen by model family (Qwen3-Embedding models get their
    /// published retrieval instruction, anything else embeds queries bare);
    /// pass "{query}" to force bare on any model.
    #[arg(long)]
    embed_query_template: Option<String>,

    /// OpenAI-compatible /v1/chat/completions base URL enabling the LM
    /// adjudication band (e.g. http://host:8080/v1). Absent = no LM;
    /// resolution is fully local.
    #[arg(long)]
    lm_endpoint: Option<String>,

    /// Model name for --lm-endpoint.
    #[arg(long)]
    lm_model: Option<String>,

    /// Extra request-body JSON merged into every LM call, from the config's
    /// server.lm.extra_body (no CLI flag — structured values belong in the
    /// file). E.g. vLLM's chat_template_kwargs {"enable_thinking": false}:
    /// adjudication is a forced choice, and reasoning tokens ahead of it are
    /// pure latency.
    #[arg(skip)]
    lm_extra_body: Option<serde_json::Value>,
}

/// The stemma config file (config.json). The server reads `databases` and
/// `server.*`; the console and MCP server read their own sections of the
/// same file, so one file describes one deployment.
#[derive(serde::Deserialize, Default)]
struct ConfigFile {
    #[serde(default)]
    databases: std::collections::BTreeMap<String, PathBuf>,
    #[serde(default)]
    server: ServerSection,
}

#[derive(serde::Deserialize, Default)]
struct ServerSection {
    listen: Option<SocketAddr>,
    dense_search: Option<String>,
    embedder: Option<EmbedderSection>,
    lm: Option<EndpointSection>,
}

#[derive(serde::Deserialize)]
struct EndpointSection {
    endpoint: String,
    model: String,
    #[serde(default)]
    extra_body: Option<serde_json::Value>,
}

/// The embedder carries one more knob than a plain endpoint: the query-side
/// template ("{query}" placeholder) that must agree with the model. Absent,
/// the default is looked up by model family
/// (`stemma_embed::default_query_template`).
#[derive(serde::Deserialize)]
struct EmbedderSection {
    endpoint: String,
    model: String,
    #[serde(default)]
    query_template: Option<String>,
}

/// Flags override the file; relative database paths in the file resolve
/// against the file's directory, so the config means the same thing from
/// any working directory.
fn merge_config(args: &mut Args) -> anyhow::Result<()> {
    let Some(path) = &args.config else {
        return Ok(());
    };
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading config {}", path.display()))?;
    let cfg: ConfigFile = serde_json::from_str(&text)
        .with_context(|| format!("parsing config {}", path.display()))?;
    let base = path.parent().unwrap_or_else(|| std::path::Path::new("."));

    if args.listen.is_none() {
        args.listen = cfg.server.listen;
    }
    if args.dense_search.is_none() {
        args.dense_search = cfg.server.dense_search;
    }
    if args.dbs.is_empty() {
        args.dbs = cfg
            .databases
            .into_iter()
            .map(|(name, p)| {
                let p = if p.is_absolute() { p } else { base.join(p) };
                (name, p)
            })
            .collect();
    }
    if let Some(e) = cfg.server.embedder {
        args.embed_endpoint.get_or_insert(e.endpoint);
        args.embed_model.get_or_insert(e.model);
        if let Some(t) = e.query_template {
            args.embed_query_template.get_or_insert(t);
        }
    }
    if let Some(l) = cfg.server.lm {
        args.lm_endpoint.get_or_insert(l.endpoint);
        args.lm_model.get_or_insert(l.model);
        if args.lm_extra_body.is_none() {
            args.lm_extra_body = l.extra_body;
        }
    }
    Ok(())
}

fn parse_db_spec(s: &str) -> Result<(String, PathBuf), String> {
    let (name, path) = s
        .split_once('=')
        .ok_or_else(|| format!("expected name=path, got {s:?}"))?;
    if name.is_empty() {
        return Err("database name must be non-empty".into());
    }
    Ok((name.to_string(), PathBuf::from(path)))
}

struct Resolver {
    // Mutex is adequate for the skeleton: SQLite connections are not Sync and
    // resolution is read-mostly. Revisit with a connection pool when the
    // pipeline lands.
    dbs: HashMap<String, Mutex<StemmaDb>>,
    dense_searches: HashMap<String, std::sync::Arc<stemma_resolve::DenseSearch>>,
    embedder: Option<std::sync::Arc<stemma_embed::CooldownEmbedder<stemma_embed::OpenAiEmbedder>>>,
    lm: Option<Box<dyn stemma_lm::LmBackend>>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct EpisodeEvidence {
    mentions: Vec<u32>,
    spans: Vec<EpisodeSpan>,
    clarification: Option<EpisodeClarification>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct EpisodeSpan {
    id: u32,
    candidates: Vec<EpisodeCandidate>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct EpisodeCandidate {
    table: String,
    column: String,
    rowid: i64,
    selected: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct EpisodeClarification {
    span_id: u32,
    options: Vec<Vec<u32>>,
}

impl From<&stemma_resolve::Trace> for EpisodeEvidence {
    fn from(trace: &stemma_resolve::Trace) -> Self {
        Self {
            mentions: trace.mentions.iter().map(|&id| id as u32).collect(),
            spans: trace
                .spans
                .iter()
                .map(|span| EpisodeSpan {
                    id: span.id as u32,
                    candidates: span
                        .candidates
                        .iter()
                        .map(|candidate| EpisodeCandidate {
                            table: candidate.table.clone(),
                            column: candidate.column.clone(),
                            rowid: candidate.rowid,
                            selected: candidate.selected,
                        })
                        .collect(),
                })
                .collect(),
            clarification: trace
                .clarification
                .as_ref()
                .map(|clarification| EpisodeClarification {
                    span_id: clarification.span_id as u32,
                    options: clarification
                        .options
                        .iter()
                        .map(|option| {
                            option
                                .candidate_indices
                                .iter()
                                .map(|&index| index as u32)
                                .collect()
                        })
                        .collect(),
                }),
        }
    }
}

impl Resolver {
    fn trace_for(
        &self,
        req: &ResolveRequest,
        episode_kind: &str,
    ) -> Result<(stemma_resolve::Trace, String), Status> {
        let db = self
            .dbs
            .get(&req.database)
            .ok_or_else(|| Status::not_found(format!("unknown database {:?}", req.database)))?;
        let db = db.lock().expect("stemmadb lock poisoned");
        let embedder = self
            .embedder
            .as_ref()
            .map(|e| e.as_ref() as &dyn stemma_embed::Embedder);
        // options.allow_lm gates the adjudication band per request: off, the
        // resolution is purely lexical/dense/KG and fully local.
        let lm = match req.options.as_ref().map(|o| o.allow_lm) {
            Some(true) => self.lm.as_deref(),
            _ => None,
        };
        let dense_search = self
            .dense_searches
            .get(&req.database)
            .expect("dense search registered with database");
        let trace = stemma_resolve::resolve_full_with_dense_search(
            &db,
            &req.query,
            embedder,
            lm,
            dense_search,
        )
        .map_err(|e| match e {
            stemma_resolve::Error::IndexMissing => Status::failed_precondition(e.to_string()),
            other => Status::internal(other.to_string()),
        })?;
        // Query history is store working memory; a failed write must never
        // fail the resolution. An empty episode id makes that degradation
        // visible and prevents feedback from attaching to unverifiable state.
        let mut episode_id = String::new();
        if !req.query.trim().is_empty() {
            let (source, session) = req
                .options
                .as_ref()
                .map(|o| (o.source.clone(), o.session.clone()))
                .unwrap_or_default();
            let revisions = evidence_revisions(&db);
            let evidence = serde_json::to_string(&EpisodeEvidence::from(&trace));
            let id = db
                .conn()
                .query_row("SELECT lower(hex(randomblob(16)))", [], |row| {
                    row.get::<_, String>(0)
                });
            if let (Ok((database_revision, vector_revision)), Ok(evidence), Ok(id)) =
                (revisions, evidence, id)
            {
                let written = db.conn().execute(
                    "INSERT INTO query_log
                     (query, mentions, elapsed_ms, source, session, episode_id,
                      episode_kind, database_revision, vector_revision, evidence_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    stemmadb::rusqlite::params![
                        req.query,
                        trace.mentions.len() as i64,
                        trace.elapsed_ms,
                        source,
                        session,
                        id,
                        episode_kind,
                        database_revision,
                        vector_revision,
                        evidence,
                    ],
                );
                if written.is_ok() {
                    episode_id = id;
                }
            }
        }
        Ok((trace, episode_id))
    }

    fn database(&self, name: &str) -> Result<&Mutex<StemmaDb>, Status> {
        self.dbs
            .get(name)
            .ok_or_else(|| Status::not_found(format!("unknown database {name:?}")))
    }

    fn record_parse_output(&self, database: &str, episode_id: &str, response: &ParseResponse) {
        if episode_id.is_empty() {
            return;
        }
        let Some(db) = self.dbs.get(database) else {
            return;
        };
        let db = db.lock().expect("stemmadb lock poisoned");
        let _ = db.conn().execute(
            "UPDATE query_log SET parse_json = ?1 WHERE episode_id = ?2",
            stemmadb::rusqlite::params![parse_output_json(response), episode_id],
        );
    }
}

fn evidence_revisions(db: &StemmaDb) -> anyhow::Result<(String, String)> {
    let has_derivations: bool = db.conn().query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'derivations')",
        [],
        |row| row.get(0),
    )?;
    let database = if has_derivations {
        stemma_ingest::corpus_fingerprint(db.conn())?
    } else {
        stemma_ingest::content_hash("")
    };
    let mut statement = db.conn().prepare(
        "SELECT vector_table, backend, model, revision, dimension, quantization,
                query_template, card_format
         FROM model_registry ORDER BY vector_table",
    )?;
    let vectors = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let vectors = serde_json::to_string(&vectors)?;
    Ok((database, stemma_ingest::content_hash(&vectors)))
}

fn parse_output_json(response: &ParseResponse) -> String {
    let status = ParseStatus::try_from(response.status)
        .map(|value| value.as_str_name())
        .unwrap_or("PARSE_STATUS_UNSPECIFIED");
    serde_json::json!({
        "status": status,
        "sql": response.sql,
        "parameters": response.parameters.iter().map(|parameter| serde_json::json!({
            "position": parameter.position,
            "value": parameter.value.as_ref().map(query_parameter_json),
        })).collect::<Vec<_>>(),
        "grounding_uses": response.grounding_uses.iter().map(|grounding| {
            serde_json::json!({
                "kind": grounding.kind,
                "name": grounding.name,
                "span_id": grounding.span_id,
                "candidate_index": grounding.candidate_index,
            })
        }).collect::<Vec<_>>(),
        "assumptions": response.assumptions,
        "validation_failures": response.validation_failures.iter().map(|failure| {
            serde_json::json!({
                "code": failure.code,
                "message": failure.message,
                "location": failure.location,
            })
        }).collect::<Vec<_>>(),
    })
    .to_string()
}

fn query_parameter_json(value: &query_parameter::Value) -> serde_json::Value {
    match value {
        query_parameter::Value::Integer(value) => serde_json::json!({"integer": value}),
        query_parameter::Value::Real(value) => serde_json::json!({"real": value}),
        query_parameter::Value::Text(value) => serde_json::json!({"text": value}),
        query_parameter::Value::Blob(value) => serde_json::json!({"blob": value}),
        query_parameter::Value::Boolean(value) => serde_json::json!({"boolean": value}),
        query_parameter::Value::Null(value) => serde_json::json!({"null": value}),
    }
}

#[tonic::async_trait]
impl ResolveService for Resolver {
    async fn resolve(
        &self,
        request: Request<ResolveRequest>,
    ) -> Result<Response<ResolveResponse>, Status> {
        let req = request.into_inner();
        let (trace, episode_id) = self.trace_for(&req, "resolve")?;
        tracing::debug!(
            query = %req.query,
            database = %req.database,
            mentions = trace.mentions.len(),
            elapsed_ms = trace.elapsed_ms,
            "resolve"
        );
        let mut response = stemma_resolve::trace_to_proto(&trace);
        response.episode_id = episode_id;
        Ok(Response::new(response))
    }

    async fn explain(
        &self,
        request: Request<ResolveRequest>,
    ) -> Result<Response<ExplainResponse>, Status> {
        let req = request.into_inner();
        let (trace, episode_id) = self.trace_for(&req, "explain")?;
        let mut response = stemma_resolve::trace_to_explain_proto(&trace);
        response.episode_id = episode_id;
        Ok(Response::new(response))
    }

    async fn parse(
        &self,
        request: Request<ParseRequest>,
    ) -> Result<Response<ParseResponse>, Status> {
        let request = request.into_inner();
        let resolve_request = ResolveRequest {
            query: request.query,
            database: request.database,
            options: request.options,
        };
        let (trace, episode_id) = self.trace_for(&resolve_request, "parse")?;
        let mut resolution = stemma_resolve::trace_to_proto(&trace);
        resolution.episode_id = episode_id.clone();
        let status = match trace.outcome().status {
            stemma_resolve::ResolutionStatus::Ambiguous => ParseStatus::Ambiguous,
            stemma_resolve::ResolutionStatus::Unknown => ParseStatus::Unknown,
            stemma_resolve::ResolutionStatus::Unanswerable => ParseStatus::Unanswerable,
            _ => ParseStatus::Resolved,
        };
        let mut response = ParseResponse {
            status: status.into(),
            resolution: Some(resolution),
            ..Default::default()
        };
        if status == ParseStatus::Resolved {
            if let Some(lm) = self.lm.as_deref() {
                let db = self
                    .database(&resolve_request.database)?
                    .lock()
                    .expect("stemmadb lock poisoned");
                match stemma_parse::parse(&trace, &db, lm) {
                    Ok(result) if result.proposals.len() == 1 => {
                        let proposal = &result.proposals[0];
                        response.sql = proposal.sql.clone();
                        response.parameters = proposal
                            .parameters
                            .iter()
                            .enumerate()
                            .map(|(index, value)| QueryParameter {
                                position: index as u32 + 1,
                                value: Some(parameter_value(value)),
                            })
                            .collect();
                        response.grounding_uses =
                            proposal.grounding.iter().map(grounding_use).collect();
                        response.assumptions = proposal.assumptions.clone();
                    }
                    Ok(_) => {
                        response.status = ParseStatus::InvalidProposal.into();
                        response.validation_failures.push(ValidationFailure {
                            code: "multiple_valid_proposals".into(),
                            message: "proposal service returned multiple valid queries".into(),
                            location: String::new(),
                        });
                    }
                    Err(error @ stemma_parse::ParseError::Backend(_)) => {
                        response.status = ParseStatus::ProposalUnavailable.into();
                        response.validation_failures.push(validation_failure(error));
                    }
                    Err(error) => {
                        response.status = ParseStatus::InvalidProposal.into();
                        response.validation_failures.push(validation_failure(error));
                    }
                }
            } else {
                response.status = ParseStatus::ProposalUnavailable.into();
            }
        }
        self.record_parse_output(&resolve_request.database, &episode_id, &response);
        Ok(Response::new(response))
    }

    async fn submit_feedback(
        &self,
        request: Request<FeedbackRequest>,
    ) -> Result<Response<Feedback>, Status> {
        let request = request.into_inner();
        let category = FeedbackCategory::try_from(request.category)
            .map_err(|_| Status::invalid_argument("unknown feedback category"))?;
        let scope = FeedbackScope::try_from(request.scope)
            .map_err(|_| Status::invalid_argument("unknown feedback scope"))?;
        if category == FeedbackCategory::Unspecified || scope == FeedbackScope::Unspecified {
            return Err(Status::invalid_argument(
                "feedback category and scope are required",
            ));
        }
        if request.correction.len() > 4096 {
            return Err(Status::invalid_argument(
                "feedback correction exceeds 4096 bytes",
            ));
        }
        if request.candidate_index.is_some() && request.clarification_option.is_some() {
            return Err(Status::invalid_argument(
                "candidate and clarification option are mutually exclusive",
            ));
        }
        if category == FeedbackCategory::MissingInterpretation
            && request.correction.trim().is_empty()
        {
            return Err(Status::invalid_argument(
                "a missing interpretation requires a correction",
            ));
        }

        let db = self
            .database(&request.database)?
            .lock()
            .expect("stemmadb lock poisoned");
        let episode = load_episode(&db, &request.episode_id)?;
        let current =
            evidence_revisions(&db).map_err(|error| Status::internal(error.to_string()))?;
        if (
            episode.database_revision.as_str(),
            episode.vector_revision.as_str(),
        ) != (current.0.as_str(), current.1.as_str())
        {
            return Err(Status::failed_precondition(
                "the episode evidence revision is no longer active",
            ));
        }
        if scope == FeedbackScope::Session && episode.session.is_empty() {
            return Err(Status::invalid_argument(
                "session-scoped feedback requires an episode with a session",
            ));
        }
        if matches!(
            category,
            FeedbackCategory::WrongQueryOperation | FeedbackCategory::WrongRows
        ) && (episode.kind != "parse" || !episode.parse_succeeded)
        {
            return Err(Status::invalid_argument(
                "query-operation and returned-row feedback require a successful parse episode",
            ));
        }
        validate_feedback_target(&request, &episode.evidence)?;

        db.conn()
            .execute(
                "INSERT INTO grounding_feedback
                     (query_id, category, scope, span_id, candidate_index,
                      clarification_option, correction)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                stemmadb::rusqlite::params![
                    episode.query_id,
                    category.as_str_name(),
                    scope.as_str_name(),
                    request.span_id,
                    request.candidate_index,
                    request.clarification_option,
                    request.correction.trim(),
                ],
            )
            .map_err(|error| Status::internal(error.to_string()))?;
        let id = db.conn().last_insert_rowid() as u64;
        Ok(Response::new(load_feedback(&db, id)?))
    }

    async fn list_feedback(
        &self,
        request: Request<ListFeedbackRequest>,
    ) -> Result<Response<FeedbackList>, Status> {
        let request = request.into_inner();
        let db = self
            .database(&request.database)?
            .lock()
            .expect("stemmadb lock poisoned");
        let mut statement = db
            .conn()
            .prepare(
                "SELECT f.id, q.episode_id, f.category, f.scope, f.span_id,
                        f.candidate_index, f.clarification_option, f.correction,
                        f.recorded_at
                 FROM grounding_feedback f JOIN query_log q ON q.id = f.query_id
                 WHERE (?1 IS NULL OR q.episode_id = ?1) ORDER BY f.id",
            )
            .map_err(|error| Status::internal(error.to_string()))?;
        let feedback = statement
            .query_map([request.episode_id], feedback_from_row)
            .map_err(|error| Status::internal(error.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| Status::internal(error.to_string()))?;
        Ok(Response::new(FeedbackList { feedback }))
    }

    async fn delete_feedback(
        &self,
        request: Request<DeleteFeedbackRequest>,
    ) -> Result<Response<DeleteFeedbackResponse>, Status> {
        let request = request.into_inner();
        let db = self
            .database(&request.database)?
            .lock()
            .expect("stemmadb lock poisoned");
        let feedback_id = i64::try_from(request.feedback_id)
            .map_err(|_| Status::invalid_argument("feedback id is out of range"))?;
        let deleted = db
            .conn()
            .execute(
                "DELETE FROM grounding_feedback WHERE id = ?1",
                [feedback_id],
            )
            .map_err(|error| Status::internal(error.to_string()))?
            != 0;
        Ok(Response::new(DeleteFeedbackResponse { deleted }))
    }
}

struct StoredEpisode {
    query_id: i64,
    kind: String,
    session: String,
    database_revision: String,
    vector_revision: String,
    parse_succeeded: bool,
    evidence: EpisodeEvidence,
}

fn load_episode(db: &StemmaDb, episode_id: &str) -> Result<StoredEpisode, Status> {
    if episode_id.is_empty() {
        return Err(Status::invalid_argument("episode id is required"));
    }
    let row = db.conn().query_row(
        "SELECT id, episode_kind, session, database_revision, vector_revision,
                evidence_json, json_extract(parse_json, '$.status')
         FROM query_log WHERE episode_id = ?1",
        [episode_id],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        },
    );
    let (query_id, kind, session, database_revision, vector_revision, evidence, parse_status) =
        match row {
            Ok(row) => row,
            Err(stemmadb::rusqlite::Error::QueryReturnedNoRows) => {
                return Err(Status::not_found("unknown resolution episode"));
            }
            Err(error) => return Err(Status::internal(error.to_string())),
        };
    Ok(StoredEpisode {
        query_id,
        kind,
        session,
        database_revision,
        vector_revision,
        parse_succeeded: parse_status.as_deref() == Some("PARSE_STATUS_RESOLVED"),
        evidence: serde_json::from_str(&evidence)
            .map_err(|error| Status::internal(format!("invalid episode evidence: {error}")))?,
    })
}

fn validate_feedback_target(
    request: &FeedbackRequest,
    evidence: &EpisodeEvidence,
) -> Result<(), Status> {
    if (request.candidate_index.is_some() || request.clarification_option.is_some())
        && request.span_id.is_none()
    {
        return Err(Status::invalid_argument(
            "candidate and clarification targets require a span",
        ));
    }
    let span = request
        .span_id
        .map(|id| {
            evidence
                .spans
                .iter()
                .find(|span| span.id == id)
                .ok_or_else(|| Status::invalid_argument("span is absent from the episode"))
        })
        .transpose()?;
    if let Some(index) = request.candidate_index {
        if span
            .and_then(|span| span.candidates.get(index as usize))
            .is_none()
        {
            return Err(Status::invalid_argument(
                "candidate is absent from the recorded presentation order",
            ));
        }
    }
    if let Some(index) = request.clarification_option {
        let clarification = evidence
            .clarification
            .as_ref()
            .filter(|clarification| Some(clarification.span_id) == request.span_id)
            .ok_or_else(|| {
                Status::invalid_argument("the episode has no clarification for this span")
            })?;
        if clarification.options.get(index as usize).is_none() {
            return Err(Status::invalid_argument(
                "clarification option is absent from the recorded presentation order",
            ));
        }
    }
    Ok(())
}

fn feedback_from_row(row: &stemmadb::rusqlite::Row<'_>) -> stemmadb::rusqlite::Result<Feedback> {
    let category: String = row.get(2)?;
    let scope: String = row.get(3)?;
    Ok(Feedback {
        id: row.get::<_, i64>(0)? as u64,
        episode_id: row.get(1)?,
        category: FeedbackCategory::from_str_name(&category)
            .unwrap_or(FeedbackCategory::Unspecified)
            .into(),
        scope: FeedbackScope::from_str_name(&scope)
            .unwrap_or(FeedbackScope::Unspecified)
            .into(),
        span_id: row.get(4)?,
        candidate_index: row.get(5)?,
        clarification_option: row.get(6)?,
        correction: row.get(7)?,
        recorded_at: row.get(8)?,
    })
}

fn load_feedback(db: &StemmaDb, id: u64) -> Result<Feedback, Status> {
    let id = i64::try_from(id).map_err(|_| Status::internal("feedback id is out of range"))?;
    db.conn()
        .query_row(
            "SELECT f.id, q.episode_id, f.category, f.scope, f.span_id,
                    f.candidate_index, f.clarification_option, f.correction,
                    f.recorded_at
             FROM grounding_feedback f JOIN query_log q ON q.id = f.query_id
             WHERE f.id = ?1",
            [id],
            feedback_from_row,
        )
        .map_err(|error| Status::internal(error.to_string()))
}

fn parameter_value(value: &stemma_parse::ParameterValue) -> query_parameter::Value {
    use stemma_parse::ParameterValue::*;
    match value {
        Null => query_parameter::Value::Null(true),
        Integer(value) => query_parameter::Value::Integer(*value),
        Real(value) => query_parameter::Value::Real(*value),
        Text(value) => query_parameter::Value::Text(value.clone()),
    }
}

fn grounding_use(value: &stemma_parse::GroundingUse) -> GroundingUse {
    let kind = match value.kind {
        stemma_parse::GroundingKind::Table => "table",
        stemma_parse::GroundingKind::Column => "column",
        stemma_parse::GroundingKind::Parameter => "parameter",
        stemma_parse::GroundingKind::Join => "join",
    };
    GroundingUse {
        kind: kind.into(),
        name: value.name.clone(),
        span_id: value.span_id.map(|id| id as u32),
        candidate_index: value.candidate_index.map(|id| id as u32),
    }
}

fn validation_failure(error: stemma_parse::ParseError) -> ValidationFailure {
    let code = match &error {
        stemma_parse::ParseError::Backend(_) => "proposal_unavailable",
        stemma_parse::ParseError::Response(_) => "malformed_response",
        stemma_parse::ParseError::Invalid(_) => "invalid_proposal",
        stemma_parse::ParseError::Schema(_) => "schema_inspection",
    };
    ValidationFailure {
        code: code.into(),
        message: error.to_string(),
        location: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence_fixture() -> EpisodeEvidence {
        EpisodeEvidence {
            mentions: vec![3],
            spans: vec![EpisodeSpan {
                id: 3,
                candidates: vec![
                    EpisodeCandidate {
                        table: "people".into(),
                        column: "name".into(),
                        rowid: 1,
                        selected: true,
                    },
                    EpisodeCandidate {
                        table: "teams".into(),
                        column: "name".into(),
                        rowid: 2,
                        selected: false,
                    },
                ],
            }],
            clarification: Some(EpisodeClarification {
                span_id: 3,
                options: vec![vec![0], vec![1]],
            }),
        }
    }

    fn feedback_request(episode_id: &str) -> FeedbackRequest {
        FeedbackRequest {
            database: "test".into(),
            episode_id: episode_id.into(),
            category: FeedbackCategory::Approved.into(),
            scope: FeedbackScope::Database.into(),
            span_id: Some(3),
            candidate_index: Some(1),
            clarification_option: None,
            correction: String::new(),
        }
    }

    #[test]
    fn parse_values_project_without_string_coercion() {
        use stemma_parse::ParameterValue::*;
        assert_eq!(parameter_value(&Null), query_parameter::Value::Null(true));
        assert_eq!(
            parameter_value(&Integer(7)),
            query_parameter::Value::Integer(7)
        );
        assert_eq!(
            parameter_value(&Real(1.5)),
            query_parameter::Value::Real(1.5)
        );
        assert_eq!(
            parameter_value(&Text("x".into())),
            query_parameter::Value::Text("x".into())
        );
        let response = ParseResponse {
            parameters: vec![QueryParameter {
                position: 1,
                value: Some(query_parameter::Value::Integer(7)),
            }],
            ..Default::default()
        };
        let output: serde_json::Value =
            serde_json::from_str(&parse_output_json(&response)).unwrap();
        assert_eq!(output["parameters"][0]["value"]["integer"], 7);
    }

    #[test]
    fn absent_grounding_reference_stays_absent() {
        let projected = grounding_use(&stemma_parse::GroundingUse {
            kind: stemma_parse::GroundingKind::Column,
            name: "total".into(),
            span_id: None,
            candidate_index: None,
        });
        assert_eq!((projected.span_id, projected.candidate_index), (None, None));
    }

    #[test]
    fn feedback_targets_recorded_candidate_and_option_order() {
        let evidence = evidence_fixture();
        assert!(validate_feedback_target(&feedback_request("episode"), &evidence).is_ok());

        let mut missing = feedback_request("episode");
        missing.candidate_index = Some(2);
        assert_eq!(
            validate_feedback_target(&missing, &evidence)
                .unwrap_err()
                .code(),
            tonic::Code::InvalidArgument
        );

        let mut option = feedback_request("episode");
        option.candidate_index = None;
        option.clarification_option = Some(1);
        assert!(validate_feedback_target(&option, &evidence).is_ok());
        option.span_id = Some(4);
        assert_eq!(
            validate_feedback_target(&option, &evidence)
                .unwrap_err()
                .code(),
            tonic::Code::InvalidArgument
        );
    }

    #[tokio::test]
    async fn feedback_round_trip_delete_and_revision_invalidation() {
        let db = StemmaDb::open_in_memory().unwrap();
        let revisions = evidence_revisions(&db).unwrap();
        db.conn()
            .execute(
                "INSERT INTO query_log
                     (query, mentions, elapsed_ms, episode_id, episode_kind,
                      database_revision, vector_revision, evidence_json)
                 VALUES ('who', 1, 1.0, 'episode', 'resolve', ?1, ?2, ?3)",
                stemmadb::rusqlite::params![
                    revisions.0,
                    revisions.1,
                    serde_json::to_string(&evidence_fixture()).unwrap(),
                ],
            )
            .unwrap();
        let resolver = Resolver {
            dbs: HashMap::from([("test".into(), Mutex::new(db))]),
            dense_searches: HashMap::from([(
                "test".into(),
                std::sync::Arc::new(stemma_resolve::DenseSearch::exact()),
            )]),
            embedder: None,
            lm: None,
        };

        let stored = resolver
            .submit_feedback(Request::new(feedback_request("episode")))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(stored.candidate_index, Some(1));
        let listed = resolver
            .list_feedback(Request::new(ListFeedbackRequest {
                database: "test".into(),
                episode_id: Some("episode".into()),
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(listed.feedback.len(), 1);
        assert!(
            resolver
                .delete_feedback(Request::new(DeleteFeedbackRequest {
                    database: "test".into(),
                    feedback_id: stored.id,
                }))
                .await
                .unwrap()
                .into_inner()
                .deleted
        );

        let db = resolver.dbs["test"].lock().unwrap();
        db.conn()
            .execute(
                "UPDATE query_log SET episode_kind = 'parse' WHERE episode_id = 'episode'",
                [],
            )
            .unwrap();
        drop(db);
        let mut wrong_rows = feedback_request("episode");
        wrong_rows.category = FeedbackCategory::WrongRows.into();
        let error = resolver
            .submit_feedback(Request::new(wrong_rows))
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);

        let db = resolver.dbs["test"].lock().unwrap();
        db.conn()
            .execute(
                "INSERT INTO model_registry (vector_table, backend, model, dimension)
                 VALUES ('vec_test', 'test', 'changed', 2)",
                [],
            )
            .unwrap();
        drop(db);
        let error = resolver
            .submit_feedback(Request::new(feedback_request("episode")))
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    }
}

/// Cadence of the cheap change probe: every this-many seconds the
/// background task reads `PRAGMA src.data_version` — a counter SQLite bumps
/// when *another* connection commits to the user database — and only a
/// changed counter triggers the (fingerprint-guarded) refresh. An
/// operational polling interval, not a data-derived quantity: it trades
/// staleness for probe cost, and the probe is one pragma read.
const REFRESH_POLL_SECS: u64 = 60;

/// The steady-state background task for one database: run the embed pass
/// once at startup (when an embedder is configured), then watch the user
/// database for change. `PRAGMA src.data_version` is the cheap global
/// signal; when it moves, the registration path re-runs — the lexical
/// index's receipts re-ingest exactly the changed tables, the knowledge
/// compiler's fingerprints recompile exactly the changed tables, and the
/// embed queue's content hashes re-embed exactly the changed rows. No
/// filesystem watcher, no restart.
fn background_task(
    name: &str,
    user_db: &std::path::Path,
    embedder: Option<std::sync::Arc<stemma_embed::CooldownEmbedder<stemma_embed::OpenAiEmbedder>>>,
    dense_search: std::sync::Arc<stemma_resolve::DenseSearch>,
) {
    let store = user_db.with_extension("stemmadb");
    let db = match StemmaDb::open(&store, user_db) {
        Ok(db) => db,
        Err(e) => {
            tracing::warn!(name, error = %e, "background task: opening store failed");
            return;
        }
    };
    if let Some(embedder) = &embedder {
        embed_pass(name, &db, embedder.as_ref());
        rebuild_dense_sidecar(name, &db, &dense_search);
    }

    let data_version = |db: &StemmaDb| -> i64 {
        db.conn()
            .query_row("PRAGMA src.data_version", [], |r| r.get(0))
            .unwrap_or(0)
    };
    let mut last = data_version(&db);
    loop {
        std::thread::sleep(std::time::Duration::from_secs(REFRESH_POLL_SECS));
        if let Some(embedder) = &embedder {
            // Resume from the work state, never from an observed down→up
            // transition: the cooldown marker is shared across databases, so
            // whichever task probes first consumes the transition and every
            // other task would sleep forever on a non-empty queue. The queue
            // is the source of truth — usable embedder + pending items means
            // drain, regardless of who cleared the marker.
            let usable = !embedder.is_down() || embedder.probe().is_ok();
            if usable && pending_embed_work(&db) {
                tracing::info!(
                    name,
                    "embed queue pending and endpoint usable; resuming drain"
                );
                embed_pass(name, &db, embedder.as_ref());
                rebuild_dense_sidecar(name, &db, &dense_search);
            }
        }
        let seen = data_version(&db);
        if seen == last {
            continue;
        }
        last = seen;
        tracing::info!(name, "user database changed; refreshing derived state");
        match stemma_ingest::build_lexical_index(&db, false) {
            Ok(stats) => tracing::info!(
                name,
                reingested_tables = stats.reingested_tables,
                values = stats.values,
                "refresh: lexical index"
            ),
            Err(e) => {
                tracing::warn!(name, error = %e, "refresh: lexical index failed");
                continue;
            }
        }
        match stemma_kg::compile(&db, false) {
            Ok(kg) => tracing::info!(
                name,
                recompiled_tables = kg.recompiled_tables,
                "refresh: knowledge graph"
            ),
            Err(e) => tracing::warn!(name, error = %e, "refresh: kg compile failed"),
        }
        if let Some(embedder) = &embedder {
            embed_pass(name, &db, embedder.as_ref());
            rebuild_dense_sidecar(name, &db, &dense_search);
        }
    }
}

fn rebuild_dense_sidecar(name: &str, db: &StemmaDb, search: &stemma_resolve::DenseSearch) {
    if let Err(error) = search.rebuild(db) {
        tracing::warn!(name, %error, "dense sidecar rebuild failed; exact search remains active");
    }
}

/// Whether the embed queue holds pending items — the cheap steady-state
/// check the background loop gates on. EXISTS under the (status, attempts,
/// id) index is constant-time, so polling it every cycle costs nothing when
/// the queue is empty.
fn pending_embed_work(db: &StemmaDb) -> bool {
    db.conn()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM embed_queue WHERE status = 'pending')",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n != 0)
        .unwrap_or(false)
}

/// Enqueues embedding work for one database and drains it, documents
/// strictly before interpretation cards: enqueue docs → drain to empty →
/// enqueue interps → drain to empty. The document sweep is small (the
/// corpus's long cells) while the card sweep can be large, so this ordering
/// lights the dense document channel in seconds instead of after the full
/// interpretation pass. The drain routes each item by kind: documents into
/// vec_dense, cards into vec_interp.
fn embed_pass(name: &str, db: &StemmaDb, embedder: &dyn stemma_embed::Embedder) {
    // Column cards first: a schema's worth of embeddings, receipted so an
    // unchanged schema costs one fingerprint check — and the column-affinity
    // pass lights up before any queue drains.
    match stemma_ingest::build_column_cards(db, embedder) {
        Ok(s) if s.rebuilt => {
            tracing::info!(name, cards = s.cards, model = %s.model, "column cards rebuilt")
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(name, error = %e, "column card build failed; column affinity degraded")
        }
    }
    let start = std::time::Instant::now();
    let queued_docs = match stemma_ingest::enqueue_missing_embeddings(db) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(name, error = %e, "embed drain: document enqueue failed");
            return;
        }
    };
    tracing::info!(
        name,
        queued_docs,
        elapsed_ms = start.elapsed().as_millis() as u64,
        "embed drain: documents enqueued"
    );
    log_dense_building(name, db);
    if !drain_to_empty(name, db, embedder, "documents") {
        return;
    }
    // Fresh vectors move the corpus's dense geometry; re-derive (receipted,
    // so an unchanged index costs one fingerprint check).
    if let Err(e) = stemma_ingest::derive_dense_geometry(db) {
        tracing::warn!(name, error = %e, "dense geometry derivation failed");
    }

    let start = std::time::Instant::now();
    let queued_interps = match stemma_ingest::enqueue_missing_interpretations(db) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(name, error = %e, "embed drain: interpretation enqueue failed");
            return;
        }
    };
    tracing::info!(
        name,
        queued_interps,
        elapsed_ms = start.elapsed().as_millis() as u64,
        "embed drain: interpretations enqueued"
    );
    log_dense_building(name, db);
    drain_to_empty(name, db, embedder, "interpretations");
}

/// One line distinguishing "dense index still building" from "no
/// dense-channel candidates": while items are pending and either vector
/// table has yet to materialize, a dense query legitimately returns nothing.
fn log_dense_building(name: &str, db: &StemmaDb) {
    let count = |sql: &str| -> i64 { db.conn().query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
    let tables =
        count("SELECT count(*) FROM sqlite_master WHERE name IN ('vec_dense', 'vec_interp')");
    let docs =
        count("SELECT count(*) FROM embed_queue WHERE status = 'pending' AND serialized = ''");
    let interps =
        count("SELECT count(*) FROM embed_queue WHERE status = 'pending' AND serialized != ''");
    if tables < 2 && docs + interps > 0 {
        tracing::info!(
            name,
            "dense channel building: {docs} docs, {interps} interps pending"
        );
    }
}

/// Drains the queue until nothing is pending; false = stopped on error
/// (left-over items keep their attempt counts for the next server start).
fn drain_to_empty(
    name: &str,
    db: &StemmaDb,
    embedder: &dyn stemma_embed::Embedder,
    phase: &str,
) -> bool {
    loop {
        match stemma_ingest::drain_embed_queue(db, embedder, stemma_ingest::EMBED_BATCH) {
            Ok(stats) => {
                tracing::info!(
                    name,
                    phase,
                    drained = stats.drained,
                    failed = stats.failed,
                    remaining = stats.remaining,
                    "embed drain: batch"
                );
                if stats.remaining == 0 {
                    tracing::info!(name, phase, "embed drain: queue empty");
                    return true;
                }
            }
            Err(e) => {
                tracing::warn!(name, phase, error = %e, "embed drain: stopped");
                return false;
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let mut args = Args::parse();
    merge_config(&mut args)?;
    let listen = args
        .listen
        .unwrap_or_else(|| "127.0.0.1:50051".parse().unwrap());
    let dense_mode = args.dense_search.as_deref().unwrap_or("exact");
    if !matches!(dense_mode, "exact" | "usearch") {
        anyhow::bail!("dense search must be exact or usearch, got {dense_mode:?}");
    }

    let mut dbs = HashMap::new();
    let mut dense_searches = HashMap::new();
    // Query templates the served stores have on record, as (origin, template)
    // — the vector-space identity each registry stored at registration.
    // Rows predating the column store '' and constrain nothing.
    let mut registered_templates: Vec<(String, String)> = Vec::new();
    for (name, user_db) in &args.dbs {
        let store = user_db.with_extension("stemmadb");
        let db = StemmaDb::open(&store, user_db)
            .with_context(|| format!("opening {name} ({})", user_db.display()))?;
        let stats = stemma_ingest::build_lexical_index(&db, false)
            .with_context(|| format!("indexing {name}"))?;
        let kg = stemma_kg::compile(&db, false)
            .with_context(|| format!("compiling knowledge graph for {name}"))?;
        let dense = stemma_ingest::build_dense_index(&db)
            .with_context(|| format!("promoting dense index for {name}"))?;
        let dense_search = std::sync::Arc::new(if dense_mode == "usearch" {
            stemma_resolve::DenseSearch::usearch(store.with_extension("stemmadb.usearch"))?
        } else {
            stemma_resolve::DenseSearch::exact()
        });
        rebuild_dense_sidecar(name, &db, &dense_search);
        if let Some(d) = &dense {
            tracing::info!(
                name,
                vectors = d.vectors,
                dim = d.dimension,
                model = %d.model,
                query_template = %d.query_template,
                promoted = d.promoted,
                "dense index"
            );
        }
        for table in ["vec_dense", "vec_interp"] {
            let stored: Option<String> = db
                .conn()
                .query_row(
                    "SELECT query_template FROM model_registry WHERE vector_table = ?1",
                    [table],
                    |r| r.get(0),
                )
                .ok();
            if let Some(t) = stored.filter(|t| !t.is_empty()) {
                registered_templates.push((format!("{name}/{table}"), t));
            }
        }
        tracing::info!(
            name,
            user_db = %user_db.display(),
            store = %store.display(),
            vec = db.vec_version().unwrap_or_default(),
            values = stats.values,
            indexed_ms = stats.elapsed_ms as u64,
            rebuilt = stats.rebuilt,
            kg_nodes = kg.nodes,
            kg_edges = kg.edges,
            "database registered"
        );
        dbs.insert(name.clone(), Mutex::new(db));
        dense_searches.insert(name.clone(), dense_search);
    }

    // The query template is part of the model identity, and the registry is
    // its record: a template a served store registered its vectors under
    // wins; explicit config (flag over file) must agree with it — a
    // disagreement means the configured embedder would send one convention's
    // queries into another convention's space, the same corruption as mixing
    // models, so it refuses at startup; the model-family lookup is only the
    // fallback for stores that predate the column and configs that state
    // nothing.
    let embedder = match (&args.embed_endpoint, &args.embed_model) {
        (Some(ep), Some(m)) => {
            let mut registered: Option<&(String, String)> = None;
            for pair in &registered_templates {
                match registered {
                    Some((origin, t)) if !stemma_embed::query_templates_agree(t, &pair.1) => {
                        anyhow::bail!(
                            "served stores register conflicting query templates: \
                             {origin} stores {t:?}, {} stores {:?}; one embedder \
                             cannot query both conventions",
                            pair.0,
                            pair.1
                        );
                    }
                    Some(_) => {}
                    None => registered = Some(pair),
                }
            }
            let embed_template = stemma_embed::resolve_query_template(
                args.embed_query_template.as_deref(),
                registered.map(|(_, t)| t.as_str()),
                m,
            )
            .with_context(|| {
                format!(
                    "query template for {}",
                    registered.map(|(o, _)| o.as_str()).unwrap_or("embedder")
                )
            })?;
            tracing::info!(
                endpoint = %ep,
                model = %m,
                query_template = embed_template.as_deref().unwrap_or("(bare)"),
                "dense channel enabled"
            );
            Some(std::sync::Arc::new(stemma_embed::CooldownEmbedder::new(
                stemma_embed::OpenAiEmbedder::new(ep, m, embed_template),
                // Down-marker refresh window; the background task owns the
                // recovery probe, so user queries always short-circuit.
                std::time::Duration::from_secs(60),
            )))
        }
        _ => None,
    };

    let lm = match (&args.lm_endpoint, &args.lm_model) {
        (Some(ep), Some(m)) => {
            tracing::info!(endpoint = %ep, model = %m, "adjudication band enabled");
            Some(stemma_lm::backend_for(ep, m, args.lm_extra_body.clone()))
        }
        _ => None,
    };

    // Each database gets a background task on its own store connection: an
    // initial embed pass (when an embedder is configured), then the
    // data_version watch that keeps derived state fresh without a restart.
    // The store is WAL, so serving is never blocked; a background failure
    // degrades the dense channel or freshness, never the server.
    for (name, user_db) in &args.dbs {
        let (name, user_db, embedder, dense_search) = (
            name.clone(),
            user_db.clone(),
            embedder.clone(),
            dense_searches[name].clone(),
        );
        tokio::task::spawn_blocking(move || {
            background_task(&name, &user_db, embedder, dense_search)
        });
    }

    tracing::info!(listen = %listen, "stemma-server starting");
    tonic::transport::Server::builder()
        .add_service(ResolveServiceServer::new(Resolver {
            dbs,
            dense_searches,
            embedder,
            lm,
        }))
        .serve(listen)
        .await?;
    Ok(())
}
