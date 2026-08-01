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
    /// Address to listen on.
    #[arg(long, default_value = "127.0.0.1:50051")]
    listen: SocketAddr,

    /// Databases to serve, as name=path/to/user.db (repeatable). The sidecar
    /// store is created next to the user DB as <path>.stemmadb.
    #[arg(long = "db", value_parser = parse_db_spec)]
    dbs: Vec<(String, PathBuf)>,
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
}

impl Resolver {
    fn trace_for(&self, req: &ResolveRequest) -> Result<stemma_resolve::Trace, Status> {
        let db = self
            .dbs
            .get(&req.database)
            .ok_or_else(|| Status::not_found(format!("unknown database {:?}", req.database)))?;
        let db = db.lock().expect("stemmadb lock poisoned");
        stemma_resolve::resolve_lexical(&db, &req.query).map_err(|e| match e {
            stemma_resolve::Error::IndexMissing => Status::failed_precondition(e.to_string()),
            other => Status::internal(other.to_string()),
        })
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let mut dbs = HashMap::new();
    for (name, user_db) in &args.dbs {
        let store = user_db.with_extension("stemmadb");
        let db = StemmaDb::open(&store, user_db)
            .with_context(|| format!("opening {name} ({})", user_db.display()))?;
        let stats = stemma_ingest::build_lexical_index(&db, false)
            .with_context(|| format!("indexing {name}"))?;
        tracing::info!(
            name,
            user_db = %user_db.display(),
            store = %store.display(),
            vec = db.vec_version().unwrap_or_default(),
            values = stats.values,
            indexed_ms = stats.elapsed_ms as u64,
            rebuilt = stats.rebuilt,
            "database registered"
        );
        dbs.insert(name.clone(), Mutex::new(db));
    }

    tracing::info!(listen = %args.listen, "stemma-server starting");
    tonic::transport::Server::builder()
        .add_service(ResolveServiceServer::new(Resolver { dbs }))
        .serve(args.listen)
        .await?;
    Ok(())
}
