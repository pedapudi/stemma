//! The `run` subcommand: execute the mechanism-ablation sweep over one
//! corpus dataset, compute the 07 metric set per (tier × ablation) cell,
//! attach paired statistics, and emit a machine-readable run file plus the
//! self-contained HTML report.
//!
//! Ablations are honest flag combinations of the shipping pipeline — the
//! same axes stemma-server exposes (absent embedder = no dense channel;
//! uncompiled KG = no graph assists; absent LM = no adjudication band).
//! There are no eval-only code paths in stemma-resolve. One deviation from
//! the 07 table, stated where it is measured: `+kg` and `+coh` are not
//! separately gateable in the shipping pipeline (every KG assist keys on
//! the compiled graph's presence), so the sweep runs `+kg` as one column
//! carrying mention detection, term coherence AND collective
//! disambiguation, and `+coh` is rejected with an explanation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use stemmadb::StemmaDb;

use crate::dataset::{self, Target};
use crate::metrics::{self, CalibrationBucket, Cell, QueryOutcome};
use crate::stats;

/// One mechanism ablation: a flag combination of the shipping pipeline.
#[derive(Debug, Clone)]
pub struct Ablation {
    pub name: String,
    pub dense: bool,
    pub kg: bool,
    pub lm: bool,
}

pub fn ablation(name: &str) -> anyhow::Result<Ablation> {
    let (dense, kg, lm) = match name {
        "lex" => (false, false, false),
        "+dense" => (true, false, false),
        "+kg" => (true, true, false),
        "+coh" => anyhow::bail!(
            "+coh is not separately gateable: collective disambiguation, KG mention \
             detection and term coherence all key on the compiled graph's presence \
             (no eval-only code paths in stemma-resolve). Use +kg, which carries all \
             three; the deviation is documented in docs/design/07-eval-harness.md"
        ),
        "+adj" => (true, true, true),
        other => anyhow::bail!("unknown ablation {other:?} (expected lex, +dense, +kg, +adj)"),
    };
    Ok(Ablation {
        name: name.to_string(),
        dense,
        kg,
        lm,
    })
}

pub const DEFAULT_ABLATIONS: [&str; 4] = ["lex", "+dense", "+kg", "+adj"];

/// Which tiers each mechanism is *supposed* to move (07's containment rule).
/// `+adj` is exempt: it buys points on ties wherever they occur.
pub fn target_tiers(ablation: &str) -> Option<&'static [&'static str]> {
    match ablation {
        "+dense" => Some(&["L2"]),
        "+kg" | "+coh" => Some(&["L3", "L4"]),
        _ => None,
    }
}

// ---------------------------------------------------------------- config --

#[derive(Deserialize, Default)]
pub struct ConfigFile {
    #[serde(default)]
    pub databases: BTreeMap<String, PathBuf>,
    #[serde(default)]
    pub server: ServerSection,
    #[serde(default)]
    pub eval: EvalSection,
}

#[derive(Deserialize, Default)]
pub struct ServerSection {
    pub embedder: Option<EndpointSection>,
    pub lm: Option<EndpointSection>,
}

#[derive(Deserialize, Clone)]
pub struct EndpointSection {
    pub endpoint: String,
    pub model: String,
    /// Query-side template ("{query}" placeholder); embedder only, ignored
    /// for the LM. Absent, the default is looked up by model family
    /// (`stemma_embed::default_query_template`) — the same resolution the
    /// server applies, so an eval run embeds queries the way the deployment
    /// would.
    #[serde(default)]
    pub query_template: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct EvalSection {
    /// BIRD dev root (the directory containing dev.json and dev_databases/).
    pub bird_dir: Option<PathBuf>,
    /// Where run artifacts (JSON + HTML) land. Default: eval/runs.
    pub runs_dir: Option<PathBuf>,
}

/// Loads config.json, resolving relative paths against the file's directory.
pub fn load_config(path: Option<&Path>) -> anyhow::Result<ConfigFile> {
    let Some(path) = path else {
        return Ok(ConfigFile::default());
    };
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading config {}", path.display()))?;
    let mut cfg: ConfigFile = serde_json::from_str(&text)
        .with_context(|| format!("parsing config {}", path.display()))?;
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    for p in cfg.databases.values_mut() {
        if !p.is_absolute() {
            *p = base.join(&p);
        }
    }
    for p in [&mut cfg.eval.bird_dir, &mut cfg.eval.runs_dir]
        .into_iter()
        .flatten()
    {
        if !p.is_absolute() {
            *p = base.join(&p);
        }
    }
    Ok(cfg)
}

// -------------------------------------------------- metered backend seams --

/// Wraps the embedder to meter calls from the outside — instrumentation
/// lives in the harness, at the seam, never inside the pipeline.
pub struct MeteredEmbedder<E> {
    inner: E,
    pub calls: AtomicUsize,
    pub texts: AtomicUsize,
    pub total_ms: AtomicU64,
}

impl<E> MeteredEmbedder<E> {
    pub fn new(inner: E) -> Self {
        Self {
            inner,
            calls: AtomicUsize::new(0),
            texts: AtomicUsize::new(0),
            total_ms: AtomicU64::new(0),
        }
    }
}

impl<E: stemma_embed::Embedder> stemma_embed::Embedder for MeteredEmbedder<E> {
    fn embed(&self, texts: &[String]) -> stemma_embed::Result<Vec<Vec<f32>>> {
        let start = std::time::Instant::now();
        let out = self.inner.embed(texts);
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.texts.fetch_add(texts.len(), Ordering::Relaxed);
        self.total_ms
            .fetch_add(start.elapsed().as_millis() as u64, Ordering::Relaxed);
        out
    }
    fn identity(&self) -> stemma_embed::ModelIdentity {
        self.inner.identity()
    }
}

/// Same idea for the LM: measured round-trip time per adjudication call.
pub struct MeteredLm {
    inner: Box<dyn stemma_lm::LmBackend>,
    pub calls: AtomicUsize,
    pub total_ms: AtomicU64,
}

impl MeteredLm {
    pub fn new(inner: Box<dyn stemma_lm::LmBackend>) -> Self {
        Self {
            inner,
            calls: AtomicUsize::new(0),
            total_ms: AtomicU64::new(0),
        }
    }
}

impl stemma_lm::LmBackend for MeteredLm {
    fn chat(
        &self,
        messages: &[stemma_lm::ChatMessage],
        schema: Option<&serde_json::Value>,
    ) -> stemma_lm::Result<String> {
        let start = std::time::Instant::now();
        let out = self.inner.chat(messages, schema);
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.total_ms
            .fetch_add(start.elapsed().as_millis() as u64, Ordering::Relaxed);
        out
    }
    fn native_structured_output(&self) -> bool {
        self.inner.native_structured_output()
    }
    fn identity(&self) -> stemma_lm::LmIdentity {
        self.inner.identity()
    }
}

// -------------------------------------------------------- run file shapes --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delta {
    /// What the delta is against: "prev:<ablation>" or "baseline".
    pub vs: String,
    pub mean: f64,
    pub ci: [f64; 2],
    pub p: f64,
    pub n: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryBrief {
    pub id: String,
    pub question: String,
    pub r5: f64,
    pub rinf: f64,
    pub mrr: f64,
    pub grounded: bool,
    pub nil_outcome: bool,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CellReport {
    #[serde(flatten)]
    pub cell: Cell,
    pub delta_prev: Option<Delta>,
    pub delta_baseline: Option<Delta>,
    /// Per-query column-strict recall@5 — the paired-statistics input and
    /// the grading currency.
    pub per_query: BTreeMap<String, f64>,
    pub queries: Vec<QueryBrief>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NilReport {
    /// Correct absences / all absence outcomes (None when the run produced
    /// no absence outcomes).
    pub precision: Option<f64>,
    /// NIL queries with no confident wrong mention / all NIL queries.
    pub recall: Option<f64>,
    pub confident_wrong: Vec<ConfidentWrong>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfidentWrong {
    pub id: String,
    pub question: String,
    pub candidate: String,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendCost {
    pub embed_calls: usize,
    pub embed_texts: usize,
    pub embed_ms_total: f64,
    pub lm_calls: usize,
    pub lm_ms_mean: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TukeyPair {
    pub a: String,
    pub b: String,
    pub p: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Failure {
    pub check: String,
    pub cell: String,
    pub detail: String,
    #[serde(default)]
    pub queries: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunFile {
    pub run_id: String,
    pub corpus: String,
    pub dataset: String,
    pub git_rev: String,
    pub date: String,
    pub ablations: Vec<String>,
    pub tiers: Vec<String>,
    /// ablation → tier → cell.
    pub cells: BTreeMap<String, BTreeMap<String, CellReport>>,
    /// ablation → NIL behavior across the whole run.
    pub nil: BTreeMap<String, NilReport>,
    pub calibration: BTreeMap<String, Vec<CalibrationBucket>>,
    pub backend_cost: BTreeMap<String, BackendCost>,
    /// tier → familywise-adjusted pairwise p-values (>2 ablations only).
    pub tukey: BTreeMap<String, Vec<TukeyPair>>,
    /// Grading vs the accepted baseline (None: no baseline yet).
    pub pass: Option<bool>,
    pub failures: Vec<Failure>,
    pub notes: Vec<String>,
}

// -------------------------------------------------------------- store prep --

/// Prepares (or reuses) the evaluation store for one corpus variant. The
/// two variants differ only in whether the knowledge graph is compiled —
/// exactly the axis the server exposes.
pub fn prepare_store(
    user_db: &Path,
    store_dir: Option<&Path>,
    kg: bool,
    fresh: bool,
    notes: &mut Vec<String>,
) -> anyhow::Result<PathBuf> {
    let stem = user_db
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "corpus".into());
    let dir = store_dir
        .map(Path::to_path_buf)
        .or_else(|| user_db.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir).ok();
    let variant = if kg { "kg" } else { "nokg" };
    let store = dir.join(format!("{stem}.eval-{variant}.stemmadb"));
    if fresh && store.exists() {
        std::fs::remove_file(&store).ok();
        std::fs::remove_file(dir.join(format!("{stem}.eval-{variant}.stemmadb-wal"))).ok();
        std::fs::remove_file(dir.join(format!("{stem}.eval-{variant}.stemmadb-shm"))).ok();
    }
    let db = StemmaDb::open(&store, user_db)
        .with_context(|| format!("opening store {}", store.display()))?;
    let stats = stemma_ingest::build_lexical_index(&db, false)?;
    if stats.rebuilt {
        notes.push(format!(
            "indexed {variant} store: {} values in {} ms",
            stats.values, stats.elapsed_ms
        ));
    }
    if kg {
        let ks = stemma_kg::compile(&db, false)?;
        notes.push(format!(
            "kg ({} tables recompiled): {} nodes, {} edges",
            ks.recompiled_tables, ks.nodes, ks.edges
        ));
    }
    // Promote any externally staged vectors (BIRD has none; the legal corpus
    // loads pre-computed embeddings through vec_staging).
    if let Some(d) = stemma_ingest::build_dense_index(&db)? {
        if d.promoted {
            notes.push(format!(
                "dense index promoted: {} vectors ({}, dim {})",
                d.vectors, d.model, d.dimension
            ));
        }
    }
    Ok(store)
}

/// With an embedder configured, fill and drain the embed queue so the dense
/// channel serves document cells. Degrades with a note — an unreachable
/// embedder must not fail the lexical part of a run.
pub fn drain_embeddings(
    db: &StemmaDb,
    embedder: &dyn stemma_embed::Embedder,
    notes: &mut Vec<String>,
) {
    match stemma_ingest::enqueue_missing_embeddings(db) {
        Ok(0) => {}
        Ok(n) => notes.push(format!("embed queue: {n} document cells enqueued")),
        Err(e) => {
            notes.push(format!("embed enqueue failed: {e}"));
            return;
        }
    }
    loop {
        match stemma_ingest::drain_embed_queue(db, embedder, stemma_ingest::EMBED_BATCH) {
            Ok(s) if s.remaining == 0 => {
                if s.drained > 0 || s.failed > 0 {
                    notes.push(format!(
                        "embed drain: {} embedded, {} failed",
                        s.drained, s.failed
                    ));
                }
                return;
            }
            Ok(_) => {}
            Err(e) => {
                notes.push(format!("embed drain stopped: {e} (dense channel degraded)"));
                return;
            }
        }
    }
}

// ------------------------------------------------------------------- run --

pub struct RunArgs {
    pub config: Option<PathBuf>,
    pub dataset: PathBuf,
    pub db: Option<PathBuf>,
    pub store_dir: Option<PathBuf>,
    pub ablations: Vec<String>,
    pub out_dir: Option<PathBuf>,
    pub baseline: Option<PathBuf>,
    pub permutations: usize,
    pub fresh: bool,
    pub embed_endpoint: Option<String>,
    pub embed_model: Option<String>,
    pub lm_endpoint: Option<String>,
    pub lm_model: Option<String>,
    pub template: Option<PathBuf>,
}

pub fn run(args: RunArgs) -> anyhow::Result<RunFile> {
    let cfg = load_config(args.config.as_deref())?;
    let questions = dataset::load(&args.dataset)?;
    anyhow::ensure!(!questions.is_empty(), "dataset has no questions");
    let corpus = questions[0].corpus.clone();

    let user_db = resolve_db_path(&cfg, &corpus, args.db.as_deref())?;
    anyhow::ensure!(
        user_db.exists(),
        "user database not found: {}",
        user_db.display()
    );

    let embed_cfg = match (args.embed_endpoint, args.embed_model) {
        (Some(endpoint), Some(model)) => Some(EndpointSection {
            endpoint,
            model,
            query_template: None,
        }),
        _ => cfg.server.embedder.clone(),
    };
    let lm_cfg = match (args.lm_endpoint, args.lm_model) {
        (Some(endpoint), Some(model)) => Some(EndpointSection {
            endpoint,
            model,
            query_template: None,
        }),
        _ => cfg.server.lm.clone(),
    };

    let names = if args.ablations.is_empty() {
        DEFAULT_ABLATIONS.iter().map(|s| s.to_string()).collect()
    } else {
        args.ablations.clone()
    };
    let ablations: Vec<Ablation> = names
        .iter()
        .map(|n| ablation(n))
        .collect::<anyhow::Result<_>>()?;

    let mut notes = Vec::new();
    let mut run_file = RunFile {
        run_id: format!("{corpus}-{}", utc_stamp()),
        corpus: corpus.clone(),
        dataset: args.dataset.display().to_string(),
        git_rev: git_rev(),
        date: utc_stamp(),
        ablations: names.clone(),
        tiers: dataset::TIERS
            .iter()
            .filter(|t| questions.iter().any(|q| &q.tier == *t))
            .map(|s| s.to_string())
            .collect(),
        cells: BTreeMap::new(),
        nil: BTreeMap::new(),
        calibration: BTreeMap::new(),
        backend_cost: BTreeMap::new(),
        tukey: BTreeMap::new(),
        pass: None,
        failures: Vec::new(),
        notes: Vec::new(),
    };

    // Outcomes per ablation, in sweep order, for paired statistics.
    let mut all_outcomes: Vec<(String, Vec<QueryOutcome>)> = Vec::new();

    for ab in &ablations {
        let store = prepare_store(&user_db, args.store_dir.as_deref(), ab.kg, args.fresh, &mut notes)?;
        let db = StemmaDb::open(&store, &user_db)?;

        let embedder = match (&embed_cfg, ab.dense) {
            (Some(e), true) => Some(MeteredEmbedder::new(stemma_embed::OpenAiEmbedder::new(
                &e.endpoint,
                &e.model,
                e.query_template
                    .clone()
                    .or_else(|| stemma_embed::default_query_template(&e.model)),
            ))),
            (None, true) => {
                notes.push(format!(
                    "{}: no embedder configured; dense channel absent",
                    ab.name
                ));
                None
            }
            _ => None,
        };
        if let Some(e) = &embedder {
            drain_embeddings(&db, e, &mut notes);
        }
        let lm = match (&lm_cfg, ab.lm) {
            (Some(l), true) => Some(MeteredLm::new(stemma_lm::backend_for(&l.endpoint, &l.model))),
            (None, true) => {
                notes.push(format!("{}: no LM configured; adjudication absent", ab.name));
                None
            }
            _ => None,
        };

        let mut outcomes = Vec::with_capacity(questions.len());
        for q in &questions {
            let trace = stemma_resolve::resolve_full(
                &db,
                &q.question,
                embedder.as_ref().map(|e| e as &dyn stemma_embed::Embedder),
                lm.as_ref().map(|l| l as &dyn stemma_lm::LmBackend),
            )?;
            let mut probe = |t: &Target, rowid: i64| probe_gold(&db, t, rowid);
            let mut full_value = |table: &str, column: &str, rowid: i64| {
                db.conn()
                    .query_row(
                        "SELECT value FROM lex_values
                         WHERE src_table = ?1 AND src_column = ?2 AND src_rowid = ?3",
                        stemmadb::rusqlite::params![table, column, rowid],
                        |r| r.get::<_, String>(0),
                    )
                    .ok()
            };
            outcomes.push(metrics::score_query(q, &trace, &mut probe, &mut full_value));
        }

        run_file.backend_cost.insert(
            ab.name.clone(),
            BackendCost {
                embed_calls: embedder.as_ref().map_or(0, |e| e.calls.load(Ordering::Relaxed)),
                embed_texts: embedder.as_ref().map_or(0, |e| e.texts.load(Ordering::Relaxed)),
                embed_ms_total: embedder
                    .as_ref()
                    .map_or(0.0, |e| e.total_ms.load(Ordering::Relaxed) as f64),
                lm_calls: lm.as_ref().map_or(0, |l| l.calls.load(Ordering::Relaxed)),
                lm_ms_mean: lm.as_ref().map_or(0.0, |l| {
                    let calls = l.calls.load(Ordering::Relaxed);
                    if calls == 0 {
                        0.0
                    } else {
                        l.total_ms.load(Ordering::Relaxed) as f64 / calls as f64
                    }
                }),
            },
        );
        all_outcomes.push((ab.name.clone(), outcomes));
    }

    // ---- aggregate cells + paired deltas ----
    let baseline = load_baseline(args.baseline.as_deref(), &corpus)?;
    for (i, (name, outcomes)) in all_outcomes.iter().enumerate() {
        let mut tier_cells = BTreeMap::new();
        for tier in &run_file.tiers {
            let subset: Vec<&QueryOutcome> =
                outcomes.iter().filter(|o| &o.tier == tier).collect();
            if subset.is_empty() {
                continue;
            }
            let cell = metrics::aggregate(&subset);
            let per_query: BTreeMap<String, f64> = subset
                .iter()
                .map(|o| (o.id.clone(), cell_metric(o)))
                .collect();
            let delta_prev = (i > 0).then(|| {
                let (prev_name, prev) = &all_outcomes[i - 1];
                let prev_map: BTreeMap<&str, f64> = prev
                    .iter()
                    .filter(|o| &o.tier == tier)
                    .map(|o| (o.id.as_str(), cell_metric(o)))
                    .collect();
                let diffs: Vec<f64> = subset
                    .iter()
                    .filter_map(|o| prev_map.get(o.id.as_str()).map(|p| cell_metric(o) - p))
                    .collect();
                Delta {
                    vs: format!("prev:{prev_name}"),
                    mean: stats::mean(&diffs),
                    ci: pair_ci(&diffs, args.permutations),
                    p: stats::paired_randomization_p(&diffs, args.permutations),
                    n: diffs.len(),
                }
            });
            let delta_baseline = baseline.as_ref().and_then(|b| {
                let bcell = b.cells.get(name)?.get(tier)?;
                let diffs: Vec<f64> = subset
                    .iter()
                    .filter_map(|o| bcell.per_query.get(&o.id).map(|p| cell_metric(o) - p))
                    .collect();
                (!diffs.is_empty()).then(|| Delta {
                    vs: "baseline".into(),
                    mean: stats::mean(&diffs),
                    ci: pair_ci(&diffs, args.permutations),
                    p: stats::paired_randomization_p(&diffs, args.permutations),
                    n: diffs.len(),
                })
            });
            let queries: Vec<QueryBrief> = subset
                .iter()
                .map(|o| QueryBrief {
                    id: o.id.clone(),
                    question: o.question.clone(),
                    r5: o.r5_strict,
                    rinf: o.rinf_strict,
                    mrr: o.mrr,
                    grounded: o.grounded,
                    nil_outcome: o.nil_outcome,
                    note: diagnose(o),
                })
                .collect();
            tier_cells.insert(
                tier.clone(),
                CellReport {
                    cell,
                    delta_prev,
                    delta_baseline,
                    per_query,
                    queries,
                },
            );
        }
        run_file.cells.insert(name.clone(), tier_cells);

        // NIL behavior + calibration across the whole run for this ablation.
        run_file
            .nil
            .insert(name.clone(), nil_report(outcomes));
        let refs: Vec<&QueryOutcome> = outcomes.iter().collect();
        run_file
            .calibration
            .insert(name.clone(), metrics::calibration_curve(&refs));
    }

    // ---- randomised Tukey HSD when the sweep compares >2 variants ----
    if all_outcomes.len() > 2 {
        for tier in &run_file.tiers {
            let ids: Vec<&str> = all_outcomes[0]
                .1
                .iter()
                .filter(|o| &o.tier == tier)
                .map(|o| o.id.as_str())
                .collect();
            if ids.is_empty() {
                continue;
            }
            let maps: Vec<BTreeMap<&str, f64>> = all_outcomes
                .iter()
                .map(|(_, os)| {
                    os.iter()
                        .filter(|o| &o.tier == tier)
                        .map(|o| (o.id.as_str(), cell_metric(o)))
                        .collect()
                })
                .collect();
            let rows: Vec<Vec<f64>> = ids
                .iter()
                .map(|id| maps.iter().map(|m| *m.get(id).unwrap_or(&0.0)).collect())
                .collect();
            let ps = stats::randomized_tukey_hsd(&rows, all_outcomes.len(), args.permutations);
            run_file.tukey.insert(
                tier.clone(),
                ps.into_iter()
                    .map(|((i, j), p)| TukeyPair {
                        a: all_outcomes[i].0.clone(),
                        b: all_outcomes[j].0.clone(),
                        p,
                    })
                    .collect(),
            );
        }
    }

    // ---- grade against the baseline, if one exists ----
    if let Some(b) = &baseline {
        let failures = crate::grade::check(&run_file, b, args.permutations);
        run_file.pass = Some(failures.is_empty());
        run_file.failures = failures;
    }

    run_file.notes = notes;

    // ---- write artifacts ----
    let out_dir = args
        .out_dir
        .or(cfg.eval.runs_dir)
        .unwrap_or_else(|| PathBuf::from("eval/runs"));
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("creating {}", out_dir.display()))?;
    let json_path = out_dir.join(format!("{}.json", run_file.run_id));
    std::fs::write(&json_path, serde_json::to_string_pretty(&run_file)?)?;
    let html_path = out_dir.join(format!("{}.html", run_file.run_id));
    let html = crate::report::render(&run_file, args.template.as_deref())?;
    std::fs::write(&html_path, html)?;
    println!("run written: {}", json_path.display());
    println!("report:      {}", html_path.display());
    print_matrix(&run_file);
    Ok(run_file)
}

/// The graded per-query currency: column-strict recall@5 (07's matrix
/// metric). NIL queries grade on affirmed absence.
pub fn cell_metric(o: &QueryOutcome) -> f64 {
    if o.n_targets == 0 {
        if o.nil_outcome {
            1.0
        } else {
            0.0
        }
    } else {
        o.r5_strict
    }
}

fn pair_ci(diffs: &[f64], permutations: usize) -> [f64; 2] {
    let (lo, hi) = stats::bootstrap_ci95(diffs, permutations);
    [lo, hi]
}

fn diagnose(o: &QueryOutcome) -> String {
    if o.n_targets == 0 {
        return if o.nil_outcome {
            "absence affirmed".into()
        } else {
            "confident wrong: mention resolved on an absent answer".into()
        };
    }
    if o.grounded {
        return "grounded".into();
    }
    if o.rinf_strict == 0.0 && o.rinf_loose == 0.0 {
        "retrieval: no channel surfaced the gold row".into()
    } else if o.rinf_strict == 0.0 {
        "coincidence: value matched outside the gold column".into()
    } else if o.r5_strict == 0.0 {
        "threshold/TOP_K: gold row traced but outside top-5".into()
    } else if o.r1_strict == 0.0 {
        "ranking: gold row in top-5, not top-1".into()
    } else {
        "partial: some targets unlinked".into()
    }
}

fn nil_report(outcomes: &[QueryOutcome]) -> NilReport {
    let nil_queries: Vec<&QueryOutcome> = outcomes.iter().filter(|o| o.n_targets == 0).collect();
    let absence_outcomes: Vec<&QueryOutcome> =
        outcomes.iter().filter(|o| o.nil_outcome).collect();
    let correct_absences = absence_outcomes.iter().filter(|o| o.n_targets == 0).count();
    let precision = (!absence_outcomes.is_empty())
        .then(|| correct_absences as f64 / absence_outcomes.len() as f64);
    let recall = (!nil_queries.is_empty()).then(|| {
        nil_queries.iter().filter(|o| o.nil_outcome).count() as f64 / nil_queries.len() as f64
    });
    let confident_wrong = nil_queries
        .iter()
        .filter(|o| !o.nil_outcome)
        .map(|o| {
            let (candidate, score) = o
                .calibration
                .iter()
                .cloned()
                .max_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(s, _)| (String::new(), s))
                .unwrap_or_default();
            let candidate = o
                .targets
                .first()
                .and_then(|t| t.best_candidate.clone())
                .unwrap_or(candidate);
            ConfidentWrong {
                id: o.id.clone(),
                question: o.question.clone(),
                candidate,
                score,
            }
        })
        .collect();
    NilReport {
        precision,
        recall,
        confident_wrong,
    }
}

/// Gold-row membership probe for truncated rowid sets: re-run the gold
/// predicate for one rowid against the attached user database.
fn probe_gold(db: &StemmaDb, t: &Target, rowid: i64) -> bool {
    let op = match t.match_mode.as_str() {
        "like" => "LIKE",
        "doc" => return false, // doc sets are small; no predicate to re-run
        _ => "=",
    };
    let sql = format!(
        "SELECT 1 FROM {}.\"{}\" WHERE rowid = ?1 AND \"{}\" {} ?2 LIMIT 1",
        stemmadb::SRC_SCHEMA,
        t.table.replace('"', "\"\""),
        t.column.replace('"', "\"\""),
        op
    );
    db.conn()
        .prepare_cached(&sql)
        .and_then(|mut s| s.exists(stemmadb::rusqlite::params![rowid, t.literal]))
        .unwrap_or(false)
}

fn resolve_db_path(
    cfg: &ConfigFile,
    corpus: &str,
    flag: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    if let Some(p) = flag {
        return Ok(p.to_path_buf());
    }
    if let Some(p) = cfg.databases.get(corpus) {
        return Ok(p.clone());
    }
    if let Some(bird) = &cfg.eval.bird_dir {
        let p = bird
            .join("dev_databases")
            .join(corpus)
            .join(format!("{corpus}.sqlite"));
        if p.exists() {
            return Ok(p);
        }
    }
    anyhow::bail!(
        "cannot locate user database for corpus {corpus:?}: pass --db, or map it in \
         config databases, or set eval.bird_dir"
    )
}

fn git_rev() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

/// UTC timestamp (yyyymmdd-hhmmss) from the system clock — no clock crate;
/// the civil-from-days algorithm is standard.
pub fn utc_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn print_matrix(run: &RunFile) {
    println!("\nmechanism × tier matrix (column-strict recall@5):");
    print!("{:>8}", "");
    for tier in &run.tiers {
        print!("{tier:>10}");
    }
    println!();
    for ab in &run.ablations {
        print!("{ab:>8}");
        for tier in &run.tiers {
            match run.cells.get(ab).and_then(|m| m.get(tier)) {
                Some(c) => print!("{:>10.3}", c.cell.r5_strict),
                None => print!("{:>10}", "—"),
            }
        }
        println!();
    }
}

// ------------------------------------------------------ baseline loading --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineCell {
    pub r5_strict: f64,
    pub grounded: f64,
    pub n: usize,
    pub per_query: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Budgets {
    /// tier → p95 latency budget (ms).
    pub p95_latency_ms: BTreeMap<String, f64>,
    pub adjudication_rate_max: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baseline {
    pub corpus: String,
    pub dataset: String,
    pub run_id: String,
    pub git_rev: String,
    pub date: String,
    pub cells: BTreeMap<String, BTreeMap<String, BaselineCell>>,
    pub nil_precision: BTreeMap<String, Option<f64>>,
    pub budgets: Budgets,
}

pub fn load_baseline(path: Option<&Path>, corpus: &str) -> anyhow::Result<Option<Baseline>> {
    let default = PathBuf::from(format!("eval/baseline/{corpus}.json"));
    let path = match path {
        Some(p) => p.to_path_buf(),
        None if default.exists() => default,
        None => return Ok(None),
    };
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading baseline {}", path.display()))?;
    let b: Baseline = serde_json::from_str(&text)
        .with_context(|| format!("parsing baseline {}", path.display()))?;
    Ok(Some(b))
}

/// The `accept` subcommand: distill a run into the reviewed baseline.
/// Budgets are set from the measured run with headroom (p95 × 1.5,
/// adjudication rate + 0.10) — tightening them later is a reviewed edit.
pub fn accept(run_path: &Path, out: &Path) -> anyhow::Result<()> {
    let run: RunFile = serde_json::from_str(&std::fs::read_to_string(run_path)?)
        .with_context(|| format!("parsing run {}", run_path.display()))?;
    let mut cells: BTreeMap<String, BTreeMap<String, BaselineCell>> = BTreeMap::new();
    let mut p95: BTreeMap<String, f64> = BTreeMap::new();
    let mut adj_max = 0.0f64;
    for (ab, tiers) in &run.cells {
        for (tier, c) in tiers {
            cells.entry(ab.clone()).or_default().insert(
                tier.clone(),
                BaselineCell {
                    r5_strict: c.cell.r5_strict,
                    grounded: c.cell.grounded,
                    n: c.cell.n,
                    per_query: c.per_query.clone(),
                },
            );
            let e = p95.entry(tier.clone()).or_default();
            *e = e.max((c.cell.latency_p95_ms * 1.5).ceil());
            adj_max = adj_max.max(c.cell.adjudication_rate);
        }
    }
    let baseline = Baseline {
        corpus: run.corpus.clone(),
        dataset: run.dataset.clone(),
        run_id: run.run_id.clone(),
        git_rev: run.git_rev.clone(),
        date: run.date.clone(),
        cells,
        nil_precision: run
            .nil
            .iter()
            .map(|(k, v)| (k.clone(), v.precision))
            .collect(),
        budgets: Budgets {
            p95_latency_ms: p95,
            adjudication_rate_max: (adj_max + 0.10).min(1.0),
        },
    };
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(out, serde_json::to_string_pretty(&baseline)?)?;
    println!("baseline written: {}", out.display());
    Ok(())
}
