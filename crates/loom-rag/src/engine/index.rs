//! Index runs: walk, hash, Merkle diff, chunk, and update the lexical index and ledger.

use super::*;
use crate::store::{LedgerWrite, LexicalWrite};

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

/// A walked file with its content hash. `hash` is missing for a file that could not be read and was never
/// indexed.
struct HashedFile {
    file: SourceFile,
    hash: Option<String>,
    unreadable: bool,
}

/// Hashes `file`, or reuses the ledger's hash when its size and mtime are unchanged; when it cannot be read,
/// the ledger's hash is kept so the file is not mistaken for a removed one.
fn hash_file(file: SourceFile, old: &BTreeMap<String, FileEntry>) -> HashedFile {
    let stored = old.get(&file.path);
    if let Some(e) = stored.filter(|e| e.size == file.size && e.mtime_ns == file.mtime_ns) {
        let hash = Some(e.hash.clone());
        return HashedFile {
            file,
            hash,
            unreadable: false,
        };
    }
    match std::fs::read(&file.abs) {
        Ok(bytes) => {
            let hash = Some(blake3::hash(&bytes).to_hex().to_string());
            HashedFile {
                file,
                hash,
                unreadable: false,
            }
        }
        Err(_) => {
            let hash = stored.map(|e| e.hash.clone());
            HashedFile {
                file,
                hash,
                unreadable: true,
            }
        }
    }
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

    /// Like [`Engine::index_focused`], but accepts an explicit token budget override.
    pub fn index_focused_budgeted(
        &mut self,
        full: bool,
        embed_budget: Option<std::time::Duration>,
        embed_budget_tokens: Option<usize>,
        focus: Option<&str>,
    ) -> Result<IndexReport> {
        let prev_tokens = self.cfg.embed.budget_tokens;
        if embed_budget_tokens.is_some() {
            self.cfg.embed.budget_tokens = embed_budget_tokens;
        }
        let res = self.index_focused(full, embed_budget, focus);
        self.cfg.embed.budget_tokens = prev_tokens;
        res
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
        let plan = self.plan_index(full)?;
        let mut report = plan.report(std::mem::take(&mut self.open_ms));
        self.write_index(&plan, full, &mut report)?;
        self.refresh_derived(&plan, full, deadline, focus, &mut report)?;
        report.duration_ms = started.elapsed().as_millis() as u64;
        Ok(report)
    }

    /// Walks and hashes the project and diffs it against the ledger.
    fn plan_index(&self, full: bool) -> Result<Plan> {
        let walked = Instant::now();
        let files = walk(&self.root, &self.cfg.walk)?;
        let old = if full {
            BTreeMap::new()
        } else {
            self.ledger.files()?
        };

        // Hash every file, reusing the stored hash when size and mtime are unchanged. A file that cannot be
        // read keeps the hash it had, so it counts as unchanged and its chunks stay indexed; one never seen
        // before is left out. Either way the run reports it.
        let hashed: Vec<HashedFile> = files.into_par_iter().map(|f| hash_file(f, &old)).collect();
        let unreadable: Vec<String> = hashed
            .iter()
            .filter(|h| h.unreadable)
            .map(|h| h.file.path.clone())
            .collect();
        let scanned: Vec<(SourceFile, String)> = hashed
            .into_iter()
            .filter_map(|h| Some((h.file, h.hash?)))
            .collect();
        let new_paths: BTreeSet<String> = scanned.iter().map(|(f, _)| f.path.clone()).collect();

        let old_tree = merkle(old.iter().map(|(p, e)| (p.as_str(), e.hash.as_str())));
        let new_tree = merkle(scanned.iter().map(|(f, h)| (f.path.as_str(), h.as_str())));
        let delta = diff(&old_tree, &new_tree);
        let mut removed: BTreeSet<String> = BTreeSet::new();
        let mut added: BTreeSet<String> = BTreeSet::new();
        for p in delta.removed.iter().chain(&delta.changed) {
            removed.extend(expand(p, old.keys().map(String::as_str)));
        }
        for p in delta.added.iter().chain(&delta.changed) {
            added.extend(expand(p, new_paths.iter().map(String::as_str)));
        }
        Ok(Plan {
            old,
            scanned,
            unreadable,
            new_paths,
            removed,
            added,
            new_tree,
            walk_ms: walked.elapsed().as_millis() as u64,
        })
    }

    /// Updates the lexical index and the ledger from `plan`, lexical index first: replaying a
    /// half-finished run is idempotent, because files are removed by path before their chunks
    /// are added again.
    fn write_index(&self, plan: &Plan, full: bool, report: &mut IndexReport) -> Result<()> {
        let mut lw = self.lexical.writer()?;
        // An empty file table (first run, or a ledger cleared by a schema change) must not
        // keep the lexical documents of an earlier run: they would be duplicated.
        if full || plan.old.is_empty() {
            lw.clear()?;
        }
        for p in &plan.removed {
            lw.remove_file(p);
        }
        let mut batch = self.ledger.batch()?;
        if full {
            batch.clear_index()?;
        }
        plan.drop_removed(&mut *batch, report)?;
        let (lw, batch) = self.write_groups(plan, lw, batch, report)?;
        // Lexical index first: the ledger commit below is what makes the run count.
        lw.commit()?;
        plan.commit_ledger(batch)
    }

    /// Chunks the added and modified files a group at a time and commits each group to both
    /// stores, lexical index first, before the next is chunked: holding every chunk of a
    /// cold index at once, and one ledger transaction over all of them, was its peak
    /// memory (DRAG-44). A crash between groups leaves the committed groups current;
    /// the next run re-chunks the rest, removing each file from the lexical index by
    /// path first so a group committed only there is not duplicated.
    fn write_groups<'a>(
        &'a self,
        plan: &Plan,
        mut lw: Box<dyn LexicalWrite + 'a>,
        mut batch: Box<dyn LedgerWrite + 'a>,
        report: &mut IndexReport,
    ) -> Result<(Box<dyn LexicalWrite + 'a>, Box<dyn LedgerWrite + 'a>)> {
        let changed_files: Vec<&(SourceFile, String)> = plan
            .scanned
            .iter()
            .filter(|(f, _)| plan.added.contains(&f.path))
            .collect();
        for (i, group) in changed_files.chunks(CHUNK_GROUP_FILES).enumerate() {
            if i > 0 {
                lw.commit()?;
                batch.commit()?;
                lw = self.lexical.writer()?;
                batch = self.ledger.batch()?;
            }
            for (f, h, chunks, imports) in self.chunk_group(group) {
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
        Ok((lw, batch))
    }

    /// Reads and chunks one group of files in parallel.
    fn chunk_group<'g>(&self, group: &[&'g (SourceFile, String)]) -> Vec<Fresh<'g>> {
        let opts = self.cfg.chunk;
        let contexts = &self.cfg.chunk_contexts;
        group
            .par_iter()
            .map(|&(f, h)| {
                let (mut chunks, imports): (Vec<Chunk>, Vec<String>) = read_text(&f.abs)
                    .map_or_else(Default::default, |src| analyze_file(&f.path, f.lang, &src, opts));
                if let Some(ctx) = contexts {
                    for c in &mut chunks {
                        c.context = ctx.get(&c.id).cloned();
                    }
                }
                (f, h, chunks, imports)
            })
            .collect()
    }

    /// What follows a write: stale caches dropped, communities, concepts and vectors brought up to date.
    fn refresh_derived(
        &mut self,
        plan: &Plan,
        full: bool,
        deadline: Option<Instant>,
        focus: Option<&str>,
        report: &mut IndexReport,
    ) -> Result<()> {
        let changed = full || report.files_changed > 0 || report.files_removed > 0;
        if changed {
            *self.symbols.write().expect("symbols lock") = None;
            *self.graph.write().expect("graph lock") = None;
        }
        // Communities follow the graph; a ledger from before communities gets them now.
        let mut communities = self.ledger.communities()?.len();
        if changed || (communities == 0 && !plan.scanned.is_empty()) {
            let t = Instant::now();
            communities = self.update_communities(report)?;
            report.graph_ms = t.elapsed().as_millis() as u64;
        }
        report.communities = communities;
        self.enrich_concepts(&plan.scanned, &plan.added, report)?;
        let embedding = Instant::now();
        self.sync_vectors(changed, deadline, focus, report)?;
        report.embed_ms = embedding.elapsed().as_millis() as u64;
        Ok(())
    }
}

/// A chunked file: the file, its hash, its chunks and the imports found in it.
type Fresh<'a> = (&'a SourceFile, &'a String, Vec<Chunk>, Vec<String>);

/// The walked project compared with the ledger: what to remove, add and refresh.
struct Plan {
    old: BTreeMap<String, FileEntry>,
    scanned: Vec<(SourceFile, String)>,
    unreadable: Vec<String>,
    new_paths: BTreeSet<String>,
    removed: BTreeSet<String>,
    added: BTreeSet<String>,
    new_tree: MerkleNode,
    walk_ms: u64,
}

impl Plan {
    fn report(&self, open_ms: u64) -> IndexReport {
        IndexReport {
            open_ms,
            walk_ms: self.walk_ms,
            files_scanned: self.scanned.len(),
            files_unreadable: self.unreadable.clone(),
            files_changed: self.added.len(),
            files_removed: self
                .removed
                .iter()
                .filter(|p| !self.new_paths.contains(p.as_str()))
                .count(),
            ..IndexReport::default()
        }
    }

    /// Removals before additions: a modified file's unchanged chunks keep their ids. A modified
    /// file loses its entry too, so after a crash before its group commits the next run sees it
    /// as added even if its content went back to the old hash.
    fn drop_removed(&self, batch: &mut dyn LedgerWrite, report: &mut IndexReport) -> Result<()> {
        for p in &self.removed {
            if let Some(e) = self.old.get(p) {
                for id in &e.chunk_ids {
                    batch.remove_chunk(id)?;
                    report.chunks_removed += 1;
                }
            }
            batch.remove_file(p)?;
            if !self.new_paths.contains(p.as_str()) {
                batch.remove_concepts(p)?;
            }
        }
        Ok(())
    }

    /// Refreshes the unchanged entries, stamps the Merkle root and the time, and commits the batch.
    fn commit_ledger(&self, mut batch: Box<dyn LedgerWrite + '_>) -> Result<()> {
        self.refresh_unchanged(&mut *batch)?;
        batch.set_meta("merkle_root", &hex(&self.new_tree.hash))?;
        batch.set_meta("last_indexed", &now_secs().to_string())?;
        batch.commit()?;
        Ok(())
    }

    /// Unchanged content with a new mtime: refreshes the entry so the next run skips it. An unreadable file
    /// keeps its old size and mtime, or the next run would trust the stale hash it carries.
    fn refresh_unchanged(&self, batch: &mut dyn LedgerWrite) -> Result<()> {
        let unreadable: BTreeSet<&str> = self.unreadable.iter().map(String::as_str).collect();
        for (f, h) in &self.scanned {
            if self.added.contains(&f.path) || unreadable.contains(f.path.as_str()) {
                continue;
            }
            if let Some(e) = self
                .old
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
        Ok(())
    }
}
