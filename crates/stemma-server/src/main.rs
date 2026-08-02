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
use stemma_proto::v1::{ExplainResponse, ResolveRequest, ResolveResponse};
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
    embedder: Option<EmbedderSection>,
}

#[derive(serde::Deserialize)]
struct EmbedderSection {
    endpoint: String,
    model: String,
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
    embedder: Option<stemma_embed::OpenAiEmbedder>,
}

impl Resolver {
    fn trace_for(&self, req: &ResolveRequest) -> Result<stemma_resolve::Trace, Status> {
        let db = self
            .dbs
            .get(&req.database)
            .ok_or_else(|| Status::not_found(format!("unknown database {:?}", req.database)))?;
        let db = db.lock().expect("stemmadb lock poisoned");
        let embedder = self
            .embedder
            .as_ref()
            .map(|e| e as &dyn stemma_embed::Embedder);
        let trace = stemma_resolve::resolve(&db, &req.query, embedder).map_err(|e| match e {
            stemma_resolve::Error::IndexMissing => Status::failed_precondition(e.to_string()),
            other => Status::internal(other.to_string()),
        })?;
        // Query history is store working memory; a failed write must never
        // fail the resolution.
        if !req.query.trim().is_empty() {
            let (source, session) = req
                .options
                .as_ref()
                .map(|o| (o.source.clone(), o.session.clone()))
                .unwrap_or_default();
            let _ = db.conn().execute(
                "INSERT INTO query_log (query, mentions, elapsed_ms, source, session)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                stemmadb::rusqlite::params![
                    req.query,
                    trace.mentions.len() as i64,
                    trace.elapsed_ms,
                    source,
                    session
                ],
            );
        }
        Ok(trace)
    }
}

#[tonic::async_trait]
impl ResolveService for Resolver {
    async fn resolve(
        &self,
        request: Request<ResolveRequest>,
    ) -> Result<Response<ResolveResponse>, Status> {
        let req = request.into_inner();
        let trace = self.trace_for(&req)?;
        tracing::debug!(
            query = %req.query,
            database = %req.database,
            mentions = trace.mentions.len(),
            elapsed_ms = trace.elapsed_ms,
            "resolve"
        );
        Ok(Response::new(stemma_resolve::trace_to_proto(&trace)))
    }

    async fn explain(
        &self,
        request: Request<ResolveRequest>,
    ) -> Result<Response<ExplainResponse>, Status> {
        let req = request.into_inner();
        let trace = self.trace_for(&req)?;
        Ok(Response::new(stemma_resolve::trace_to_explain_proto(
            &trace,
        )))
    }
}

/// Enqueues missing document embeddings for one database and drains the
/// queue to empty, batch by batch, on its own store connection.
fn drain_task(name: &str, user_db: &std::path::Path, endpoint: &str, model: &str) {
    let store = user_db.with_extension("stemmadb");
    let db = match StemmaDb::open(&store, user_db) {
        Ok(db) => db,
        Err(e) => {
            tracing::warn!(name, error = %e, "embed drain: opening store failed");
            return;
        }
    };
    let embedder = stemma_embed::OpenAiEmbedder::new(endpoint, model);
    let queued = match stemma_ingest::enqueue_missing_embeddings(&db) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(name, error = %e, "embed drain: enqueue failed");
            return;
        }
    };
    tracing::info!(name, queued, "embed drain: queue filled");
    loop {
        match stemma_ingest::drain_embed_queue(&db, &embedder, stemma_ingest::EMBED_BATCH) {
            Ok(stats) => {
                tracing::info!(
                    name,
                    queued,
                    drained = stats.drained,
                    failed = stats.failed,
                    remaining = stats.remaining,
                    "embed drain: batch"
                );
                if stats.remaining == 0 {
                    tracing::info!(name, "embed drain: queue empty");
                    return;
                }
            }
            Err(e) => {
                // Left-over items stay pending with their attempt counts;
                // the next server start picks them back up.
                tracing::warn!(name, error = %e, "embed drain: stopped");
                return;
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

    let mut dbs = HashMap::new();
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
        if let Some(d) = &dense {
            tracing::info!(
                name,
                vectors = d.vectors,
                dim = d.dimension,
                model = %d.model,
                promoted = d.promoted,
                "dense index"
            );
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
    }

    let embedder = match (&args.embed_endpoint, &args.embed_model) {
        (Some(ep), Some(m)) => {
            tracing::info!(endpoint = %ep, model = %m, "dense channel enabled");
            Some(stemma_embed::OpenAiEmbedder::new(ep, m))
        }
        _ => None,
    };

    // Index-time embedding: with an embedder configured, each database gets a
    // background task that enqueues its unembedded documents and drains the
    // queue until empty, then exits — no polling loop. The store is WAL, so
    // the task opens its own connection and serving is never blocked; a
    // drain failure degrades the dense channel, never the server.
    if let (Some(ep), Some(model)) = (&args.embed_endpoint, &args.embed_model) {
        for (name, user_db) in &args.dbs {
            let (name, user_db) = (name.clone(), user_db.clone());
            let (ep, model) = (ep.clone(), model.clone());
            tokio::task::spawn_blocking(move || drain_task(&name, &user_db, &ep, &model));
        }
    }

    tracing::info!(listen = %listen, "stemma-server starting");
    tonic::transport::Server::builder()
        .add_service(ResolveServiceServer::new(Resolver { dbs, embedder }))
        .serve(listen)
        .await?;
    Ok(())
}
