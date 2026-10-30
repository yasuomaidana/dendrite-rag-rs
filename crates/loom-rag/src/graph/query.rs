//! Graph queries served as `graph.*` methods (`docs/specs/graph.md` § Queries): the
//! communities, the god nodes, a path between two symbols or files, and an explanation of
//! one symbol or file.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use serde::Serialize;

use super::communities::{Community, TopSymbol};
use super::{CodeGraph, EdgeKind, Site};
use crate::engine::{Engine, Result};

mod path;
use path::{bfs, incoming_edges};
mod explain;
pub use crate::llm::Concept;
use explain::{Communities, explain_file, explain_symbol, symbol_summary};

/// FEAT-045 calls a symbol with more than 20 users a god node.
pub const GOD_NODE_IN_DEGREE: usize = 21;
/// Neighbours listed per direction in `graph.explain`.
const EXPLAIN_NEIGHBOURS: usize = 20;
/// Member files listed per community in `graph.communities`.
const LISTED_FILES: usize = 50;

#[derive(Debug, Clone, Serialize)]
pub struct CommunityView {
    pub id: u32,
    pub label: String,
    pub size: usize,
    /// The first member files, sorted; `size` counts all of them.
    pub files: Vec<String>,
    pub top_symbols: Vec<TopSymbol>,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommunityRef {
    pub id: u32,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GodNode {
    pub symbol: String,
    pub path: String,
    pub line: usize,
    pub in_degree: usize,
    pub out_degree: usize,
    /// Files whose symbols use it.
    pub used_from_files: usize,
    pub community: Option<CommunityRef>,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Neighbour {
    pub symbol: String,
    pub path: String,
    pub line: usize,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Explanation {
    Symbol {
        symbol: String,
        sites: Vec<Site>,
        community: Option<CommunityRef>,
        god_node: bool,
        in_degree: usize,
        out_degree: usize,
        /// Symbols that call or use it.
        users: Vec<Neighbour>,
        /// Symbols it calls or uses.
        uses: Vec<Neighbour>,
        summary: String,
    },
    File {
        path: String,
        community: Option<CommunityRef>,
        /// Concepts extracted from Markdown or text files.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        concepts: Vec<crate::llm::Concept>,
        /// Symbols defined here, most users first.
        defines: Vec<TopSymbol>,
        imports: Vec<String>,
        imported_by: Vec<String>,
        external_imports: Vec<String>,
        /// Files this one calls or uses, heaviest first (all edge kinds).
        depends_on: Vec<String>,
        /// Files that call or use this one, heaviest first.
        used_by: Vec<String>,
        summary: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct PathStep {
    /// A symbol name or a file path.
    pub node: String,
    /// `path:line` of a symbol's definition; absent for files.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// The edge from the previous step; absent on the first step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<EdgeKind>,
    /// True when the edge was followed against its direction (`B` uses `A`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub reversed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphPath {
    pub found: bool,
    /// Every edge points forward (`from` uses … uses `to`).
    pub directed: bool,
    /// `symbol` or `file`: the graph the path runs through.
    pub level: &'static str,
    pub steps: Vec<PathStep>,
}

/// One-paragraph description of a community, used by `graph.communities` and as the
/// text of its summary chunk.
pub fn summarize(c: &Community) -> String {
    let mut dirs: BTreeMap<&str, usize> = BTreeMap::new();
    for f in &c.files {
        *dirs
            .entry(f.rsplit_once('/').map_or(".", |(d, _)| d))
            .or_default() += 1;
    }
    let mut dirs: Vec<(&str, usize)> = dirs.into_iter().collect();
    dirs.sort_by_key(|&(d, n)| (std::cmp::Reverse(n), d));
    let dir_list: Vec<String> = dirs.iter().take(4).map(|(d, n)| format!("{d} ({n})")).collect();
    let mut s = format!(
        "Community {}: {} files in {}{}.",
        c.label,
        c.files.len(),
        dir_list.join(", "),
        if dirs.len() > 4 { ", …" } else { "" }
    );
    if !c.top_symbols.is_empty() {
        let syms: Vec<String> = c
            .top_symbols
            .iter()
            .map(|t| format!("{} ({}:{}, {} users)", t.name, t.path, t.line, t.in_degree))
            .collect();
        s.push_str(" Most used: ");
        s.push_str(&syms.join(", "));
        s.push('.');
    }
    s
}

impl Engine {
    /// Communities with at least `min_size` files, largest first.
    pub fn graph_communities(&self, limit: usize, min_size: usize) -> Result<Vec<CommunityView>> {
        let mut cs = self.ledger.communities()?;
        cs.retain(|c| c.files.len() >= min_size.max(1));
        cs.sort_by_key(|c| (std::cmp::Reverse(c.files.len()), c.id));
        cs.truncate(limit);
        Ok(cs
            .into_iter()
            .map(|c| CommunityView {
                summary: summarize(&c),
                id: c.id,
                label: c.label,
                size: c.files.len(),
                files: c.files.into_iter().take(LISTED_FILES).collect(),
                top_symbols: c.top_symbols,
            })
            .collect())
    }

    /// Symbols with at least `min_in_degree` users, most used first.
    pub fn graph_god_nodes(&self, min_in_degree: usize, limit: usize) -> Result<Vec<GodNode>> {
        let g = self.code_graph()?;
        let comms = Communities::load(self)?;
        let mut nodes: Vec<u32> = (0..g.symbol_count() as u32)
            .filter(|&s| !g.sites(s).is_empty() && g.in_degree(s) >= min_in_degree)
            .collect();
        nodes.sort_by_key(|&s| (std::cmp::Reverse(g.in_degree(s)), g.symbol_name(s).to_string()));
        nodes.truncate(limit);
        Ok(nodes
            .into_iter()
            .map(|s| {
                let site = &g.sites(s)[0];
                let community = comms.of(g.file_name(g.symbol_file(s)));
                let used_from_files = g
                    .users(s)
                    .map(|(u, _)| g.symbol_file(u))
                    .collect::<HashSet<_>>()
                    .len();
                GodNode {
                    summary: symbol_summary(&g, s, community.as_ref()),
                    symbol: g.symbol_name(s).to_string(),
                    path: site.path.clone(),
                    line: site.line,
                    in_degree: g.in_degree(s),
                    out_degree: g.out_degree(s),
                    used_from_files,
                    community,
                }
            })
            .collect())
    }

    /// Loads stored concepts for a file path (empty if none or unindexed).
    pub fn concepts_for_file(&self, path: &str) -> Result<Vec<crate::llm::Concept>> {
        let Some(json) = self.ledger.concepts(path)? else {
            return Ok(Vec::new());
        };
        Ok(serde_json::from_str(&json).unwrap_or_default())
    }

    /// Explains a symbol (every definition of that name, up to five) or a file.
    pub fn graph_explain(&self, target: &str) -> Result<Vec<Explanation>> {
        let g = self.code_graph()?;
        let comms = Communities::load(self)?;
        if let Some(f) = g.file_id(target) {
            let path = g.file_name(f);
            let concepts = self.concepts_for_file(path)?;
            return Ok(vec![explain_file(&g, f, &comms, concepts)]);
        }
        let mut nodes: Vec<u32> = g
            .symbols_named(target)
            .iter()
            .copied()
            .filter(|&s| !g.sites(s).is_empty())
            .collect();
        nodes.sort_by_key(|&s| std::cmp::Reverse(g.in_degree(s)));
        nodes.truncate(5);
        Ok(nodes.into_iter().map(|s| explain_symbol(&g, s, &comms)).collect())
    }

    /// The shortest path from `from` to `to` (symbols or files) of at most `max_hops`
    /// edges of the given kinds (all kinds when empty). A forward path is preferred; when
    /// there is none, edges may be followed backwards.
    pub fn graph_path(&self, from: &str, to: &str, kinds: &[EdgeKind], max_hops: usize) -> Result<GraphPath> {
        let g = self.code_graph()?;
        let kinds: HashSet<EdgeKind> = if kinds.is_empty() {
            [EdgeKind::Call, EdgeKind::Import, EdgeKind::Type].into()
        } else {
            kinds.iter().copied().collect()
        };
        let symbol_nodes = |name: &str| -> Vec<u32> {
            g.symbols_named(name)
                .iter()
                .copied()
                .filter(|&s| !g.sites(s).is_empty())
                .collect()
        };
        let (src_syms, dst_syms) = (symbol_nodes(from), symbol_nodes(to));
        let both_symbols = g.file_id(from).is_none()
            && g.file_id(to).is_none()
            && !src_syms.is_empty()
            && !dst_syms.is_empty();
        if both_symbols {
            for directed in [true, false] {
                let next = |s: u32| -> Vec<(u32, EdgeKind, bool)> {
                    let mut out: Vec<(u32, EdgeKind, bool)> = g
                        .uses(s)
                        .filter(|(_, k)| kinds.contains(k))
                        .map(|(t, k)| (t, k, false))
                        .collect();
                    if !directed {
                        out.extend(
                            g.users(s)
                                .filter(|(_, k)| kinds.contains(k))
                                .map(|(t, k)| (t, k, true)),
                        );
                    }
                    out.sort_unstable_by_key(|&(t, k, r)| (r, t, k));
                    out
                };
                if let Some(path) = bfs(&src_syms, &dst_syms, max_hops, next) {
                    let steps = path
                        .into_iter()
                        .map(|(s, via)| {
                            let site = &g.sites(s)[0];
                            PathStep {
                                node: g.symbol_name(s).to_string(),
                                location: Some(format!("{}:{}", site.path, site.line)),
                                via: via.map(|(k, _)| k),
                                reversed: via.is_some_and(|(_, r)| r),
                            }
                        })
                        .collect();
                    return Ok(GraphPath {
                        found: true,
                        directed,
                        level: "symbol",
                        steps,
                    });
                }
            }
            return Ok(GraphPath {
                found: false,
                directed: false,
                level: "symbol",
                steps: Vec::new(),
            });
        }
        // At file level: a symbol stands for the files defining it.
        let files_of = |name: &str, syms: &[u32]| -> Vec<u32> {
            match g.file_id(name) {
                Some(f) => vec![f],
                None => {
                    let mut fs: Vec<u32> = syms.iter().map(|&s| g.symbol_file(s)).collect();
                    fs.sort_unstable();
                    fs.dedup();
                    fs
                }
            }
        };
        let (src, dst) = (files_of(from, &src_syms), files_of(to, &dst_syms));
        let incoming = incoming_edges(&g);
        for directed in [true, false] {
            let next = |f: u32| -> Vec<(u32, EdgeKind, bool)> {
                let mut out: Vec<(u32, EdgeKind, bool)> = g
                    .file_edges(f)
                    .filter(|(_, k, _)| kinds.contains(k))
                    .map(|(t, k, _)| (t, k, false))
                    .collect();
                if !directed {
                    out.extend(
                        incoming[f as usize]
                            .iter()
                            .filter(|(_, k)| kinds.contains(k))
                            .map(|&(t, k)| (t, k, true)),
                    );
                }
                out.sort_unstable_by_key(|&(t, k, r)| (r, t, k));
                out
            };
            if let Some(path) = bfs(&src, &dst, max_hops, next) {
                let steps = path
                    .into_iter()
                    .map(|(f, via)| PathStep {
                        node: g.file_name(f).to_string(),
                        location: None,
                        via: via.map(|(k, _)| k),
                        reversed: via.is_some_and(|(_, r)| r),
                    })
                    .collect();
                return Ok(GraphPath {
                    found: true,
                    directed,
                    level: "file",
                    steps,
                });
            }
        }
        Ok(GraphPath {
            found: false,
            directed: false,
            level: "file",
            steps: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests;
