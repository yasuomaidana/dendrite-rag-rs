//! Calibrating `[rerank] abstain` (DRAG-51): the best reranked hit's relevance for
//! questions the project answers (golden sets) and for questions it cannot answer
//! (`eval/golden-negatives.json`), and how a threshold would split them.
//!
//!     loom-eval abstain-probe <root> <golden…> --negatives eval/golden-negatives.json --set dendrite [--index-dir DIR]

use std::collections::BTreeMap;
use std::path::PathBuf;

use loom_rag::engine::Engine;
use loom_rag::retrieve::SearchOpts;
use serde::Deserialize;

use crate::golden;

#[derive(Deserialize)]
struct Negatives {
    sets: BTreeMap<String, Vec<String>>,
}

/// The best hit's relevance probability and whether an expected file is in the top 10.
fn probe(engine: &Engine, q: &str, expect: &[String]) -> anyhow::Result<(f32, bool)> {
    let hits = engine.search(q, &SearchOpts::default())?;
    let top = hits
        .first()
        .and_then(|h| engine.rerank_probability(h.score))
        .ok_or_else(|| {
            anyhow::anyhow!("abstain-probe needs a reranker ([rerank] or DENDRITE_RAG_RERANK_*)")
        })?;
    let found = hits.iter().take(10).any(|h| expect.contains(&h.path));
    Ok((top, found))
}

pub fn run(
    root: PathBuf,
    golden_sets: Vec<PathBuf>,
    negatives: PathBuf,
    set: String,
    index_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let mut cfg = dendrite_rag::settings::resolve(&root)?;
    cfg.rerank.abstain = None;
    if index_dir.is_some() {
        cfg.index_dir = index_dir;
    }
    let mut engine = Engine::open(&root, cfg)?;
    engine.index(false)?;
    let mut positives: Vec<(f32, bool)> = Vec::new();
    for path in golden_sets {
        for q in golden::load(&path)?.queries {
            positives.push(probe(&engine, &q.q, &q.expect)?);
        }
    }
    let neg: Negatives = serde_json::from_str(&std::fs::read_to_string(&negatives)?)?;
    let questions = neg
        .sets
        .get(&set)
        .ok_or_else(|| anyhow::anyhow!("no set {set:?} in {}", negatives.display()))?;
    let mut negative: Vec<f32> = Vec::new();
    for q in questions {
        let (p, _) = probe(&engine, q, &[])?;
        println!("negative {p:.3}  {q}");
        negative.push(p);
    }
    let answered: Vec<f32> = positives.iter().filter(|(_, f)| *f).map(|(p, _)| *p).collect();
    println!(
        "\n{} golden questions ({} answered in the top 10), {} negatives\n",
        positives.len(),
        answered.len(),
        negative.len()
    );
    println!(
        "| abstain below | negatives abstained | golden abstained | answered golden abstained |\n|---|---|---|---|"
    );
    let share = |v: &[f32], t: f32| v.iter().filter(|&&p| p < t).count() as f64 / v.len().max(1) as f64;
    let all: Vec<f32> = positives.iter().map(|(p, _)| *p).collect();
    for i in 1..20 {
        let t = i as f32 * 0.05;
        println!(
            "| {t:.2} | {:.2} | {:.2} | {:.2} |",
            share(&negative, t),
            share(&all, t),
            share(&answered, t)
        );
    }
    Ok(())
}
