//! Graph queries served as `graph.*` methods (`docs/specs/graph.md` § Queries): the
//! communities, the god nodes, a path between two symbols or files, and an explanation of
//! one symbol or file.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use serde::Serialize;

use super::communities::{Community, TopSymbol};
use super::{CodeGraph, EdgeKind, Site};
use crate::engine::{Engine, Result};

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

    /// Explains a symbol (every definition of that name, up to five) or a file.
    pub fn graph_explain(&self, target: &str) -> Result<Vec<Explanation>> {
        let g = self.code_graph()?;
        let comms = Communities::load(self)?;
        if let Some(f) = g.file_id(target) {
            return Ok(vec![explain_file(&g, f, &comms)]);
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

/// A node on a found path with the `(kind, reversed)` edge that reached it.
type Hop = (u32, Option<(EdgeKind, bool)>);

/// Breadth-first search from any of `src` to any of `dst`; each returned step carries the
/// edge `(kind, reversed)` that reached it.
fn bfs(
    src: &[u32],
    dst: &[u32],
    max_hops: usize,
    next: impl Fn(u32) -> Vec<(u32, EdgeKind, bool)>,
) -> Option<Vec<Hop>> {
    let targets: HashSet<u32> = dst.iter().copied().collect();
    let mut prev: HashMap<u32, Option<(u32, EdgeKind, bool)>> = HashMap::new();
    let mut queue: VecDeque<(u32, usize)> = VecDeque::new();
    for &s in src {
        prev.insert(s, None);
        queue.push_back((s, 0));
    }
    while let Some((v, depth)) = queue.pop_front() {
        if targets.contains(&v) {
            let mut path = vec![(v, prev[&v].map(|(_, k, r)| (k, r)))];
            let mut cur = v;
            while let Some((p, _, _)) = prev[&cur] {
                path.push((p, prev[&p].map(|(_, k, r)| (k, r))));
                cur = p;
            }
            path.reverse();
            return Some(path);
        }
        if depth == max_hops {
            continue;
        }
        for (u, k, r) in next(v) {
            if let std::collections::hash_map::Entry::Vacant(e) = prev.entry(u) {
                e.insert(Some((v, k, r)));
                queue.push_back((u, depth + 1));
            }
        }
    }
    None
}

/// File → (file, kind) for every edge pointing at it.
fn incoming_edges(g: &CodeGraph) -> Vec<Vec<(u32, EdgeKind)>> {
    let mut incoming = vec![Vec::new(); g.file_count()];
    for f in 0..g.file_count() as u32 {
        for (t, k, _) in g.file_edges(f) {
            incoming[t as usize].push((f, k));
        }
    }
    incoming
}

/// Community lookup by file.
struct Communities {
    by_file: BTreeMap<String, u32>,
    labels: HashMap<u32, String>,
}

impl Communities {
    fn load(engine: &Engine) -> Result<Self> {
        Ok(Self {
            by_file: engine.ledger.community_files()?,
            labels: engine
                .ledger
                .communities()?
                .into_iter()
                .map(|c| (c.id, c.label))
                .collect(),
        })
    }

    fn of(&self, path: &str) -> Option<CommunityRef> {
        let id = *self.by_file.get(path)?;
        Some(CommunityRef {
            id,
            label: self.labels.get(&id).cloned().unwrap_or_default(),
        })
    }
}

fn neighbours(g: &CodeGraph, it: impl Iterator<Item = (u32, EdgeKind)>) -> Vec<Neighbour> {
    let mut v: Vec<(u32, EdgeKind)> = it.filter(|(s, _)| !g.sites(*s).is_empty()).collect();
    v.sort_by_key(|&(s, k)| (std::cmp::Reverse(g.in_degree(s)), g.symbol_name(s).to_string(), k));
    v.into_iter()
        .take(EXPLAIN_NEIGHBOURS)
        .map(|(s, kind)| {
            let site = &g.sites(s)[0];
            Neighbour {
                symbol: g.symbol_name(s).to_string(),
                path: site.path.clone(),
                line: site.line,
                kind,
            }
        })
        .collect()
}

fn symbol_summary(g: &CodeGraph, s: u32, community: Option<&CommunityRef>) -> String {
    let site = &g.sites(s)[0];
    let files: HashSet<u32> = g.users(s).map(|(u, _)| g.symbol_file(u)).collect();
    let mut text = format!(
        "{} is defined at {}:{} and used by {} symbols in {} files; it uses {} symbols.",
        g.symbol_name(s),
        site.path,
        site.line,
        g.in_degree(s),
        files.len(),
        g.out_degree(s)
    );
    if let Some(c) = community {
        text.push_str(&format!(" It belongs to community {} ({}).", c.id, c.label));
    }
    if g.in_degree(s) >= GOD_NODE_IN_DEGREE {
        text.push_str(" It is a god node: changes to it reach much of the codebase.");
    }
    text
}

fn explain_symbol(g: &CodeGraph, s: u32, comms: &Communities) -> Explanation {
    let community = comms.of(g.file_name(g.symbol_file(s)));
    Explanation::Symbol {
        summary: symbol_summary(g, s, community.as_ref()),
        symbol: g.symbol_name(s).to_string(),
        sites: g.sites(s).to_vec(),
        god_node: g.in_degree(s) >= GOD_NODE_IN_DEGREE,
        in_degree: g.in_degree(s),
        out_degree: g.out_degree(s),
        users: neighbours(g, g.users(s)),
        uses: neighbours(g, g.uses(s)),
        community,
    }
}

fn explain_file(g: &CodeGraph, f: u32, comms: &Communities) -> Explanation {
    let path = g.file_name(f).to_string();
    let mut defines: Vec<u32> = (0..g.symbol_count() as u32)
        .filter(|&s| g.symbol_file(s) == f && !g.sites(s).is_empty())
        .collect();
    defines.sort_by_key(|&s| (std::cmp::Reverse(g.in_degree(s)), g.symbol_name(s).to_string()));
    let defines: Vec<TopSymbol> = defines
        .into_iter()
        .take(EXPLAIN_NEIGHBOURS)
        .map(|s| TopSymbol {
            name: g.symbol_name(s).to_string(),
            path: path.clone(),
            line: g.sites(s)[0].line,
            in_degree: g.in_degree(s),
        })
        .collect();
    let mut imports = Vec::new();
    let mut out_weight: HashMap<u32, f32> = HashMap::new();
    for (t, k, w) in g.file_edges(f) {
        if k == EdgeKind::Import {
            imports.push(g.file_name(t).to_string());
        }
        *out_weight.entry(t).or_default() += w;
    }
    let mut imported_by = Vec::new();
    let mut in_weight: HashMap<u32, f32> = HashMap::new();
    for src in 0..g.file_count() as u32 {
        for (_, k, w) in g.file_edges(src).filter(|(t, _, _)| *t == f) {
            if k == EdgeKind::Import {
                imported_by.push(g.file_name(src).to_string());
            }
            *in_weight.entry(src).or_default() += w;
        }
    }
    imported_by.sort();
    imported_by.dedup();
    let heaviest = |m: HashMap<u32, f32>| -> Vec<String> {
        let mut v: Vec<(u32, f32)> = m.into_iter().collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        v.into_iter()
            .take(EXPLAIN_NEIGHBOURS)
            .map(|(t, _)| g.file_name(t).to_string())
            .collect()
    };
    let community = comms.of(&path);
    let depends_on = heaviest(out_weight);
    let used_by = heaviest(in_weight);
    let mut summary = format!(
        "{path} defines {} symbols, depends on {} files and is used by {} files.",
        defines.len(),
        depends_on.len(),
        used_by.len()
    );
    if let Some(c) = &community {
        summary.push_str(&format!(" It belongs to community {} ({}).", c.id, c.label));
    }
    Explanation::File {
        external_imports: g.external_imports(f).to_vec(),
        path,
        community,
        defines,
        imports,
        imported_by,
        depends_on,
        used_by,
        summary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Config;
    use std::fs;

    fn engine() -> (tempfile::TempDir, Engine) {
        let dir = tempfile::tempdir().unwrap();
        let files = [
            ("go.mod", "module example.com/m\n"),
            (
                "pkg/sidecar/sidecar.go",
                "package sidecar\n\nfunc LocateBinary(n string) string { return n }\n",
            ),
            (
                "pkg/sidecar/client.go",
                "package sidecar\n\nfunc Start(n string) { run(LocateBinary(n)) }\n\nfunc run(p string) {}\n",
            ),
            (
                "pkg/rag/retriever.go",
                "package rag\n\nimport \"example.com/m/pkg/sidecar\"\n\nfunc Retrieve() { sidecar.Start(\"x\") }\n",
            ),
        ];
        for (p, src) in files {
            let path = dir.path().join(p);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, src).unwrap();
        }
        let mut e = Engine::open_with_embedder(dir.path(), Config::default(), None).unwrap();
        e.index(false).unwrap();
        (dir, e)
    }

    #[test]
    fn path_follows_calls_forward_then_backward() {
        let (_d, e) = engine();
        let p = e.graph_path("Retrieve", "LocateBinary", &[], 6).unwrap();
        assert!(p.found && p.directed);
        let names: Vec<&str> = p.steps.iter().map(|s| s.node.as_str()).collect();
        assert_eq!(names, ["Retrieve", "Start", "LocateBinary"]);
        assert_eq!(p.steps[1].via, Some(EdgeKind::Call));

        let back = e.graph_path("LocateBinary", "Retrieve", &[], 6).unwrap();
        assert!(back.found && !back.directed);
        assert!(back.steps[1].reversed);

        let files = e
            .graph_path("pkg/rag/retriever.go", "pkg/sidecar/sidecar.go", &[], 6)
            .unwrap();
        assert_eq!(files.level, "file");
        assert!(files.found && files.directed);

        assert!(!e.graph_path("Retrieve", "LocateBinary", &[], 1).unwrap().found);
        assert!(
            !e.graph_path("Retrieve", "LocateBinary", &[EdgeKind::Type], 6)
                .unwrap()
                .found
        );
    }

    #[test]
    fn explains_symbols_and_files() {
        let (_d, e) = engine();
        let x = e.graph_explain("Start").unwrap();
        let Explanation::Symbol {
            users,
            uses,
            community,
            ..
        } = &x[0]
        else {
            panic!("expected a symbol: {x:?}");
        };
        assert_eq!(users[0].symbol, "Retrieve");
        assert!(uses.iter().any(|n| n.symbol == "LocateBinary"));
        assert!(community.is_some());

        let x = e.graph_explain("pkg/rag/retriever.go").unwrap();
        let Explanation::File {
            imports, depends_on, ..
        } = &x[0]
        else {
            panic!("expected a file: {x:?}");
        };
        assert!(imports.contains(&"pkg/sidecar/client.go".to_string()));
        assert!(depends_on.contains(&"pkg/sidecar/client.go".to_string()));
        let x = e.graph_explain("pkg/sidecar/client.go").unwrap();
        let Explanation::File {
            imported_by, used_by, ..
        } = &x[0]
        else {
            panic!();
        };
        assert_eq!(imported_by, &["pkg/rag/retriever.go"]);
        assert_eq!(used_by, &["pkg/rag/retriever.go"]);
        assert!(e.graph_explain("Nope").unwrap().is_empty());
    }

    #[test]
    fn communities_and_god_nodes() {
        let (_d, e) = engine();
        let cs = e.graph_communities(10, 2).unwrap();
        assert!(!cs.is_empty());
        assert!(cs[0].summary.starts_with("Community "));
        assert!(cs.iter().all(|c| c.size >= 2));
        let gods = e.graph_god_nodes(1, 10).unwrap();
        assert!(
            gods.iter()
                .any(|g| g.symbol == "LocateBinary" && g.in_degree >= 1)
        );
        assert!(e.graph_god_nodes(GOD_NODE_IN_DEGREE, 10).unwrap().is_empty());
    }
}
