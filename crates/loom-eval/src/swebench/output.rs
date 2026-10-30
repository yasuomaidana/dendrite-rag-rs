//! Result tables and `--json` output for the SWE-bench runner (DRAG-54 / DRAG-55).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::instance::LeakageClass;
use super::metrics::{InstanceScore, summarize_report};
use crate::metrics::QueryScore;
use crate::runs::{MethodRun, RunFile, SetRun};

/// Evaluated methods, in report order.
pub const METHODS: [&str; 7] = [
    "grep-agent",
    "bm25-file",
    "chunk-bm25",
    "chunk-bm25-decay",
    "chunk-bm25-notests",
    "hybrid",
    "hybrid-decay",
];

const LEAKAGE_CLASSES: [LeakageClass; 3] = [
    LeakageClass::PathExplicit,
    LeakageClass::TracebackSymbol,
    LeakageClass::Opaque,
];

/// Name of the set holding every scored instance.
const SET_ALL: &str = "swebench-verified";

/// Filesystem/set-name friendly repository slug (`owner/name` -> `owner_name`).
pub fn repo_slug(repo: &str) -> String {
    repo.replace('/', "_")
}

/// Per-instance scores keyed by method label.
pub type MethodScores = BTreeMap<String, Vec<InstanceScore>>;

fn print_table(heading: &str, all: &MethodScores, keep: impl Fn(&InstanceScore) -> bool) {
    println!("\n{heading}\n");
    println!(
        "| Method | n | Micro Hit@1 | Micro Hit@5 | Micro Hit@10 | Repo-Macro Hit@10 | Gold-recall@10 | MRR@10 | Multi-file All@10 |"
    );
    println!("|---|---|---|---|---|---|---|---|---|");

    for m in METHODS {
        let Some(all_scores) = all.get(m) else {
            continue;
        };
        let scores: Vec<InstanceScore> = all_scores.iter().filter(|s| keep(s)).cloned().collect();
        if scores.is_empty() {
            continue;
        }
        let (micro, repo_macro, _, _) = summarize_report(&scores);
        let all10_str = micro
            .all_gold10
            .map(|v| format!("{v:.3}"))
            .unwrap_or_else(|| "—".into());
        println!(
            "| {m} | {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} | {all10_str} |",
            micro.n,
            micro.recall1,
            micro.recall5,
            micro.recall10,
            repo_macro.recall10,
            micro.gold_recall10,
            micro.mrr10
        );
    }
}

/// Prints the overall results table followed by one table per leakage class.
pub fn print_results(all: &MethodScores) {
    print_table("# SWE-bench Verified Localization Results", all, |_| true);
    for class in LEAKAGE_CLASSES {
        let present = all
            .values()
            .any(|scores| scores.iter().any(|s| s.leakage_class == class));
        if present {
            let heading = format!("## Leakage class: {}", class.as_str());
            print_table(&heading, all, |s| s.leakage_class == class);
        }
    }
}

fn to_query_score(s: &InstanceScore) -> QueryScore {
    QueryScore {
        q: s.instance_id.clone(),
        rr: s.mrr10,
        ndcg: s.ndcg10,
        recall: Vec::new(),
    }
}

fn make_set(name: String, all: &MethodScores, keep: impl Fn(&InstanceScore) -> bool) -> Option<SetRun> {
    let mut methods = Vec::new();
    for m in METHODS {
        let Some(scores) = all.get(m) else {
            continue;
        };
        let per_query: Vec<QueryScore> = scores.iter().filter(|s| keep(s)).map(to_query_score).collect();
        if !per_query.is_empty() {
            methods.push(MethodRun {
                label: m.to_string(),
                per_query,
            });
        }
    }
    if methods.is_empty() {
        None
    } else {
        Some(SetRun { name, methods })
    }
}

/// Builds the `loom-eval compare`-compatible run: all instances, per repository and per
/// leakage class.
pub fn build_run_file(root: String, all: &MethodScores) -> RunFile {
    let mut sets = Vec::new();
    sets.extend(make_set(SET_ALL.to_string(), all, |_| true));

    let slugs: BTreeSet<String> = all
        .values()
        .flat_map(|scores| scores.iter().map(|s| repo_slug(&s.repo)))
        .collect();
    for slug in slugs {
        let name = format!("{SET_ALL}/{slug}");
        sets.extend(make_set(name, all, |s| repo_slug(&s.repo) == slug));
    }

    for class in LEAKAGE_CLASSES {
        let name = format!("{SET_ALL}/{}", class.as_str());
        sets.extend(make_set(name, all, |s| s.leakage_class == class));
    }

    RunFile { root, sets }
}

/// Path of the per-instance detail file written next to the run file.
pub fn detail_path(out_path: &Path) -> PathBuf {
    let mut name = out_path.as_os_str().to_os_string();
    name.push(".detail.json");
    PathBuf::from(name)
}

/// Writes `<out_path>` (compare-compatible [`RunFile`]) and `<out_path>.detail.json`
/// (per-instance scores).
pub fn write_json(out_path: &Path, root: String, all: &MethodScores) -> anyhow::Result<()> {
    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let run = build_run_file(root, all);
    std::fs::write(out_path, serde_json::to_string_pretty(&run)?)?;

    let detail = detail_path(out_path);
    std::fs::write(&detail, serde_json::to_string_pretty(all)?)?;
    eprintln!(
        "\nWrote compare-compatible run to {} and detailed scores to {}",
        out_path.display(),
        detail.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(id: &str, repo: &str, class: LeakageClass, mrr: f64) -> InstanceScore {
        InstanceScore {
            instance_id: id.into(),
            repo: repo.into(),
            leakage_class: class,
            recall1: mrr > 0.99,
            recall5: mrr > 0.0,
            recall10: mrr > 0.0,
            all_gold5: None,
            all_gold10: None,
            gold_recall10: mrr,
            mrr10: mrr,
            ndcg10: mrr,
            tokens_to_gold: None,
        }
    }

    #[test]
    fn run_file_has_overall_repo_and_leakage_sets() {
        let mut all = MethodScores::new();
        all.insert(
            "hybrid".into(),
            vec![
                score("a-1", "a/b", LeakageClass::Opaque, 1.0),
                score("c-1", "c/d", LeakageClass::PathExplicit, 0.5),
            ],
        );
        let run = build_run_file("root".into(), &all);
        let names: Vec<&str> = run.sets.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "swebench-verified",
                "swebench-verified/a_b",
                "swebench-verified/c_d",
                "swebench-verified/path_explicit",
                "swebench-verified/opaque_nl",
            ]
        );
        let q = &run.sets[0].methods[0].per_query[1];
        assert_eq!(q.q, "c-1");
        assert!(q.recall.is_empty());
    }

    #[test]
    fn detail_path_appends_suffix() {
        assert_eq!(
            detail_path(Path::new("out/run.json")),
            PathBuf::from("out/run.json.detail.json")
        );
    }
}
