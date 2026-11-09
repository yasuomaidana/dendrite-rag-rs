//! The ledger on PostgreSQL: files, chunks, embeddings, metadata and communities.

use std::collections::HashSet;

use super::dsl::{insert_rows, lookup, remove, rows, upsert};
use super::schema::{
    rag_chunks, rag_communities, rag_community_chunks, rag_community_files, rag_concepts, rag_embeddings,
    rag_files, rag_llm_cache, rag_meta,
};
use super::*;

pub(crate) struct PgLedger(pub(crate) Arc<Pg>);

impl LedgerStore for PgLedger {
    fn files(&self) -> crate::ledger::Result<BTreeMap<String, FileEntry>> {
        let rows: Vec<(String, Value)> = rows!(self.0, rag_files, (rag_files::path, rag_files::entry))?;
        rows.into_iter().map(|(p, v)| Ok((p, decode(v)?))).collect()
    }

    fn chunk(&self, id: &str) -> crate::ledger::Result<Option<Chunk>> {
        let row: Option<Value> = lookup!(self.0, rag_chunks, chunk, id = id)?;
        Ok(row.map(decode).transpose()?)
    }

    fn chunks(&self, ids: &[String]) -> crate::ledger::Result<Vec<Chunk>> {
        let rows: Vec<(String, Value)> = self.0.run(|c| {
            rag_chunks::table
                .filter(rag_chunks::repo.eq(self.0.repo).and(rag_chunks::id.eq_any(ids)))
                .select((rag_chunks::id, rag_chunks::chunk))
                .load(c)
        })?;
        let mut by_id: HashMap<String, Value> = rows.into_iter().collect();
        ids.iter()
            .filter_map(|id| by_id.remove(id))
            .map(|v| Ok(decode(v)?))
            .collect()
    }

    fn for_each_chunk(&self, f: &mut dyn FnMut(Chunk)) -> crate::ledger::Result<()> {
        let rows: Vec<Value> = rows!(self.0, rag_chunks, rag_chunks::chunk, order id)?;
        for r in rows {
            f(decode(r)?);
        }
        Ok(())
    }

    fn chunk_count(&self) -> crate::ledger::Result<u64> {
        let n: i64 = self.0.run(|c| {
            rag_chunks::table
                .filter(rag_chunks::repo.eq(self.0.repo))
                .count()
                .get_result(c)
        })?;
        Ok(n as u64)
    }

    fn meta(&self, key: &str) -> crate::ledger::Result<Option<String>> {
        lookup!(self.0, rag_meta, value, key = key)
    }

    fn embedding(&self, key: &str) -> crate::ledger::Result<Option<Vec<f32>>> {
        Ok(self.embeddings(&[key.to_string()])?.pop().flatten())
    }

    fn embeddings(&self, keys: &[String]) -> crate::ledger::Result<Vec<Option<Vec<f32>>>> {
        // One query per model (in practice one): `hash = ANY(..)` within the model.
        let mut by_model: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for k in keys {
            let (m, h) = split_key(k);
            by_model.entry(m).or_default().push(h);
        }
        let mut found: HashMap<(String, String), Vec<f32>> = HashMap::new();
        for (model, hashes) in by_model {
            let rows: Vec<(String, String, Vector)> = self.0.run(|c| {
                rag_embeddings::table
                    .filter(
                        rag_embeddings::model
                            .eq(model)
                            .and(rag_embeddings::hash.eq_any(hashes)),
                    )
                    .select((rag_embeddings::model, rag_embeddings::hash, rag_embeddings::vec))
                    .load(c)
            })?;
            found.extend(rows.into_iter().map(|(m, h, v)| ((m, h), v.to_vec())));
        }
        Ok(keys
            .iter()
            .map(|k| {
                let (m, h) = split_key(k);
                found.remove(&(m.to_string(), h.to_string()))
            })
            .collect())
    }

    fn embedding_count(&self) -> crate::ledger::Result<u64> {
        let n: i64 = self.0.run(|c| rag_embeddings::table.count().get_result(c))?;
        Ok(n as u64)
    }

    fn community_files(&self) -> crate::ledger::Result<BTreeMap<String, u32>> {
        let rows: Vec<(String, i32)> = rows!(
            self.0,
            rag_community_files,
            (rag_community_files::path, rag_community_files::cid)
        )?;
        Ok(rows.into_iter().map(|(p, c)| (p, c as u32)).collect())
    }

    fn community_of_chunk(&self, chunk: &str) -> crate::ledger::Result<Option<u32>> {
        let cid: Option<i32> = lookup!(self.0, rag_community_chunks, cid, chunk = chunk)?;
        Ok(cid.map(|c| c as u32))
    }

    fn community(&self, id: u32) -> crate::ledger::Result<Option<Community>> {
        let row: Option<Value> = lookup!(self.0, rag_communities, record, id = id as i32)?;
        Ok(row.map(decode).transpose()?)
    }

    fn communities(&self) -> crate::ledger::Result<Vec<Community>> {
        let rows: Vec<Value> = rows!(self.0, rag_communities, rag_communities::record, order id)?;
        rows.into_iter().map(|r| Ok(decode(r)?)).collect()
    }

    fn llm_cache(&self, model: &str, prompt_hash: &str) -> crate::ledger::Result<Option<String>> {
        self.0.run(|c| {
            rag_llm_cache::table
                .find((model, prompt_hash))
                .select(rag_llm_cache::value)
                .first(c)
                .optional()
        })
    }

    fn concepts(&self, path: &str) -> crate::ledger::Result<Option<String>> {
        lookup!(self.0, rag_concepts, concepts, path = path)
    }

    fn batch(&self) -> crate::ledger::Result<Box<dyn LedgerWrite + '_>> {
        Ok(Box::new(PgLedgerWrite {
            pg: &self.0,
            ops: Vec::new(),
        }))
    }
}

fn decode<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, serde_json::Error> {
    serde_json::from_value(v)
}

/// Which of the `(model, hash)` pairs the embedding cache already holds.
pub(crate) fn present_embeddings(
    conn: &mut PgConnection,
    pairs: &[(String, String)],
) -> Result<HashSet<(String, String)>, DbError> {
    let mut by_model: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (m, h) in pairs {
        by_model.entry(m).or_default().push(h);
    }
    let mut present = HashSet::new();
    for (model, hashes) in by_model {
        let rows: Vec<(String, String)> = rag_embeddings::table
            .filter(
                rag_embeddings::model
                    .eq(model)
                    .and(rag_embeddings::hash.eq_any(hashes)),
            )
            .select((rag_embeddings::model, rag_embeddings::hash))
            .load(conn)?;
        present.extend(rows);
    }
    Ok(present)
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum LedgerOp {
    PutFile(String, Value),
    RemoveFile(String),
    PutChunk(String, String, Value),
    RemoveChunk(String),
    PutEmbedding(String, String, Vec<f32>),
    SetMeta(String, String),
    PutLlmCache(String, String, String),
    PutConcepts(String, String),
    RemoveConcepts(String),
    /// Records, `(path, id)` and `(chunk, id)` rows.
    PutCommunities(Vec<(i32, Value)>, Vec<(String, i32)>, Vec<(String, i32)>),
    ClearIndex,
}

impl LedgerOp {
    /// Every community, with the id of each member file and of its chunks.
    pub(crate) fn communities(
        communities: &[Community],
        files: &BTreeMap<String, FileEntry>,
    ) -> crate::ledger::Result<Self> {
        let mut records = Vec::new();
        let mut by_file = Vec::new();
        let mut by_chunk = Vec::new();
        for c in communities {
            records.push((c.id as i32, serde_json::to_value(c)?));
            for f in &c.files {
                by_file.push((f.clone(), c.id as i32));
                for chunk in files.get(f).map_or(&[][..], |e| e.chunk_ids.as_slice()) {
                    by_chunk.push((chunk.clone(), c.id as i32));
                }
            }
        }
        Ok(LedgerOp::PutCommunities(records, by_file, by_chunk))
    }
}

struct PgLedgerWrite<'a> {
    pg: &'a Pg,
    ops: Vec<LedgerOp>,
}

impl PgLedgerWrite<'_> {
    fn queue(&mut self, op: LedgerOp) -> crate::ledger::Result<()> {
        self.ops.push(op);
        Ok(())
    }
}

impl LedgerWrite for PgLedgerWrite<'_> {
    fn put_file(&mut self, path: &str, entry: &FileEntry) -> crate::ledger::Result<()> {
        self.queue(LedgerOp::PutFile(path.to_string(), serde_json::to_value(entry)?))
    }

    fn remove_file(&mut self, path: &str) -> crate::ledger::Result<()> {
        self.queue(LedgerOp::RemoveFile(path.to_string()))
    }

    fn put_chunk(&mut self, chunk: &Chunk) -> crate::ledger::Result<()> {
        self.queue(LedgerOp::PutChunk(
            chunk.id.clone(),
            chunk.path.clone(),
            serde_json::to_value(chunk)?,
        ))
    }

    fn remove_chunk(&mut self, id: &str) -> crate::ledger::Result<()> {
        self.queue(LedgerOp::RemoveChunk(id.to_string()))
    }

    fn put_embedding(&mut self, key: &str, vector: &[f32]) -> crate::ledger::Result<()> {
        let (m, h) = split_key(key);
        self.queue(LedgerOp::PutEmbedding(
            m.to_string(),
            h.to_string(),
            vector.to_vec(),
        ))
    }

    fn put_llm_cache(&mut self, model: &str, prompt_hash: &str, value: &str) -> crate::ledger::Result<()> {
        self.queue(LedgerOp::PutLlmCache(
            model.to_string(),
            prompt_hash.to_string(),
            value.to_string(),
        ))
    }

    fn put_concepts(&mut self, path: &str, json: &str) -> crate::ledger::Result<()> {
        self.queue(LedgerOp::PutConcepts(path.to_string(), json.to_string()))
    }

    fn remove_concepts(&mut self, path: &str) -> crate::ledger::Result<()> {
        self.queue(LedgerOp::RemoveConcepts(path.to_string()))
    }

    fn set_meta(&mut self, key: &str, value: &str) -> crate::ledger::Result<()> {
        self.queue(LedgerOp::SetMeta(key.to_string(), value.to_string()))
    }

    fn put_communities(
        &mut self,
        communities: &[Community],
        files: &BTreeMap<String, FileEntry>,
    ) -> crate::ledger::Result<()> {
        self.queue(LedgerOp::communities(communities, files)?)
    }

    fn clear_index(&mut self) -> crate::ledger::Result<()> {
        self.queue(LedgerOp::ClearIndex)
    }

    fn commit(self: Box<Self>) -> crate::ledger::Result<()> {
        self.pg.apply(SyncBatch::Ledger(self.ops)).map_err(backend)
    }
}

#[derive(Insertable)]
#[diesel(table_name = rag_communities)]
struct NewCommunity {
    repo: i32,
    id: i32,
    record: Value,
}

#[derive(Insertable)]
#[diesel(table_name = rag_community_files)]
struct NewCommunityFile {
    repo: i32,
    path: String,
    cid: i32,
}

#[derive(Insertable)]
#[diesel(table_name = rag_community_chunks)]
struct NewCommunityChunk {
    repo: i32,
    chunk: String,
    cid: i32,
}

pub(crate) fn apply_ledger(tx: &mut PgConnection, repo: i32, op: LedgerOp) -> Result<(), DbError> {
    match op {
        LedgerOp::PutFile(path, entry) => {
            upsert!(tx, rag_files { repo: repo, path: path, entry: entry } on (repo, path) set [entry]);
        }
        LedgerOp::RemoveFile(path) => {
            remove!(tx, rag_files, repo, path = path);
        }
        LedgerOp::PutChunk(id, path, chunk) => {
            upsert!(tx, rag_chunks { repo: repo, id: id, path: path, chunk: chunk } on (repo, id) set [path, chunk]);
        }
        LedgerOp::RemoveChunk(id) => {
            remove!(tx, rag_chunks, repo, id = id);
        }
        LedgerOp::PutEmbedding(model, hash, vec) => {
            upsert!(
                tx,
                rag_embeddings {
                    model: model,
                    hash: hash,
                    vec: Vector::from(vec)
                }
            );
        }
        LedgerOp::SetMeta(key, value) => {
            upsert!(tx, rag_meta { repo: repo, key: key, value: value } on (repo, key) set [value]);
        }
        LedgerOp::PutLlmCache(model, hash, value) => {
            upsert!(tx, rag_llm_cache { model: model, prompt_hash: hash, value: value } on (model, prompt_hash) set [value]);
        }
        LedgerOp::PutConcepts(path, concepts) => {
            upsert!(tx, rag_concepts { repo: repo, path: path, concepts: concepts } on (repo, path) set [concepts]);
        }
        LedgerOp::RemoveConcepts(path) => {
            remove!(tx, rag_concepts, repo, path = path);
        }
        LedgerOp::PutCommunities(records, by_file, by_chunk) => {
            put_communities(tx, repo, records, by_file, by_chunk)?;
        }
        LedgerOp::ClearIndex => {
            remove!(tx, rag_files, repo);
            remove!(tx, rag_chunks, repo);
        }
    }
    Ok(())
}

/// Replaces the repository's communities and the community of each file and chunk.
fn put_communities(
    tx: &mut PgConnection,
    repo: i32,
    records: Vec<(i32, Value)>,
    by_file: Vec<(String, i32)>,
    by_chunk: Vec<(String, i32)>,
) -> Result<(), DbError> {
    remove!(tx, rag_communities, repo);
    remove!(tx, rag_community_files, repo);
    remove!(tx, rag_community_chunks, repo);
    let records: Vec<NewCommunity> = records
        .into_iter()
        .map(|(id, record)| NewCommunity { repo, id, record })
        .collect();
    insert_rows!(tx, rag_communities, records);
    let files: Vec<NewCommunityFile> = by_file
        .into_iter()
        .map(|(path, cid)| NewCommunityFile { repo, path, cid })
        .collect();
    insert_rows!(tx, rag_community_files, files);
    let chunks: Vec<NewCommunityChunk> = by_chunk
        .into_iter()
        .map(|(chunk, cid)| NewCommunityChunk { repo, chunk, cid })
        .collect();
    insert_rows!(tx, rag_community_chunks, chunks, do_nothing);
    Ok(())
}
