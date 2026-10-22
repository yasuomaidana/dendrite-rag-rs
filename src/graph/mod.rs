//! The typed code graph (`docs/specs/graph.md`): files linked by calls, type references
//! and imports, and defined symbols linked by the calls and type uses inside them.
//!
//! It is derived from the ledger (chunk `calls`/`refs` and file `imports`), so it is
//! rebuilt in memory after each index change instead of being stored.

pub mod communities;
pub mod leiden;
pub mod query;
pub mod resolve;
pub mod summary;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::chunk::Chunk;
use crate::engine::{Engine, Result};
use crate::lang::Lang;
use crate::ledger::FileEntry;
use resolve::Resolver;

/// Names defined in more places than this are too ambiguous to link (`new`, `run`, `init`).
const MAX_DEF_FANOUT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EdgeKind {
    Call,
    Import,
    Type,
}

impl EdgeKind {
    /// Weight of one edge of this kind in the undirected graph communities are found on.
    pub fn weight(self) -> f32 {
        match self {
            EdgeKind::Call => 1.0,
            EdgeKind::Import => 0.6,
            EdgeKind::Type => 0.3,
        }
    }
}

/// Where a symbol is defined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Site {
    pub path: String,
    pub line: usize,
    pub chunk: String,
}

#[derive(Debug, Default)]
pub struct CodeGraph {
    files: Vec<String>,
    file_ids: HashMap<String, u32>,
    /// File → (file, kind) → summed weight; directed, self-loops dropped.
    file_out: Vec<BTreeMap<(u32, EdgeKind), f32>>,
    /// Symbol nodes are `(name, defining file)`: a name defined in two packages is two nodes.
    symbols: Vec<(String, u32)>,
    symbol_ids: HashMap<(String, u32), u32>,
    by_name: HashMap<String, Vec<u32>>,
    sites: Vec<Vec<Site>>,
    /// Symbol → (symbol, kind), directed from the user to the definition.
    sym_out: Vec<HashSet<(u32, EdgeKind)>>,
    sym_in: Vec<HashSet<(u32, EdgeKind)>>,
    /// Import specifiers that resolved to no project file, per file (libraries, std).
    external: BTreeMap<u32, Vec<String>>,
}

impl CodeGraph {
    /// Builds the graph from every stored chunk, the file table and the `go.mod` files.
    pub fn build(
        chunks: impl IntoIterator<Item = Chunk>,
        files: &BTreeMap<String, FileEntry>,
        go_mods: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        let mut g = CodeGraph::default();
        for path in files.keys() {
            g.file(path);
        }
        // Collect definitions first, keeping per chunk what it uses.
        let mut uses: Vec<Uses> = Vec::new();
        for c in chunks {
            if summary::is_summary(&c.path) {
                continue;
            }
            let file = g.file(&c.path);
            let mut owners: Vec<u32> = Vec::new();
            for s in &c.symbols {
                let id = g.symbol(s, file);
                g.sites[id as usize].push(Site {
                    path: c.path.clone(),
                    line: c.start_line,
                    chunk: c.id.clone(),
                });
                owners.push(id);
            }
            // A chunk cut from inside a large function belongs to that function.
            if owners.is_empty()
                && let Some(name) = c.scope.last().and_then(|s| s.split_once(' ')).map(|(_, n)| n)
            {
                owners.push(g.symbol(name, file));
            }
            let types = c.refs.iter().filter(|r| !c.calls.contains(r)).cloned().collect();
            uses.push(Uses {
                file,
                owners,
                calls: c.calls,
                method_calls: c.method_calls.into_iter().collect(),
                types,
            });
        }
        // Imports first: they decide which definitions a name may link to.
        let resolver = Resolver::new(files.keys().map(String::as_str), go_mods);
        let mut imported: Vec<HashSet<u32>> = vec![HashSet::new(); g.files.len()];
        for (path, entry) in files {
            let Some(lang) = Lang::from_path(std::path::Path::new(path)) else {
                continue;
            };
            let from = g.file_ids[path];
            for spec in &entry.imports {
                let targets = resolver.resolve(path, lang, spec);
                if targets.is_empty() {
                    g.external.entry(from).or_default().push(spec.clone());
                }
                for t in targets {
                    if let Some(&to) = g.file_ids.get(&t) {
                        imported[from as usize].insert(to);
                        g.add_file_edge(from, to, EdgeKind::Import, EdgeKind::Import.weight());
                    }
                }
            }
        }
        for u in uses {
            let Uses {
                file,
                owners,
                calls,
                method_calls,
                types,
            } = u;
            let go = g.files[file as usize].ends_with(".go");
            for (names, kind) in [(calls, EdgeKind::Call), (types, EdgeKind::Type)] {
                for name in names {
                    let targets = g.visible_defs(file, &name, &imported[file as usize]);
                    if targets.is_empty() || targets.len() > MAX_DEF_FANOUT {
                        continue;
                    }
                    // `x.len()` names a method of a value whose type is unknown here: link
                    // it only when the project defines that name once and it is not a
                    // standard container method. (Go selectors are package-qualified and
                    // already filtered by imports.)
                    if !go
                        && method_calls.contains(&name)
                        && (COMMON_METHODS.contains(&name.as_str())
                            || g.symbols_named(&name)
                                .iter()
                                .filter(|&&s| !g.sites[s as usize].is_empty())
                                .count()
                                != 1)
                    {
                        continue;
                    }
                    let share = kind.weight() / targets.len() as f32;
                    for target in targets {
                        let to = g.symbols[target as usize].1;
                        g.add_file_edge(file, to, kind, share);
                        for &o in &owners {
                            if o != target {
                                g.sym_out[o as usize].insert((target, kind));
                                g.sym_in[target as usize].insert((o, kind));
                            }
                        }
                    }
                }
            }
        }
        g
    }

    /// The files defining `sym` that a use in `from` can mean: the same directory (package
    /// or module) or a file `from` imports. Names are not qualified, so without this a call
    /// to `time.Now` would link to any project function named `Now`. Go cannot use a name
    /// from another package without importing it; other languages fall back to an
    /// unambiguous definition (methods are called on values whose type is not imported).
    /// Test files are only linked from test files.
    fn visible_defs(&self, from: u32, name: &str, imported: &HashSet<u32>) -> Vec<u32> {
        let from_path = &self.files[from as usize];
        let from_test = is_test(from_path);
        let all: Vec<u32> = self
            .by_name
            .get(name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&s| !self.sites[s as usize].is_empty())
            .filter(|&s| from_test || !is_test(&self.files[self.symbols[s as usize].1 as usize]))
            .collect();
        let dir = parent(from_path);
        let visible: Vec<u32> = all
            .iter()
            .copied()
            .filter(|&s| {
                let f = self.symbols[s as usize].1;
                parent(&self.files[f as usize]) == dir || imported.contains(&f)
            })
            .collect();
        if !visible.is_empty() || from_path.ends_with(".go") {
            return visible;
        }
        if all.len() <= 2 { all } else { Vec::new() }
    }

    fn file(&mut self, path: &str) -> u32 {
        if let Some(&id) = self.file_ids.get(path) {
            return id;
        }
        let id = self.files.len() as u32;
        self.files.push(path.to_string());
        self.file_ids.insert(path.to_string(), id);
        self.file_out.push(BTreeMap::new());
        id
    }

    fn symbol(&mut self, name: &str, file: u32) -> u32 {
        let key = (name.to_string(), file);
        if let Some(&id) = self.symbol_ids.get(&key) {
            return id;
        }
        let id = self.symbols.len() as u32;
        self.symbols.push(key.clone());
        self.symbol_ids.insert(key, id);
        self.by_name.entry(name.to_string()).or_default().push(id);
        self.sites.push(Vec::new());
        self.sym_out.push(HashSet::new());
        self.sym_in.push(HashSet::new());
        id
    }

    fn add_file_edge(&mut self, from: u32, to: u32, kind: EdgeKind, w: f32) {
        if from != to {
            *self.file_out[from as usize].entry((to, kind)).or_default() += w;
        }
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn file_name(&self, id: u32) -> &str {
        &self.files[id as usize]
    }

    pub fn file_id(&self, path: &str) -> Option<u32> {
        self.file_ids.get(path).copied()
    }

    /// Outgoing file edges: `(file, kind, weight)`.
    pub fn file_edges(&self, file: u32) -> impl Iterator<Item = (u32, EdgeKind, f32)> + '_ {
        self.file_out[file as usize]
            .iter()
            .map(|(&(to, k), &w)| (to, k, w))
    }

    /// The symmetric weighted file graph communities are computed on: for each file, its
    /// neighbours with the summed weight of every edge kind in both directions.
    pub fn undirected(&self) -> Vec<Vec<(u32, f32)>> {
        let mut adj: Vec<BTreeMap<u32, f32>> = vec![BTreeMap::new(); self.files.len()];
        for (from, edges) in self.file_out.iter().enumerate() {
            for (&(to, _), &w) in edges {
                *adj[from].entry(to).or_default() += w;
                *adj[to as usize].entry(from as u32).or_default() += w;
            }
        }
        adj.into_iter().map(|m| m.into_iter().collect()).collect()
    }

    /// Import specifiers of `file` that are not project files.
    pub fn external_imports(&self, file: u32) -> &[String] {
        self.external.get(&file).map_or(&[], Vec::as_slice)
    }

    /// Every symbol node named `name` (one per defining file).
    pub fn symbols_named(&self, name: &str) -> &[u32] {
        self.by_name.get(name).map_or(&[], Vec::as_slice)
    }

    pub fn symbol_name(&self, id: u32) -> &str {
        &self.symbols[id as usize].0
    }

    /// The file defining the symbol.
    pub fn symbol_file(&self, id: u32) -> u32 {
        self.symbols[id as usize].1
    }

    pub fn symbol_count(&self) -> usize {
        self.symbols.len()
    }

    pub fn sites(&self, sym: u32) -> &[Site] {
        &self.sites[sym as usize]
    }

    /// Symbols that call or use `sym`.
    pub fn users(&self, sym: u32) -> impl Iterator<Item = (u32, EdgeKind)> + '_ {
        self.sym_in[sym as usize].iter().copied()
    }

    /// Symbols `sym` calls or uses.
    pub fn uses(&self, sym: u32) -> impl Iterator<Item = (u32, EdgeKind)> + '_ {
        self.sym_out[sym as usize].iter().copied()
    }

    pub fn in_degree(&self, sym: u32) -> usize {
        self.sym_in[sym as usize].len()
    }

    pub fn out_degree(&self, sym: u32) -> usize {
        self.sym_out[sym as usize].len()
    }
}

impl Engine {
    /// The code graph, built from the ledger on first use after each index change.
    pub fn code_graph(&self) -> Result<Arc<CodeGraph>> {
        if let Some(g) = self.graph.read().expect("graph lock").as_ref() {
            return Ok(g.clone());
        }
        let files = self.ledger.files()?;
        let go_mods: Vec<(String, String)> = files
            .keys()
            .filter(|p| *p == "go.mod" || p.ends_with("/go.mod"))
            .filter_map(|p| Some((p.clone(), std::fs::read_to_string(self.root.join(p)).ok()?)))
            .collect();
        let mut chunks = Vec::new();
        self.ledger.for_each_chunk(&mut |c| chunks.push(c))?;
        let g = Arc::new(CodeGraph::build(chunks, &files, go_mods));
        *self.graph.write().expect("graph lock") = Some(g.clone());
        Ok(g)
    }

    /// Every stored community, by id.
    pub fn communities(&self) -> Result<Vec<communities::Community>> {
        Ok(self.ledger.communities()?)
    }
}

/// Container, string and iterator methods every standard library has: a receiver call to
/// one of these is almost never the project's own definition of the same name.
const COMMON_METHODS: &[&str] = &[
    "add",
    "append",
    "as_ref",
    "as_str",
    "clear",
    "clone",
    "close",
    "contains",
    "count",
    "entry",
    "extend",
    "filter",
    "find",
    "first",
    "flush",
    "get",
    "get_mut",
    "insert",
    "into",
    "is_empty",
    "items",
    "iter",
    "join",
    "keys",
    "last",
    "len",
    "map",
    "next",
    "parse",
    "pop",
    "push",
    "read",
    "remove",
    "replace",
    "set",
    "sort",
    "split",
    "to_string",
    "trim",
    "unwrap",
    "update",
    "values",
    "write",
];

/// What one chunk defines and uses, gathered before linking.
struct Uses {
    file: u32,
    owners: Vec<u32>,
    calls: Vec<String>,
    method_calls: HashSet<String>,
    types: Vec<String>,
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Test sources by the usual naming conventions.
fn is_test(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.ends_with("_test.go")
        || name.ends_with("_test.py")
        || name.starts_with("test_")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.ends_with("Test.java")
        || path.contains("/tests/")
        || path.starts_with("tests/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{ChunkOptions, analyze_file};

    pub(crate) fn graph_of(sources: &[(&str, &str)], go_mod: Option<&str>) -> CodeGraph {
        let mut chunks = Vec::new();
        let mut files = BTreeMap::new();
        for (path, src) in sources {
            let lang = Lang::from_path(std::path::Path::new(path)).unwrap();
            let (c, imports) = analyze_file(path, lang, src, ChunkOptions::default());
            files.insert(
                path.to_string(),
                FileEntry {
                    hash: String::new(),
                    size: 0,
                    mtime_ns: 0,
                    chunk_ids: c.iter().map(|c| c.id.clone()).collect(),
                    imports,
                },
            );
            chunks.extend(c);
        }
        let mods = go_mod.map(|m| ("go.mod".to_string(), format!("module {m}\n")));
        CodeGraph::build(chunks, &files, mods)
    }

    #[test]
    fn links_files_by_calls_imports_and_types() {
        let g = graph_of(
            &[
                (
                    "pkg/sidecar/sidecar.go",
                    "package sidecar\n\ntype Client struct{}\n\nfunc LocateBinary(n string) string { return n }\n",
                ),
                (
                    "pkg/rag/retriever.go",
                    "package rag\n\nimport \"example.com/m/pkg/sidecar\"\n\nfunc Run(c *sidecar.Client) { sidecar.LocateBinary(\"x\") }\n",
                ),
            ],
            Some("example.com/m"),
        );
        let from = g.file_id("pkg/rag/retriever.go").unwrap();
        let to = g.file_id("pkg/sidecar/sidecar.go").unwrap();
        let kinds: Vec<EdgeKind> = g
            .file_edges(from)
            .filter(|(t, _, _)| *t == to)
            .map(|(_, k, _)| k)
            .collect();
        assert_eq!(kinds, [EdgeKind::Call, EdgeKind::Import, EdgeKind::Type]);
        assert_eq!(g.file_edges(to).count(), 0);

        let locate = g.symbols_named("LocateBinary")[0];
        let run = g.symbols_named("Run")[0];
        assert_eq!(g.users(locate).collect::<Vec<_>>(), [(run, EdgeKind::Call)]);
        assert_eq!(g.in_degree(locate), 1);
        assert_eq!(g.sites(locate)[0].path, "pkg/sidecar/sidecar.go");

        let und = g.undirected();
        assert_eq!(und[to as usize].len(), 1);
        assert!((und[to as usize][0].1 - 1.9).abs() < 1e-5);
    }

    #[test]
    fn names_link_only_to_visible_definitions() {
        let g = graph_of(
            &[
                (
                    "pkg/clock/clock.go",
                    "package clock\n\nfunc Now() int { return 1 }\n",
                ),
                ("pkg/clock/clock_test.go", "package clock\n\nfunc Helper() {}\n"),
                (
                    "pkg/app/app.go",
                    "package app\n\nimport \"time\"\n\nfunc Run() { time.Now(); Helper() }\n",
                ),
            ],
            Some("example.com/m"),
        );
        // `time.Now` is not the project's `clock.Now`: app does not import clock.
        assert_eq!(g.in_degree(g.symbols_named("Now")[0]), 0);
        assert_eq!(g.in_degree(g.symbols_named("Helper")[0]), 0);
    }

    #[test]
    fn unresolved_imports_are_kept_as_external() {
        let g = graph_of(
            &[(
                "a.go",
                "package a\n\nimport \"fmt\"\n\nfunc F() { fmt.Println() }\n",
            )],
            None,
        );
        assert_eq!(g.external_imports(g.file_id("a.go").unwrap()), ["fmt"]);
    }
}
