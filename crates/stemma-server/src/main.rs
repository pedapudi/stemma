//! stemma-server: gRPC front door for the Resolve API.
//!
//! Milestone-1 skeleton: serves ResolveService over tonic against a set of
//! registered databases; the pipeline itself lands in later milestones, so
//! Resolve currently returns an empty resolution for any query.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Context;
use clap::Parser;
use stemma_proto::v1::resolve_service_server::{ResolveService, ResolveServiceServer};
use stemma_proto::v1::{ResolveRequest, ResolveResponse};
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

#[tonic::async_trait]
impl ResolveService for Resolver {
    async fn resolve(
        &self,
        request: Request<ResolveRequest>,
    ) -> Result<Response<ResolveResponse>, Status> {
        let req = request.into_inner();
        let db = self
            .dbs
            .get(&req.database)
            .ok_or_else(|| Status::not_found(format!("unknown database {:?}", req.database)))?;
        // Touch the store so a broken registration fails loudly now rather
        // than in milestone 2.
        {
            let db = db.lock().expect("stemmadb lock poisoned");
            db.src_tables()
                .map_err(|e| Status::internal(format!("stemmadb: {e}")))?;
        }
        tracing::debug!(query = %req.query, database = %req.database, "resolve (skeleton)");
        Ok(Response::new(ResolveResponse {
            mentions: vec![],
            rewritten_query: String::new(),
        }))
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
        tracing::info!(
            name,
            user_db = %user_db.display(),
            store = %store.display(),
            vec = db.vec_version().unwrap_or_default(),
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
