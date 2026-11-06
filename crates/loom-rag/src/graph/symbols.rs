//! Symbol tables and the reference graph built from chunk definitions and references
//! (`docs/specs/retrieval.md` § Symbol-name ranking and § Context packing).
//!
//! Chunk ids, file paths and symbol names are interned once as `u32` handles: at 50k
//! chunks the tables would otherwise repeat the same strings millions of times.

use std::collections::{HashMap, HashSet};

use crate::chunk::Chunk;
use crate::corpus::Corpus;

/// Interns strings as dense `u32` handles.
#[derive(Debug, Default)]
struct Interner {
    names: Vec<Box<str>>,
    ids: HashMap<Box<str>, u32>,
}

impl Interner {
    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.ids.get(s) {
            return id;
        }
        let id = self.names.len() as u32;
        self.names.push(s.into());
        self.ids.insert(s.into(), id);
        id
    }

    fn get(&self, s: &str) -> Option<u32> {
        self.ids.get(s).copied()
    }

    fn name(&self, id: u32) -> &str {
        &self.names[id as usize]
    }

    fn len(&self) -> usize {
        self.names.len()
    }
}

#[derive(Debug, Default)]
pub struct SymbolIndex {
    chunks: Interner,
    files: Interner,
    symbols: Interner,
    /// Symbol → chunks defining it.
    defs: Vec<Vec<u32>>,
    /// Symbol → chunks referencing it.
    refs: Vec<Vec<u32>>,
    /// Chunk → (file, defined symbols, referenced symbols).
    chunk_file: Vec<u32>,
    chunk_defs: Vec<Box<[u32]>>,
    chunk_refs: Vec<Box<[u32]>>,
    chunk_corpus: Vec<Corpus>,
    /// File → (referenced file, weight), built by [`SymbolIndex::finish`].
    file_edges: Vec<Vec<(u32, f32)>>,
}

/// Names referenced by more chunks than this are too common to link through (`new`,
/// `String`, `Error`), so expansion and the file graph ignore them.
const MAX_REF_FANOUT: usize = 200;

impl SymbolIndex {
    pub fn build(chunks: impl IntoIterator<Item = Chunk>) -> Self {
        let mut idx = SymbolIndex::default();
        for c in chunks {
            idx.add(c);
        }
        idx.finish();
        idx
    }

    fn symbol(&mut self, name: &str) -> u32 {
        let id = self.symbols.intern(name);
        if id as usize == self.defs.len() {
            self.defs.push(Vec::new());
            self.refs.push(Vec::new());
        }
        id
    }

    /// Adds one chunk's definitions and references; its content is not kept.
    pub fn add(&mut self, c: Chunk) {
        let chunk = self.chunks.intern(&c.id);
        if chunk as usize != self.chunk_file.len() {
            return; // already added
        }
        self.chunk_file.push(self.files.intern(&c.path));
        self.chunk_corpus.push(Corpus::of(&c.path, c.lang));
        let defs: Box<[u32]> = c.symbols.iter().map(|s| self.symbol(s)).collect();
        let refs: Box<[u32]> = c.refs.iter().map(|s| self.symbol(s)).collect();
        for &s in defs.iter() {
            self.defs[s as usize].push(chunk);
        }
        for &s in refs.iter() {
            self.refs[s as usize].push(chunk);
        }
        self.chunk_defs.push(defs);
        self.chunk_refs.push(refs);
    }

    /// Builds the file reference graph once every chunk has been added.
    pub fn finish(&mut self) {
        let mut edges: Vec<HashMap<u32, f32>> = vec![HashMap::new(); self.files.len()];
        for (chunk, names) in self.chunk_refs.iter().enumerate() {
            let from = self.chunk_file[chunk];
            for &name in names.iter() {
                let targets = &self.defs[name as usize];
                if targets.is_empty() || targets.len() > MAX_REF_FANOUT {
                    continue;
                }
                let share = 1.0 / targets.len() as f32;
                for &t in targets {
                    let to = self.chunk_file[t as usize];
                    if to != from {
                        *edges[from as usize].entry(to).or_default() += share;
                    }
                }
            }
        }
        self.file_edges = edges
            .into_iter()
            .map(|m| {
                let mut v: Vec<(u32, f32)> = m.into_iter().collect();
                v.sort_by_key(|e| e.0);
                v
            })
            .collect();
    }

    /// The corpus of a chunk added to the index.
    pub fn corpus_of(&self, chunk_id: &str) -> Option<Corpus> {
        self.chunks.get(chunk_id).map(|c| self.chunk_corpus[c as usize])
    }

    fn chunk_ids(&self, ids: &[u32]) -> Vec<String> {
        ids.iter().map(|&c| self.chunks.name(c).to_string()).collect()
    }

    /// Chunks that define identifier-like tokens of the query, then chunks that reference
    /// them. Plain lowercase words are ignored so natural-language queries do not match
    /// every function named `search` or `index`.
    pub fn rank_query(&self, query: &str, limit: usize) -> Vec<(String, f32)> {
        let mut out: Vec<(String, f32)> = Vec::new();
        let mut seen = HashSet::new();
        let names: Vec<u32> = identifiers(query)
            .iter()
            .filter_map(|n| self.symbols.get(n))
            .collect();
        for (table, score) in [(&self.defs, 2.0), (&self.refs, 1.0)] {
            for &name in &names {
                let ids = &table[name as usize];
                if score < 2.0 && ids.len() > MAX_REF_FANOUT {
                    continue;
                }
                for &id in ids {
                    if seen.insert(id) {
                        out.push((self.chunks.name(id).to_string(), score));
                    }
                }
            }
        }
        out.truncate(limit);
        out
    }

    /// The chunks `table` lists for the symbol `name`.
    fn chunks_of(&self, name: &str, table: &[Vec<u32>]) -> Vec<String> {
        self.symbols
            .get(name)
            .map_or_else(Vec::new, |s| self.chunk_ids(&table[s as usize]))
    }

    /// Chunks defining `name`.
    pub fn definitions(&self, name: &str) -> Vec<String> {
        self.chunks_of(name, &self.defs)
    }

    /// Chunks referencing `name` (calls, type uses).
    pub fn references(&self, name: &str) -> Vec<String> {
        self.chunks_of(name, &self.refs)
    }

    /// One-hop neighbours of a chunk: definitions of what it references, then chunks that
    /// reference what it defines.
    pub fn neighbours(&self, chunk: &str, limit: usize) -> Vec<String> {
        let Some(c) = self.chunks.get(chunk) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut seen: HashSet<u32> = HashSet::from([c]);
        // What the chunk references, through their definitions; what it defines, through their references.
        for (names, table, cap) in [
            (&self.chunk_refs, &self.defs, 3),
            (&self.chunk_defs, &self.refs, 20),
        ] {
            for &name in names[c as usize].iter() {
                let ids = &table[name as usize];
                if ids.len() <= cap {
                    out.extend(ids.iter().filter(|&&id| seen.insert(id)));
                }
            }
        }
        out.truncate(limit);
        self.chunk_ids(&out)
    }

    /// Personalized PageRank over the file reference graph, seeded with `seeds`
    /// (`file → weight`). Returns `file → rank`, normalized so the maximum is 1.
    pub fn personalized_pagerank(&self, seeds: &HashMap<String, f32>) -> HashMap<String, f32> {
        let n = self.files.len();
        if n == 0 {
            return HashMap::new();
        }
        let mut teleport = vec![0f32; n];
        for (f, w) in seeds {
            if let Some(i) = self.files.get(f) {
                teleport[i as usize] += w.max(0.0);
            }
        }
        let total: f32 = teleport.iter().sum();
        if total == 0.0 {
            return HashMap::new();
        }
        teleport.iter_mut().for_each(|t| *t /= total);
        let out_weight: Vec<f32> = self
            .file_edges
            .iter()
            .map(|e| e.iter().map(|(_, w)| w).sum())
            .collect();
        const DAMPING: f32 = 0.85;
        let mut rank = teleport.clone();
        for _ in 0..20 {
            let mut next: Vec<f32> = teleport.iter().map(|t| (1.0 - DAMPING) * t).collect();
            let mut dangling = 0.0;
            for (from, edges) in self.file_edges.iter().enumerate() {
                if out_weight[from] == 0.0 {
                    dangling += rank[from];
                    continue;
                }
                for &(to, w) in edges {
                    next[to as usize] += DAMPING * rank[from] * w / out_weight[from];
                }
            }
            // Mass from files without outgoing references returns to the seeds.
            for (i, t) in teleport.iter().enumerate() {
                next[i] += DAMPING * dangling * t;
            }
            rank = next;
        }
        let max = rank.iter().cloned().fold(0f32, f32::max);
        rank.into_iter()
            .enumerate()
            .filter(|(_, r)| *r > 0.0)
            .map(|(i, r)| {
                (
                    self.files.name(i as u32).to_string(),
                    if max > 0.0 { r / max } else { 0.0 },
                )
            })
            .collect()
    }
}

/// Identifier-shaped tokens of a query: names with `_`, `::`, `.`, an inner capital, a
/// leading capital, or a call `name(`. Qualified names also yield their last segment.
pub fn identifiers(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in query.split(|c: char| c.is_whitespace() || ",;\"'`[]{}<>".contains(c)) {
        let call = raw.contains('(');
        let token = raw
            .split('(')
            .next()
            .unwrap_or_default()
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
        if token.is_empty() {
            continue;
        }
        let qualified = token.contains("::") || token.contains('.');
        let inner_capital = token.chars().skip(1).any(char::is_uppercase);
        let leading_capital = token.chars().next().is_some_and(char::is_uppercase) && token.len() > 1;
        if !(call || qualified || token.contains('_') || inner_capital || leading_capital) {
            continue;
        }
        out.push(token.to_string());
        if qualified && let Some(last) = token.rsplit(['.', ':']).next().filter(|s| !s.is_empty()) {
            out.push(last.to_string());
        }
    }
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{ChunkOptions, chunk_file};
    use crate::lang::Lang;
    use rstest::{fixture, rstest};

    #[fixture]
    fn chunks() -> Vec<Chunk> {
        let mut out = chunk_file(
            "pkg/sidecar/locate.go",
            Lang::Go,
            "package sidecar\n\nfunc LocateBinary(name string) string { return name }\n",
            ChunkOptions::default(),
        );
        out.extend(chunk_file(
            "pkg/sidecar/client.go",
            Lang::Go,
            "package sidecar\n\nfunc Start(name string) { path := LocateBinary(name); run(path) }\n",
            ChunkOptions::default(),
        ));
        out.extend(chunk_file(
            "cmd/main.go",
            Lang::Go,
            "package main\n\nfunc main() { Start(\"helper\") }\n",
            ChunkOptions::default(),
        ));
        out
    }

    #[rstest]
    #[case("where is LocateBinary defined", &["LocateBinary"][..])]
    #[case(
        "sidecar::Client.start() and max_bytes",
        &["sidecar::Client.start", "start", "max_bytes"][..]
    )]
    #[case("how do we index files", &[][..])]
    #[case("call run(x)", &["run"][..])]
    fn extracts_identifier_shaped_tokens_only(#[case] query: &str, #[case] expected: &[&str]) {
        assert_eq!(identifiers(query), expected);
    }

    #[rstest]
    fn ranks_definitions_before_references(chunks: Vec<Chunk>) {
        let c = chunks;
        let idx = SymbolIndex::build(c.clone());
        let ranked = idx.rank_query("who calls LocateBinary", 10);
        assert_eq!(ranked[0].0, c[0].id);
        assert_eq!(ranked[0].1, 2.0);
        assert_eq!(ranked[1].0, c[1].id);
    }

    #[rstest]
    fn neighbours_follow_references_both_ways(chunks: Vec<Chunk>) {
        let c = chunks;
        let idx = SymbolIndex::build(c.clone());
        // Start's chunk references LocateBinary and is referenced by main.
        let n = idx.neighbours(&c[1].id, 10);
        assert!(n.contains(&c[0].id) && n.contains(&c[2].id), "{n:?}");
    }

    #[rstest]
    fn pagerank_flows_along_references(chunks: Vec<Chunk>) {
        let idx = SymbolIndex::build(chunks);
        let pr = idx.personalized_pagerank(&HashMap::from([("cmd/main.go".to_string(), 1.0)]));
        // main → client → locate: rank decays along the chain but reaches it.
        assert!(pr["pkg/sidecar/client.go"] > 0.0);
        assert!(pr["pkg/sidecar/locate.go"] > 0.0);
        assert!(pr["pkg/sidecar/client.go"] > pr["pkg/sidecar/locate.go"] * 0.5);
        assert!(idx.personalized_pagerank(&HashMap::new()).is_empty());
    }
}
