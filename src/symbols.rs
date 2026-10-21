//! Symbol tables and the reference graph built from chunk definitions and references
//! (`docs/specs/retrieval.md` § Symbol-name ranking and § Context packing).

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::chunk::Chunk;

#[derive(Debug, Default)]
pub struct SymbolIndex {
    /// Symbol name → chunks defining it.
    defs: HashMap<String, Vec<String>>,
    /// Symbol name → chunks referencing it.
    refs: HashMap<String, Vec<String>>,
    chunk_defs: HashMap<String, Vec<String>>,
    chunk_refs: HashMap<String, Vec<String>>,
    chunk_file: HashMap<String, usize>,
    files: Vec<String>,
    file_ids: HashMap<String, usize>,
    /// File → (referenced file → number of references).
    file_edges: Vec<BTreeMap<usize, f32>>,
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

    /// Adds one chunk's definitions and references; its content is not kept.
    pub fn add(&mut self, c: Chunk) {
        let f = match self.file_ids.get(&c.path) {
            Some(&f) => f,
            None => {
                self.files.push(c.path.clone());
                self.file_ids.insert(c.path, self.files.len() - 1);
                self.files.len() - 1
            }
        };
        self.chunk_file.insert(c.id.clone(), f);
        for s in &c.symbols {
            self.defs.entry(s.clone()).or_default().push(c.id.clone());
        }
        for r in &c.refs {
            self.refs.entry(r.clone()).or_default().push(c.id.clone());
        }
        self.chunk_defs.insert(c.id.clone(), c.symbols);
        self.chunk_refs.insert(c.id, c.refs);
    }

    /// Builds the file reference graph once every chunk has been added.
    pub fn finish(&mut self) {
        self.file_edges = vec![BTreeMap::new(); self.files.len()];
        for (chunk, names) in &self.chunk_refs {
            let from = self.chunk_file[chunk];
            for name in names {
                let Some(targets) = self.defs.get(name) else {
                    continue;
                };
                if targets.len() > MAX_REF_FANOUT {
                    continue;
                }
                let share = 1.0 / targets.len() as f32;
                for t in targets {
                    let to = self.chunk_file[t];
                    if to != from {
                        *self.file_edges[from].entry(to).or_default() += share;
                    }
                }
            }
        }
    }

    /// Chunks that define identifier-like tokens of the query, then chunks that reference
    /// them. Plain lowercase words are ignored so natural-language queries do not match
    /// every function named `search` or `index`.
    pub fn rank_query(&self, query: &str, limit: usize) -> Vec<(String, f32)> {
        let mut out: Vec<(String, f32)> = Vec::new();
        let mut seen = HashSet::new();
        let names = identifiers(query);
        for (pass, table, score) in [(0, &self.defs, 2.0), (1, &self.refs, 1.0)] {
            for name in &names {
                if let Some(ids) = table.get(name) {
                    if pass == 1 && ids.len() > MAX_REF_FANOUT {
                        continue;
                    }
                    for id in ids {
                        if seen.insert(id.clone()) {
                            out.push((id.clone(), score));
                        }
                    }
                }
            }
        }
        out.truncate(limit);
        out
    }

    /// Chunks defining `name`.
    pub fn definitions(&self, name: &str) -> &[String] {
        self.defs.get(name).map(Vec::as_slice).unwrap_or_default()
    }

    /// Chunks referencing `name` (calls, type uses).
    pub fn references(&self, name: &str) -> &[String] {
        self.refs.get(name).map(Vec::as_slice).unwrap_or_default()
    }

    /// One-hop neighbours of a chunk: definitions of what it references, then chunks that
    /// reference what it defines.
    pub fn neighbours(&self, chunk: &str, limit: usize) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen: HashSet<&str> = HashSet::from([chunk]);
        let refs = self.chunk_refs.get(chunk).map(Vec::as_slice).unwrap_or_default();
        for name in refs {
            if let Some(ids) = self.defs.get(name).filter(|ids| ids.len() <= 3) {
                for id in ids {
                    if seen.insert(id) {
                        out.push(id.clone());
                    }
                }
            }
        }
        let defs = self.chunk_defs.get(chunk).map(Vec::as_slice).unwrap_or_default();
        for name in defs {
            if let Some(ids) = self.refs.get(name).filter(|ids| ids.len() <= 20) {
                for id in ids {
                    if seen.insert(id) {
                        out.push(id.clone());
                    }
                }
            }
        }
        out.truncate(limit);
        out
    }

    /// Personalized PageRank over the file reference graph, seeded with `seeds`
    /// (`file → weight`). Returns `file → rank`, normalized so the maximum is 1.
    pub fn personalized_pagerank(&self, seeds: &HashMap<String, f32>) -> HashMap<String, f32> {
        let n = self.files.len();
        if n == 0 {
            return HashMap::new();
        }
        let file_ids: HashMap<&str, usize> = self
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.as_str(), i))
            .collect();
        let mut teleport = vec![0f32; n];
        for (f, w) in seeds {
            if let Some(&i) = file_ids.get(f.as_str()) {
                teleport[i] += w.max(0.0);
            }
        }
        let total: f32 = teleport.iter().sum();
        if total == 0.0 {
            return HashMap::new();
        }
        teleport.iter_mut().for_each(|t| *t /= total);
        let out_weight: Vec<f32> = self.file_edges.iter().map(|e| e.values().sum()).collect();
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
                for (&to, &w) in edges {
                    next[to] += DAMPING * rank[from] * w / out_weight[from];
                }
            }
            // Mass from files without outgoing references returns to the seeds.
            for (i, t) in teleport.iter().enumerate() {
                next[i] += DAMPING * dangling * t;
            }
            rank = next;
        }
        let max = rank.iter().cloned().fold(0f32, f32::max);
        self.files
            .iter()
            .zip(rank)
            .filter(|(_, r)| *r > 0.0)
            .map(|(f, r)| (f.clone(), if max > 0.0 { r / max } else { 0.0 }))
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
            "package main\n\nfunc main() { Start(\"dendrite-rag\") }\n",
            ChunkOptions::default(),
        ));
        out
    }

    #[test]
    fn extracts_identifier_shaped_tokens_only() {
        assert_eq!(identifiers("where is LocateBinary defined"), ["LocateBinary"]);
        assert_eq!(
            identifiers("sidecar::Client.start() and max_bytes"),
            ["sidecar::Client.start", "start", "max_bytes"]
        );
        assert_eq!(identifiers("how do we index files"), Vec::<String>::new());
        assert_eq!(identifiers("call run(x)"), ["run"]);
    }

    #[test]
    fn ranks_definitions_before_references() {
        let c = chunks();
        let idx = SymbolIndex::build(c.clone());
        let ranked = idx.rank_query("who calls LocateBinary", 10);
        assert_eq!(ranked[0].0, c[0].id);
        assert_eq!(ranked[0].1, 2.0);
        assert_eq!(ranked[1].0, c[1].id);
    }

    #[test]
    fn neighbours_follow_references_both_ways() {
        let c = chunks();
        let idx = SymbolIndex::build(c.clone());
        // Start's chunk references LocateBinary and is referenced by main.
        let n = idx.neighbours(&c[1].id, 10);
        assert!(n.contains(&c[0].id) && n.contains(&c[2].id), "{n:?}");
    }

    #[test]
    fn pagerank_flows_along_references() {
        let idx = SymbolIndex::build(chunks());
        let pr = idx.personalized_pagerank(&HashMap::from([("cmd/main.go".to_string(), 1.0)]));
        // main → client → locate: rank decays along the chain but reaches it.
        assert!(pr["pkg/sidecar/client.go"] > 0.0);
        assert!(pr["pkg/sidecar/locate.go"] > 0.0);
        assert!(pr["pkg/sidecar/client.go"] > pr["pkg/sidecar/locate.go"] * 0.5);
        assert!(idx.personalized_pagerank(&HashMap::new()).is_empty());
    }
}
