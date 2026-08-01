//! Dumps the full resolution trace for a query as JSON.
//! Usage: cargo run -p stemma-resolve --example trace_dump -- <user.db> "<query>"

fn main() {
    let mut args = std::env::args().skip(1);
    let user = std::path::PathBuf::from(args.next().expect("user.db path"));
    let query = args.next().expect("query");
    let store = user.with_extension("stemmadb");
    let db = stemmadb::StemmaDb::open(&store, &user).expect("open");
    stemma_ingest::build_lexical_index(&db, false).expect("ingest");
    let trace = stemma_resolve::resolve_lexical(&db, &query).expect("resolve");
    println!("{}", serde_json::to_string_pretty(&trace).unwrap());
}
