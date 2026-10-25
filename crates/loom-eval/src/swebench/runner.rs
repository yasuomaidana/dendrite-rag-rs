//! The SWE-bench Verified file localization runner (DRAG-54 / DRAG-55).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use loom_rag::corpus::CorpusFilter;
use loom_rag::engine::Engine;
use loom_rag::retrieve::{Mode, SearchOpts, estimate_tokens};

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

pub fn run(opts: SwebenchOpts) -> anyhow::Result<()> {
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

            // Checkout detached worktree at base commit
            let status = Command::new("git")
                .arg("-C")
                .arg(&repo_dir)
                .args(["checkout", "--detach", &inst.base_commit, "--force"])
                .output()?;
            if !status.status.success() {
                eprintln!(
                    "    git checkout failed: {}",
                    String::from_utf8_lossy(&status.stderr)
                );
                *skips.checkout_failed.entry(repo_name.clone()).or_default() += 1;
                continue;
            }

            // Incremental index: Merkle diff skips unchanged files, embeddings cached by content_hash
            let started = Instant::now();
            let mut engine = Engine::open(&repo_dir, cfg.clone())?;
            let report = engine.index(false)?;
            eprintln!(
                "    indexed in {:.1}s ({} files changed, {} chunks added, {} embedded)",
                started.elapsed().as_secs_f64(),
                report.files_changed,
                report.chunks_added,
                report.embedded
            );

            // Build file baselines
            let baselines = Baselines::build(&repo_dir, &walk_opts)?;

            // Use Q1 as primary, evaluate each method
            let query = &inst.q1;
            let all = &mut all_method_scores;

            // 1. grep-agent
            let grep_files: Vec<String> = baselines.grep(query).into_iter().map(|u| u.path).collect();
            record(all, "grep-agent", InstanceScore::compute(inst, &grep_files, None));

            // 2. bm25-file
            let bm25_files: Vec<String> = baselines.bm25_files(query)?.into_iter().map(|u| u.path).collect();
            record(all, "bm25-file", InstanceScore::compute(inst, &bm25_files, None));

            // 3-5. chunk-bm25 variants over lexical chunk hits
            let lex_hits = engine.search(
                query,
                &SearchOpts {
                    top_k: 200,
                    mode: Mode::Lexical,
                    corpus: CorpusFilter::Code,
                    ..SearchOpts::default()
                },
            )?;
            let lex_pairs: Vec<(String, f32)> = lex_hits.into_iter().map(|h| (h.path, h.score)).collect();
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
            let hybrid_hits = engine.search(
                query,
                &SearchOpts {
                    top_k: 200,
                    mode: Mode::Hybrid,
                    corpus: CorpusFilter::Code,
                    ..SearchOpts::default()
                },
            )?;
            let hybrid_pairs: Vec<(String, f32)> =
                hybrid_hits.into_iter().map(|h| (h.path, h.score)).collect();
            let hybrid_files = files_of(pool_chunk_scores(
                hybrid_pairs.clone(),
                PoolingStrategy::Max,
                false,
            ));

            // Tokens to first gold span of the plain hybrid context pack (exact path equality)
            let pack = engine.context_for(query, 4000, CorpusFilter::Code)?;
            let mut tokens_to_gold = None;
            let mut read_tokens = 0;
            for span in &pack.spans {
                read_tokens += estimate_tokens(&span.content);
                if path_is_gold(&inst.expected_files, &span.path) {
                    tokens_to_gold = Some(read_tokens);
                    break;
                }
            }
            record(
                all,
                "hybrid",
                InstanceScore::compute(inst, &hybrid_files, tokens_to_gold),
            );

            // 7. hybrid-decay: ranked differently from the context pack, so no tokens_to_gold
            let hybrid_decay_files =
                files_of(pool_chunk_scores(hybrid_pairs, PoolingStrategy::DecayTop3, false));
            record(
                all,
                "hybrid-decay",
                InstanceScore::compute(inst, &hybrid_decay_files, None),
            );

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
