//! Index runs: walk, hash, Merkle diff, chunk, and update the lexical index and ledger.

use super::*;

fn merkle<'a>(entries: impl Iterator<Item = (&'a str, &'a str)>) -> MerkleNode {
    MerkleNode::from_paths("", entries.map(|(p, h)| (p.to_string(), h.as_bytes().to_vec())))
}

/// The files a delta path stands for: the path itself, or every file under it when the
/// diff reports a whole directory.
fn expand<'a>(delta_path: &str, files: impl Iterator<Item = &'a str>) -> Vec<String> {
    let p = delta_path.trim_start_matches('/');
    let prefix = format!("{p}/");
    files
        .filter(|f| *f == p || p.is_empty() || f.starts_with(&prefix))
        .map(str::to_string)
        .collect()
}

/// Files chunked together before their chunks are written: enough for the parallel
/// chunker to stay busy, few enough that a group's chunks stay small in memory.
pub(super) const CHUNK_GROUP_FILES: usize = 512;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Engine {
    /// Brings the index up to date. Unchanged files are skipped by `(size, mtime)` and then
    /// by content hash; the changed set comes from a Merkle diff of `path → hash`.
    /// `full` discards the index and rebuilds it.
    pub fn index(&mut self, full: bool) -> Result<IndexReport> {
        self.index_with(full, None)
    }

    /// Like [`Engine::index`], but stops the embedding phase once `embed_budget` has elapsed
    /// since the call started. The lexical index is always completed; chunks left without
    /// vectors are counted in `embed_pending` and embedded by later runs.
    pub fn index_with(
        &mut self,
        full: bool,
        embed_budget: Option<std::time::Duration>,
    ) -> Result<IndexReport> {
        self.index_focused(full, embed_budget, None)
    }

    /// Like [`Engine::index_with`]; under a budget, the chunks `focus` (the question about to
    /// be asked) matches lexically are embedded before the rest of the backlog, so dense
    /// retrieval covers them first.
    pub fn index_focused(
        &mut self,
        full: bool,
        embed_budget: Option<std::time::Duration>,
        focus: Option<&str>,
    ) -> Result<IndexReport> {
        let started = Instant::now();
        let deadline = embed_budget.map(|b| started + b);
        let walked = Instant::now();
        let files = walk(&self.root, &self.cfg.walk)?;
        let old = if full {
            BTreeMap::new()
        } else {
            self.ledger.files()?
        };

        // Hash every file, reusing the stored hash when size and mtime are unchanged.
        let scanned: Vec<(SourceFile, String)> = files
            .into_par_iter()
            .filter_map(|f| {
                if let Some(e) = old
                    .get(&f.path)
                    .filter(|e| e.size == f.size && e.mtime_ns == f.mtime_ns)
                {
                    return Some((f, e.hash.clone()));
                }
                let bytes = std::fs::read(&f.abs).ok()?;
                let hash = blake3::hash(&bytes).to_hex().to_string();
                Some((f, hash))
            })
            .collect();

        let old_tree = merkle(old.iter().map(|(p, e)| (p.as_str(), e.hash.as_str())));
        let new_tree = merkle(scanned.iter().map(|(f, h)| (f.path.as_str(), h.as_str())));
        let new_paths: BTreeSet<&str> = scanned.iter().map(|(f, _)| f.path.as_str()).collect();
        let delta = diff(&old_tree, &new_tree);

        let mut removed: BTreeSet<String> = BTreeSet::new();
        let mut added: BTreeSet<String> = BTreeSet::new();
        for p in delta.removed.iter().chain(&delta.changed) {
            removed.extend(expand(p, old.keys().map(String::as_str)));
        }
        for p in delta.added.iter().chain(&delta.changed) {
            added.extend(expand(p, new_paths.iter().copied()));
        }

        let walk_ms = walked.elapsed().as_millis() as u64;
        let mut report = IndexReport {
            open_ms: std::mem::take(&mut self.open_ms),
            walk_ms,
            files_scanned: scanned.len(),
            files_changed: added.len(),
            files_removed: removed.iter().filter(|p| !new_paths.contains(p.as_str())).count(),
            ..IndexReport::default()
        };

        // Lexical index first: replaying a half-finished run is idempotent, because files
        // are removed by path before their chunks are added again.
        let mut lw = self.lexical.writer()?;
        // An empty file table (first run, or a ledger cleared by a schema change) must not
        // keep the lexical documents of an earlier run: they would be duplicated.
        if full || old.is_empty() {
            lw.clear()?;
        }
        for p in &removed {
            lw.remove_file(p);
        }
        let mut batch = self.ledger.batch()?;
        if full {
            batch.clear_index()?;
        }
        // Removals before additions: a modified file's unchanged chunks keep their ids.
        // A modified file loses its entry too, so after a crash before its group commits
        // the next run sees it as added even if its content went back to the old hash.
        for p in &removed {
            if let Some(e) = old.get(p) {
                for id in &e.chunk_ids {
                    batch.remove_chunk(id)?;
                    report.chunks_removed += 1;
                }
            }
            batch.remove_file(p)?;
            if !new_paths.contains(p.as_str()) {
                batch.remove_concepts(p)?;
            }
        }
        // Chunk the added and modified files a group at a time and commit each group to both
        // stores, lexical index first, before the next is chunked: holding every chunk of a
        // cold index at once, and one ledger transaction over all of them, was its peak
        // memory (DRAG-44). A crash between groups leaves the committed groups current;
        // the next run re-chunks the rest, removing each file from the lexical index by
        // path first so a group committed only there is not duplicated.
        let opts = self.cfg.chunk;
        let contexts = self.cfg.chunk_contexts.clone();
        let changed_files: Vec<&(SourceFile, String)> =
            scanned.iter().filter(|(f, _)| added.contains(&f.path)).collect();
        let groups: Vec<_> = changed_files.chunks(CHUNK_GROUP_FILES).collect();
        for (i, group) in groups.iter().enumerate() {
            if i > 0 {
                lw.commit()?;
                batch.commit()?;
                lw = self.lexical.writer()?;
                batch = self.ledger.batch()?;
            }
            let fresh: Vec<(&SourceFile, &String, Vec<Chunk>, Vec<String>)> = group
                .par_iter()
                .map(|(f, h)| {
                    let (mut chunks, imports): (Vec<Chunk>, Vec<String>) = read_text(&f.abs)
                        .map_or_else(Default::default, |src| analyze_file(&f.path, f.lang, &src, opts));
                    if let Some(ctx) = &contexts {
                        for c in &mut chunks {
                            c.context = ctx.get(&c.id).cloned();
                        }
                    }
                    (f, h, chunks, imports)
                })
                .collect();
            for (f, h, chunks, imports) in fresh {
                lw.remove_file(&f.path);
                for c in &chunks {
                    lw.add(c)?;
                    batch.put_chunk(c)?;
                }
                report.chunks_added += chunks.len();
                let entry = FileEntry {
                    hash: h.clone(),
                    size: f.size,
                    mtime_ns: f.mtime_ns,
                    chunk_ids: chunks.iter().map(|c| c.id.clone()).collect(),
                    imports,
                };
                batch.put_file(&f.path, &entry)?;
            }
        }
        // Lexical index first: the ledger commit below is what makes the run count.
        lw.commit()?;

        // Unchanged content with a new mtime: refresh the entry so the next run skips it.
        for (f, h) in &scanned {
            if added.contains(&f.path) {
                continue;
            }
            if let Some(e) = old
                .get(&f.path)
                .filter(|e| e.size != f.size || e.mtime_ns != f.mtime_ns)
            {
                let entry = FileEntry {
                    hash: h.clone(),
                    size: f.size,
                    mtime_ns: f.mtime_ns,
                    ..e.clone()
                };
                batch.put_file(&f.path, &entry)?;
            }
        }
        batch.set_meta("merkle_root", &hex(&new_tree.hash))?;
        batch.set_meta("last_indexed", &now_secs().to_string())?;
        batch.commit()?;

        let changed = full || report.files_changed > 0 || report.files_removed > 0;
        if changed {
            *self.symbols.write().expect("symbols lock") = None;
            *self.graph.write().expect("graph lock") = None;
        }
        // Communities follow the graph; a ledger from before communities gets them now.
        let mut communities = self.ledger.communities()?.len();
        if changed || (communities == 0 && !scanned.is_empty()) {
            let t = Instant::now();
            communities = self.update_communities(&mut report)?;
            report.graph_ms = t.elapsed().as_millis() as u64;
        }
        report.communities = communities;
        self.enrich_concepts(&scanned, &added, &mut report)?;
        let embedding = Instant::now();
        self.sync_vectors(changed, deadline, focus, &mut report)?;
        report.embed_ms = embedding.elapsed().as_millis() as u64;
        report.duration_ms = started.elapsed().as_millis() as u64;
        Ok(report)
    }
}
