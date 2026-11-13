//! The SWE-bench Verified file localization runner (DRAG-54 / DRAG-55).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use loom_rag::corpus::CorpusFilter;
use loom_rag::engine::{Config, Engine};
use loom_rag::retrieve::{Mode, SearchOpts, estimate_tokens};
use loom_rag::walk::WalkOptions;

use super::instance::SweInstance;
use super::metrics::{InstanceScore, path_is_gold};
use super::output::{MethodScores, print_results, repo_slug, write_json};
use super::pool::{PoolingStrategy, pool_chunk_scores};
use crate::baselines::Baselines;

pub struct SwebenchOpts {
    pub instances_path: PathBuf,
    pub repos_cache: PathBuf,
    pub index_root: PathBuf,
    pub json_out: Option<PathBuf>,
    pub stratified_only: bool,
    pub repo_filter: Option<String>,
    pub limit: Option<usize>,
    pub smoke: bool,
}

/// Instances that could not be scored, per repository.
#[derive(Default)]
struct SkipStats {
    missing_repo: BTreeMap<String, usize>,
    checkout_failed: BTreeMap<String, usize>,
}

impl SkipStats {
    fn print(&self, scored: usize) {
        let missing: usize = self.missing_repo.values().sum();
        let failed: usize = self.checkout_failed.values().sum();
        println!("\n## Skipped instances\n");
        for (repo, n) in &self.missing_repo {
            println!("- {repo}: {n} skipped (repository directory missing)");
        }
        for (repo, n) in &self.checkout_failed {
            println!("- {repo}: {n} skipped (git checkout failed)");
        }
        println!(
            "- total skipped: {} ({missing} missing repository, {failed} checkout failed)",
            missing + failed
        );
        println!("- instances scored: {scored}");
    }
}

fn record(all: &mut MethodScores, method: &str, score: InstanceScore) {
    all.entry(method.to_string()).or_default().push(score);
}

fn files_of(pooled: Vec<(String, f32)>) -> Vec<String> {
    pooled.into_iter().map(|(p, _)| p).collect()
}

/// The instances `opts` selects, in the file's order.
fn select_instances(opts: &SwebenchOpts) -> anyhow::Result<Vec<SweInstance>> {
    let mut instances = super::instance::load_instances(&opts.instances_path)?;
    if opts.stratified_only {
        instances.retain(|i| i.stratified_120);
    }
    if let Some(rf) = &opts.repo_filter {
        instances.retain(|i| i.repo.contains(rf));
    }
    if opts.smoke {
        instances.truncate(5);
    } else if let Some(limit) = opts.limit {
        instances.truncate(limit);
    }
    Ok(instances)
}

/// Checks `repo_dir` out detached at `commit`; false (reported) when git refuses.
fn checkout(repo_dir: &Path, commit: &str) -> anyhow::Result<bool> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args(["checkout", "--detach", commit, "--force"])
        .output()?;
    if !status.status.success() {
        eprintln!(
            "    git checkout failed: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }
    Ok(status.status.success())
}

/// Tokens a reader spends in the hybrid context pack before the first gold span (exact path equality).
fn tokens_to_gold(engine: &Engine, query: &str, inst: &SweInstance) -> anyhow::Result<Option<usize>> {
    let pack = engine.context_for(query, 4000, CorpusFilter::Code)?;
    let mut read_tokens = 0;
    for span in &pack.spans {
        read_tokens += estimate_tokens(&span.content);
        if path_is_gold(&inst.expected_files, &span.path) {
            return Ok(Some(read_tokens));
        }
    }
    Ok(None)
}

/// Code chunks ranked by `mode`, as `(path, score)`.
fn chunk_hits(engine: &Engine, query: &str, mode: Mode) -> anyhow::Result<Vec<(String, f32)>> {
    let hits = engine.search(
        query,
        &SearchOpts {
            top_k: 200,
            mode,
            corpus: CorpusFilter::Code,
            ..SearchOpts::default()
        },
    )?;
    Ok(hits.into_iter().map(|h| (h.path, h.score)).collect())
}

/// Scores the chunk-bm25 variants and the hybrid methods of one instance.
fn score_engine_methods(
    engine: &Engine,
    query: &str,
    inst: &SweInstance,
    all: &mut MethodScores,
) -> anyhow::Result<()> {
    // 3-5. chunk-bm25 variants over lexical chunk hits
    let lex_pairs = chunk_hits(engine, query, Mode::Lexical)?;
    let variants = [
        ("chunk-bm25", PoolingStrategy::Max, false),
        ("chunk-bm25-decay", PoolingStrategy::DecayTop3, false),
        ("chunk-bm25-notests", PoolingStrategy::DecayTop3, true),
    ];
    for (label, strategy, exclude) in variants {
        let files = files_of(pool_chunk_scores(lex_pairs.clone(), strategy, exclude));
        record(all, label, InstanceScore::compute(inst, &files, None));
    }

    // 6. hybrid (Max)
    let hybrid_pairs = chunk_hits(engine, query, Mode::Hybrid)?;
    let hybrid_files = files_of(pool_chunk_scores(
        hybrid_pairs.clone(),
        PoolingStrategy::Max,
        false,
    ));
    let tokens = tokens_to_gold(engine, query, inst)?;
    record(all, "hybrid", InstanceScore::compute(inst, &hybrid_files, tokens));

    // 7. hybrid-decay: ranked differently from the context pack, so no tokens_to_gold
    let hybrid_decay_files = files_of(pool_chunk_scores(hybrid_pairs, PoolingStrategy::DecayTop3, false));
    record(
        all,
        "hybrid-decay",
        InstanceScore::compute(inst, &hybrid_decay_files, None),
    );
    Ok(())
}

/// Indexes the checked-out repository and scores every method on the instance's first query.
fn score_instance(
    repo_dir: &Path,
    cfg: &Config,
    walk_opts: &WalkOptions,
    inst: &SweInstance,
    all: &mut MethodScores,
) -> anyhow::Result<()> {
    // Incremental index: Merkle diff skips unchanged files, embeddings cached by content_hash
    let started = Instant::now();
    let mut engine = Engine::open(repo_dir, cfg.clone())?;
    let report = engine.index(false)?;
    eprintln!(
        "    indexed in {:.1}s ({} files changed, {} chunks added, {} embedded)",
        started.elapsed().as_secs_f64(),
        report.files_changed,
        report.chunks_added,
        report.embedded
    );
    let baselines = Baselines::build(repo_dir, walk_opts)?;

    // Use Q1 as primary, evaluate each method
    let query = &inst.q1;
    // 1. grep-agent
    let grep_files: Vec<String> = baselines.grep(query).into_iter().map(|u| u.path).collect();
    record(all, "grep-agent", InstanceScore::compute(inst, &grep_files, None));
    // 2. bm25-file
    let bm25_files: Vec<String> = baselines.bm25_files(query)?.into_iter().map(|u| u.path).collect();
    record(all, "bm25-file", InstanceScore::compute(inst, &bm25_files, None));
    score_engine_methods(&engine, query, inst, all)
}

pub fn run(opts: SwebenchOpts) -> anyhow::Result<()> {
    let instances = select_instances(&opts)?;
    if instances.is_empty() {
        eprintln!("No instances matched the filter.");
        return Ok(());
    }

    // Group by repo
    let mut by_repo: BTreeMap<String, Vec<SweInstance>> = BTreeMap::new();
    for inst in instances {
        by_repo.entry(inst.repo.clone()).or_default().push(inst);
    }

    eprintln!(
        "Running SWE-bench file localization over {} instances across {} repos...",
        by_repo.values().map(|v| v.len()).sum::<usize>(),
        by_repo.len()
    );

    let mut all_method_scores = MethodScores::new();
    let mut skips = SkipStats::default();
    let mut scored = 0usize;

    for (repo_name, mut repo_instances) in by_repo {
        let repo_dir = opts.repos_cache.join(&repo_name);
        if !repo_dir.is_dir() {
            eprintln!(
                "Skipping repository {repo_name}: directory {} not found.",
                repo_dir.display()
            );
            *skips.missing_repo.entry(repo_name).or_default() += repo_instances.len();
            continue;
        }

        let repo_index_dir = opts.index_root.join(repo_slug(&repo_name));
        std::fs::create_dir_all(&repo_index_dir)?;

        // Sort instances by created_at / commit order
        repo_instances.sort_by(|a, b| a.created_at.cmp(&b.created_at));

        eprintln!("--> Repository {repo_name} ({} instances)", repo_instances.len());

        let mut cfg = dendrite_rag::settings::resolve(&repo_dir)?;
        cfg.index_dir = Some(repo_index_dir);
        let walk_opts = cfg.walk.clone();

        for (idx, inst) in repo_instances.iter().enumerate() {
            eprintln!(
                "  [{}/{}] Instance {} (commit {})",
                idx + 1,
                repo_instances.len(),
                inst.instance_id,
                &inst.base_commit[..8.min(inst.base_commit.len())]
            );
            if !checkout(&repo_dir, &inst.base_commit)? {
                *skips.checkout_failed.entry(repo_name.clone()).or_default() += 1;
                continue;
            }
            score_instance(&repo_dir, &cfg, &walk_opts, inst, &mut all_method_scores)?;
            scored += 1;
        }
    }

    skips.print(scored);
    print_results(&all_method_scores);

    if let Some(out_path) = &opts.json_out {
        write_json(
            out_path,
            opts.repos_cache.display().to_string(),
            &all_method_scores,
        )?;
    }

    Ok(())
}
