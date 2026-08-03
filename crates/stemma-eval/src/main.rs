//! stemma-eval: the evaluation harness (docs/design/06-evaluation.md,
//! docs/design/07-eval-harness.md).
//!
//! BIRD's leaderboard numbers are conditional on human-written "evidence"
//! (pre-solved entity/value linking). stemma's protocol runs in the
//! no-evidence setting and asks: how much of that linking can we
//! reconstruct? Ground truth is derived from the gold SQL, denotation-
//! verified against the database instance, never hand-labeled.
//!
//! Subcommands:
//! - `derive`  — raw targets + corpus stats from a BIRD question file;
//! - `dataset` — the harness input: verified targets, gold rowid sets,
//!   mechanical tier assignment, one JSONL per corpus;
//! - `run`     — the mechanism-ablation sweep: metrics per (tier ×
//!   ablation) cell, paired statistics, run JSON + self-contained HTML
//!   report;
//! - `grade`   — compare a run to the accepted baseline; nonzero exit and
//!   named failures on regression;
//! - `accept`  — distill a run into a new baseline (a reviewed change).
//!
//! Configuration comes from config.json (`--config`) and flags only —
//! never environment variables.

mod dataset;
mod derive;
mod grade;
mod metrics;
mod report;
mod runner;
mod stats;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "stemma-eval", about = "stemma evaluation harness")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Derive raw resolution targets from a BIRD-format question file.
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
    /// Build denotation-verified evaluation datasets (one JSONL per corpus).
    Dataset {
        /// Path to dev.json (BIRD format).
        #[arg(long)]
        questions: PathBuf,
        /// The dev_databases directory (contains <db_id>/<db_id>.sqlite).
        #[arg(long)]
        db_root: PathBuf,
        /// Output directory for the JSONL files (eval/datasets).
        #[arg(long)]
        out_dir: PathBuf,
        /// Restrict to these db_ids (repeatable). Empty = all.
        #[arg(long = "db-id")]
        db_ids: Vec<String>,
        /// Provenance tag recorded on every record.
        #[arg(long, default_value = "bird-dev-20240627")]
        source: String,
    },
    /// Run the mechanism-ablation sweep over one corpus dataset.
    Run {
        /// Path to config.json (databases, server endpoints, eval section).
        #[arg(long)]
        config: Option<PathBuf>,
        /// The corpus dataset (JSONL) to evaluate.
        #[arg(long)]
        dataset: PathBuf,
        /// User database path. Default: config databases[corpus], then
        /// <eval.bird_dir>/dev_databases/<corpus>/<corpus>.sqlite.
        #[arg(long)]
        db: Option<PathBuf>,
        /// Where evaluation stores live. Default: next to the user db.
        #[arg(long)]
        store_dir: Option<PathBuf>,
        /// Ablations to run, cumulative order (repeatable).
        /// Default: lex +dense +kg +adj.
        #[arg(long = "ablation")]
        ablations: Vec<String>,
        /// Where run artifacts (JSON + HTML) land. Default: eval.runs_dir,
        /// then eval/runs.
        #[arg(long)]
        out_dir: Option<PathBuf>,
        /// Baseline to grade against. Default: eval/baseline/<corpus>.json
        /// when present; otherwise the run is ungraded.
        #[arg(long)]
        baseline: Option<PathBuf>,
        /// Permutations / resamples for the paired statistics.
        #[arg(long, default_value_t = 10_000)]
        permutations: usize,
        /// Rebuild evaluation stores from scratch.
        #[arg(long)]
        fresh: bool,
        /// OpenAI-compatible /v1/embeddings base URL (overrides config).
        #[arg(long)]
        embed_endpoint: Option<String>,
        /// Model name for --embed-endpoint.
        #[arg(long)]
        embed_model: Option<String>,
        /// OpenAI-compatible /v1/chat/completions base URL (overrides config).
        #[arg(long)]
        lm_endpoint: Option<String>,
        /// Model name for --lm-endpoint.
        #[arg(long)]
        lm_model: Option<String>,
        /// Report template override (default: the embedded checked-in build).
        #[arg(long)]
        template: Option<PathBuf>,
    },
    /// Grade a run against the accepted baseline (exit 1 on failure).
    Grade {
        #[arg(long)]
        run: PathBuf,
        #[arg(long)]
        baseline: PathBuf,
        #[arg(long, default_value_t = 10_000)]
        permutations: usize,
    },
    /// Distill a run into a new accepted baseline (a reviewed change).
    Accept {
        #[arg(long)]
        run: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    match args.cmd {
        Cmd::Derive {
            questions,
            out,
            db_ids,
        } => derive::derive(questions, out, db_ids),
        Cmd::Dataset {
            questions,
            db_root,
            out_dir,
            db_ids,
            source,
        } => derive::dataset(questions, db_root, out_dir, db_ids, source),
        Cmd::Run {
            config,
            dataset,
            db,
            store_dir,
            ablations,
            out_dir,
            baseline,
            permutations,
            fresh,
            embed_endpoint,
            embed_model,
            lm_endpoint,
            lm_model,
            template,
        } => {
            runner::run(runner::RunArgs {
                config,
                dataset,
                db,
                store_dir,
                ablations,
                out_dir,
                baseline,
                permutations,
                fresh,
                embed_endpoint,
                embed_model,
                lm_endpoint,
                lm_model,
                template,
            })?;
            Ok(())
        }
        Cmd::Grade {
            run,
            baseline,
            permutations,
        } => {
            let passed = grade::grade(&run, &baseline, permutations)?;
            if !passed {
                std::process::exit(1);
            }
            Ok(())
        }
        Cmd::Accept { run, out } => runner::accept(&run, &out),
    }
}
