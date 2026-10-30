//! Communities of the file graph: Leiden partition, ids kept stable across index runs,
//! and a label and the most-used symbols for each (`docs/specs/graph.md` § Communities).

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::CodeGraph;
use super::leiden::leiden;

/// Symbols listed per community.
const TOP_SYMBOLS: usize = 10;

/// One community as stored in the ledger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Community {
    pub id: u32,
    /// The members' main directory and their most-used symbol, e.g. `pkg/synapse · Synapse`.
    pub label: String,
    /// Member files, sorted.
    pub files: Vec<String>,
    /// The most-used symbols defined in the community, most users first.
    pub top_symbols: Vec<TopSymbol>,
    /// Id of the community's summary chunk (`docs/specs/graph.md` § Summary chunks); none
    /// for a single file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_chunk: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopSymbol {
    pub name: String,
    pub path: String,
    pub line: usize,
    pub in_degree: usize,
    /// The chunk holding the definition.
    #[serde(default)]
    pub chunk: String,
}

/// Partitions the graph's files. `previous` maps files to the ids of the last run; each new
/// community, largest first, takes the free previous id it shares most files with, so
/// labels and citations do not churn when one file changes.
pub fn detect(g: &CodeGraph, previous: &BTreeMap<String, u32>, resolution: f64) -> Vec<Community> {
    let membership = leiden(&g.undirected(), resolution);
    let mut groups: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for (file, &c) in membership.iter().enumerate() {
        groups.entry(c).or_default().push(file as u32);
    }
    let mut groups: Vec<Vec<u32>> = groups.into_values().collect();
    groups.sort_by_key(|files| (std::cmp::Reverse(files.len()), files[0]));

    let mut next_id = previous.values().max().map_or(0, |m| m + 1);
    let mut taken: HashSet<u32> = HashSet::new();
    let mut out = Vec::with_capacity(groups.len());
    for files in groups {
        let mut overlap: HashMap<u32, usize> = HashMap::new();
        for &f in &files {
            if let Some(&old) = previous.get(g.file_name(f)) {
                *overlap.entry(old).or_default() += 1;
            }
        }
        let reused = overlap
            .into_iter()
            .filter(|(id, _)| !taken.contains(id))
            .max_by_key(|&(id, n)| (n, std::cmp::Reverse(id)))
            .map(|(id, _)| id);
        let id = reused.unwrap_or_else(|| {
            next_id += 1;
            next_id - 1
        });
        taken.insert(id);
        out.push(describe(g, id, &files));
    }
    out.sort_by_key(|c| c.id);
    out
}

fn describe(g: &CodeGraph, id: u32, files: &[u32]) -> Community {
    let members: HashSet<u32> = files.iter().copied().collect();
    let mut symbols: Vec<u32> = (0..g.symbol_count() as u32)
        .filter(|&s| members.contains(&g.symbol_file(s)) && !g.sites(s).is_empty())
        .collect();
    symbols.sort_by_key(|&s| (std::cmp::Reverse(g.in_degree(s)), g.symbol_name(s).to_string()));
    let top_symbols: Vec<TopSymbol> = symbols
        .iter()
        .take(TOP_SYMBOLS)
        .map(|&s| {
            let site = &g.sites(s)[0];
            TopSymbol {
                name: g.symbol_name(s).to_string(),
                path: site.path.clone(),
                line: site.line,
                in_degree: g.in_degree(s),
                chunk: site.chunk.clone(),
            }
        })
        .collect();
    let mut paths: Vec<String> = files.iter().map(|&f| g.file_name(f).to_string()).collect();
    paths.sort();
    let dir = common_dir(&paths);
    let label = match top_symbols.first() {
        Some(s) if !dir.is_empty() => format!("{dir} · {}", s.name),
        Some(s) => s.name.clone(),
        None if !dir.is_empty() => dir,
        None => paths[0].clone(),
    };
    Community {
        id,
        label,
        files: paths,
        top_symbols,
        summary_chunk: None,
    }
}

/// The directory holding the most member files (the first in path order on a tie). A
/// community that spans `cmd/` and `pkg/` is still named after where most of it lives.
fn common_dir(paths: &[String]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for p in paths {
        *counts
            .entry(p.rsplit_once('/').map_or("", |(d, _)| d))
            .or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|&(d, n)| (n, std::cmp::Reverse(d)))
        .map_or_else(String::new, |(d, _)| d.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::tests::graph_of;
    use rstest::{fixture, rstest};

    #[fixture]
    fn sources() -> Vec<(&'static str, &'static str)> {
        vec![
            ("pkg/a/a1.go", "package a\n\nfunc A1() { A2(); A3() }\n"),
            ("pkg/a/a2.go", "package a\n\nfunc A2() { A3() }\n"),
            ("pkg/a/a3.go", "package a\n\nfunc A3() { A1() }\n"),
            ("pkg/b/b1.go", "package b\n\nfunc B1() { B2(); B3() }\n"),
            ("pkg/b/b2.go", "package b\n\nfunc B2() { B3() }\n"),
            ("pkg/b/b3.go", "package b\n\nfunc B3() { B1() }\n"),
            ("README.md", "# Demo\n"),
        ]
    }

    #[rstest]
    fn groups_packages_and_labels_them(sources: Vec<(&'static str, &'static str)>) {
        let g = graph_of(&sources, Some("example.com/m"));
        let cs = detect(&g, &BTreeMap::new(), 1.0);
        let a = cs
            .iter()
            .find(|c| c.files.contains(&"pkg/a/a1.go".into()))
            .unwrap();
        assert_eq!(a.files, ["pkg/a/a1.go", "pkg/a/a2.go", "pkg/a/a3.go"]);
        assert!(a.label.starts_with("pkg/a · A"), "{}", a.label);
        assert_eq!(a.top_symbols[0].name, "A3");
        assert!(cs.iter().any(|c| c.files == ["README.md"]));
    }

    #[rstest]
    fn ids_survive_a_rerun(sources: Vec<(&'static str, &'static str)>) {
        let g = graph_of(&sources, Some("example.com/m"));
        let first = detect(&g, &BTreeMap::new(), 1.0);
        let previous: BTreeMap<String, u32> = first
            .iter()
            .flat_map(|c| c.files.iter().map(move |f| (f.clone(), c.id)))
            .collect();
        // Rename-free change: package b gains a file; a keeps its id.
        let mut more = sources;
        more.push(("pkg/b/b4.go", "package b\n\nfunc B4() { B1() }\n"));
        let second = detect(&graph_of(&more, Some("example.com/m")), &previous, 1.0);
        let id_of = |cs: &[Community], f: &str| cs.iter().find(|c| c.files.contains(&f.into())).unwrap().id;
        assert_eq!(id_of(&first, "pkg/a/a1.go"), id_of(&second, "pkg/a/a1.go"));
        assert_eq!(id_of(&first, "pkg/b/b1.go"), id_of(&second, "pkg/b/b4.go"));
    }
}
