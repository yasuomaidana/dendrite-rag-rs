//! Retrieval evaluation: quality, tokens read and latency of the engine against baselines,
//! paired comparisons, and the repeat-question timing run.
//!
//!     loom-eval quality <root> eval/golden-dendrite.json [more sets…] [--index-dir DIR] [--json RUN.json]
//!     loom-eval compare <A.json> <B.json>
//!     loom-eval abstain-probe <root> <golden…> --negatives eval/golden-negatives.json --set NAME
//!     loom-eval export-chunks <root> --index-dir DIR --out chunks.jsonl
//!     loom-eval repeat <root> "<question>" --bin target/release/dendrite-rag [--corpus code] [--llm]

use std::path::PathBuf;
use std::time::Instant;

use clap::{Parser, Subcommand};
use loom_eval::abstain;
use loom_eval::baselines::Baselines;
use loom_eval::contextual;
use loom_eval::golden;
use loom_eval::methods::Method;
use loom_eval::metrics::{BUDGETS, Scores};
use loom_eval::repeat;
use loom_eval::report;
use loom_eval::runs;
use loom_rag::engine::Engine;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Score every method on golden sets.
    Quality {
        root: PathBuf,
        #[arg(required = true)]
        golden: Vec<PathBuf>,
        /// Index directory (default: the project's own).
        #[arg(long)]
        index_dir: Option<PathBuf>,
        /// Only methods whose label contains this text.
        #[arg(long)]
        only: Option<String>,
        /// Write raw per-query scores to a JSON file.
        #[arg(long)]
        json: Option<PathBuf>,
        /// Index chunks with these contexts (`{chunk_id: text}`); use a fresh index dir.
        #[arg(long)]
        contexts: Option<PathBuf>,
    },
    /// Write every indexed file with its chunks as JSON lines, for an external model to
    /// write contexts for (`--contexts`).
    ExportChunks {
        root: PathBuf,
        #[arg(long)]
        index_dir: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Calibrate `[rerank] abstain`: the best reranked hit's relevance on golden and
    /// unanswerable questions, and how thresholds split them.
    AbstainProbe {
        root: PathBuf,
        #[arg(required = true)]
        golden: Vec<PathBuf>,
        #[arg(long)]
        negatives: PathBuf,
        /// The negatives set for this project (a key of the file's `sets`).
        #[arg(long)]
        set: String,
        /// Index directory (default: the project's own).
        #[arg(long)]
        index_dir: Option<PathBuf>,
    },
    /// Compare two evaluation runs.
    Compare {
        /// Baseline run JSON file (A).
        a: PathBuf,
        /// Candidate run JSON file (B).
        b: PathBuf,
    },
    /// Print each query's corpus probe (best code and docs scores) as CSV, with the corpus
    /// its expected files belong to, to tune `corpus: auto`.
    Probe {
        root: PathBuf,
        #[arg(required = true)]
        golden: Vec<PathBuf>,
    },
    /// Ask one question twice, each in a fresh sidecar process, timing every stage.
    Repeat {
        root: PathBuf,
        question: String,
        /// The `dendrite-rag` binary.
        #[arg(long)]
        bin: PathBuf,
        #[arg(long, default_value = "code")]
        corpus: String,
        /// Also call `ask` (the configured local model through the gateway).
        #[arg(long)]
        llm: bool,
        #[arg(long, default_value_t = 2)]
        runs: usize,
    },
    /// Benchmark SWE-bench Verified file localization.
    Swebench {
        /// SWE-bench Verified instances JSON file (from eval/swebench/import.py).
        instances: PathBuf,
        /// Directory where git repositories are cloned/cached.
        #[arg(long)]
        repos_cache: PathBuf,
        /// Directory where repository indexes are stored.
        #[arg(long)]
        index_root: PathBuf,
        /// Write raw per-instance scores to JSON.
        #[arg(long)]
        json: Option<PathBuf>,
        /// Run only the Stratified-120 subset.
        #[arg(long)]
        stratified: bool,
        /// Filter by repository name.
        #[arg(long)]
        repo: Option<String>,
        /// Limit number of instances.
        #[arg(long)]
        limit: Option<usize>,
        /// 5-instance smoke test.
        #[arg(long)]
        smoke: bool,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Cmd::Swebench {
            instances,
            repos_cache,
            index_root,
            json,
            stratified,
            repo,
            limit,
            smoke,
        } => loom_eval::swebench::run(loom_eval::swebench::SwebenchOpts {
            instances_path: instances,
            repos_cache,
            index_root,
            json_out: json,
            stratified_only: stratified,
            repo_filter: repo,
            limit,
            smoke,
        }),
        Cmd::Quality {
            root,
            golden,
            index_dir,
            only,
            json,
            contexts,
        } => quality(root, golden, index_dir, only, json, contexts),
        Cmd::ExportChunks { root, index_dir, out } => {
            let mut cfg = dendrite_rag::settings::resolve(&root)?;
            if index_dir.is_some() {
                cfg.index_dir = index_dir;
            }
            let mut engine = Engine::open(&root, cfg)?;
            engine.index(false)?;
            let files = contextual::export(&engine, &out)?;
            eprintln!("exported {files} files to {}", out.display());
            Ok(())
        }
        Cmd::Compare { a, b } => compare(a, b),
        Cmd::AbstainProbe {
            root,
            golden,
            negatives,
            set,
            index_dir,
        } => abstain::run(root, golden, negatives, set, index_dir),
        Cmd::Probe { root, golden } => probe(root, golden),
        Cmd::Repeat {
            root,
            question,
            bin,
            corpus,
            llm,
            runs,
        } => {
            let runs = (0..runs)
                .map(|_| repeat::run_once(&bin, &root, &question, &corpus, llm))
                .collect::<anyhow::Result<Vec<_>>>()?;
            print!("{}", repeat::report(&question, &corpus, &runs));
            Ok(())
        }
    }
}

fn quality(
    root: PathBuf,
    golden: Vec<PathBuf>,
    index_dir: Option<PathBuf>,
    only: Option<String>,
    json: Option<PathBuf>,
    contexts: Option<PathBuf>,
) -> anyhow::Result<()> {
    let mut cfg = dendrite_rag::settings::resolve(&root)?;
    if index_dir.is_some() {
        cfg.index_dir = index_dir;
    }
    if let Some(path) = contexts {
        cfg.chunk_contexts = Some(std::sync::Arc::new(contextual::load(&path)?));
    }
    let walk = cfg.walk.clone();
    let mut engine = Engine::open(&root, cfg)?;
    let started = Instant::now();
    let report = engine.index(false)?;
    let status = engine.status()?;
    eprintln!(
        "index: {} files, {} chunks, {} vectors ({:?} coverage, model {:?}) in {:.1} s; {} changed",
        status.files,
        status.chunks,
        status.vectors,
        status.embed_coverage,
        status.embed_model,
        started.elapsed().as_secs_f64(),
        report.files_changed
    );
    let started = Instant::now();
    let baselines = Baselines::build(&root, &walk)?;
    eprintln!("baselines built in {:.1} s", started.elapsed().as_secs_f64());

    let dense = engine.has_vectors();
    let methods: Vec<Method> = Method::all(dense)
        .into_iter()
        .filter(|m| only.as_ref().is_none_or(|o| m.label().contains(o.as_str())))
        .collect();
    println!(
        "Corpus: {} ({} files, {} chunks, embedder {}, vector coverage {:.0}%)\n",
        root.display(),
        status.files,
        status.chunks,
        status.embed_model.as_deref().unwrap_or("none"),
        status.embed_coverage.unwrap_or(0.0) * 100.0
    );
    let mut set_runs = Vec::new();
    for path in golden {
        let set = golden::load(&path)?;
        let mut rows = Vec::new();
        let mut method_runs = Vec::new();
        for m in &methods {
            let mut scores = Scores::default();
            for q in &set.queries {
                let started = Instant::now();
                let units = m.run(&engine, &baselines, &q.q, *BUDGETS.last().expect("budgets"))?;
                let elapsed = started.elapsed();
                let per_budget = if m.packs_per_budget() {
                    Some(
                        BUDGETS
                            .iter()
                            .map(|&b| m.run(&engine, &baselines, &q.q, b))
                            .collect::<anyhow::Result<Vec<_>>>()?,
                    )
                } else {
                    None
                };
                scores.add(q, &units, per_budget.as_deref(), elapsed);
            }
            if json.is_some() {
                method_runs.push(runs::MethodRun {
                    label: m.label(),
                    per_query: scores.per_query.clone(),
                });
            }
            rows.push((m.label(), scores));
        }
        println!(
            "{}",
            report::table(&format!("{} ({} queries)", set.name, set.queries.len()), &rows)
        );
        if json.is_some() {
            set_runs.push(runs::SetRun {
                name: set.name,
                methods: method_runs,
            });
        }
    }
    if let Some(json_path) = json {
        let run_file = runs::RunFile {
            root: root.display().to_string(),
            sets: set_runs,
        };
        let s = serde_json::to_string_pretty(&run_file)?;
        if let Some(parent) = json_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&json_path, s)?;
    }
    Ok(())
}

fn compare(a: PathBuf, b: PathBuf) -> anyhow::Result<()> {
    let file_a: runs::RunFile = serde_json::from_str(&std::fs::read_to_string(&a)?)?;
    let file_b: runs::RunFile = serde_json::from_str(&std::fs::read_to_string(&b)?)?;
    print!("{}", runs::compare(&file_a, &file_b));
    Ok(())
}

fn probe(root: PathBuf, golden: Vec<PathBuf>) -> anyhow::Result<()> {
    let engine = Engine::open(&root, dendrite_rag::settings::resolve(&root)?)?;
    println!("set,kind,dense_code,dense_docs,lexical_code,lexical_docs,docs_share,query");
    let f = |v: Option<f32>| v.map_or(String::new(), |x| format!("{x:.4}"));
    for path in golden {
        let set = golden::load(&path)?;
        for q in &set.queries {
            let p = engine.probe(&q.q, &mut Default::default())?;
            let kind = if q.is_mixed() {
                "mixed"
            } else if q.expect.iter().all(|e| golden::is_doc(e)) {
                "docs"
            } else {
                "code"
            };
            println!(
                "{},{kind},{},{},{},{},{:.3},\"{}\"",
                set.name,
                f(p.dense_code),
                f(p.dense_docs),
                f(p.lexical_code),
                f(p.lexical_docs),
                p.docs_share,
                q.q.replace('"', "'")
            );
        }
    }
    Ok(())
}
