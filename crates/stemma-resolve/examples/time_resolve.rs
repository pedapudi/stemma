//! Times lexical resolution against an existing store, one line per query.
//!
//! Usage: time_resolve <store.stemmadb> <user.db> <questions.txt> [passes]
//!
//! Each pass resolves every question once; per-pass medians separate cold
//! (first pass: page cache and prepared statements empty) from warm. The
//! store is only read — resolution never writes.

use std::io::BufRead;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: time_resolve <store.stemmadb> <user.db> <questions.txt> [passes]");
        std::process::exit(2);
    }
    let passes: usize = args.get(4).map(|s| s.parse().expect("passes")).unwrap_or(2);
    let questions: Vec<String> =
        std::io::BufReader::new(std::fs::File::open(&args[3]).expect("questions file"))
            .lines()
            .map(|l| l.expect("read line"))
            .filter(|l| !l.trim().is_empty())
            .collect();

    let db = stemmadb::StemmaDb::open(
        std::path::Path::new(&args[1]),
        std::path::Path::new(&args[2]),
    )
    .expect("open store");

    for pass in 0..passes {
        let mut times: Vec<f64> = Vec::new();
        for q in &questions {
            let t0 = std::time::Instant::now();
            let trace = stemma_resolve::resolve_lexical(&db, q).expect("resolve");
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            times.push(ms);
            println!(
                "pass={pass} ms={ms:8.1} spans={:3} mentions={:2} q={q:.60}",
                trace.spans.len(),
                trace.mentions.len()
            );
        }
        let mut sorted = times.clone();
        sorted.sort_by(f64::total_cmp);
        let median = if sorted.len() % 2 == 1 {
            sorted[sorted.len() / 2]
        } else {
            (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
        };
        let total: f64 = times.iter().sum();
        println!(
            "pass={pass} median_ms={median:.1} total_ms={total:.1} n={}",
            times.len()
        );
    }
}
