//! The ledger on PostgreSQL: files, chunks, embeddings, metadata and communities.

use super::*;

pub(crate) struct PgLedger(pub(crate) Arc<Pg>);

impl LedgerStore for PgLedger {
    fn files(&self) -> crate::ledger::Result<BTreeMap<String, FileEntry>> {
        let rows = self
            .0
            .client()
            .query(
                "SELECT path, entry FROM rag_files WHERE repo = $1",
                &[&self.0.repo],
            )
            .map_err(backend)?;
        rows.into_iter()
            .map(|r| Ok((r.get(0), serde_json::from_value(r.get::<_, Value>(1))?)))
            .collect()
    }

    fn chunk(&self, id: &str) -> crate::ledger::Result<Option<Chunk>> {
        let row = self
            .0
            .client()
            .query_opt(
                "SELECT chunk FROM rag_chunks WHERE repo = $1 AND id = $2",
                &[&self.0.repo, &id],
            )
            .map_err(backend)?;
        Ok(match row {
            Some(r) => Some(serde_json::from_value(r.get::<_, Value>(0))?),
            None => None,
        })
    }

    fn chunks(&self, ids: &[String]) -> crate::ledger::Result<Vec<Chunk>> {
        let rows = self
            .0
            .client()
            .query(
                "SELECT id, chunk FROM rag_chunks WHERE repo = $1 AND id = ANY($2)",
                &[&self.0.repo, &ids],
            )
            .map_err(backend)?;
        let mut by_id: HashMap<String, Value> = rows.into_iter().map(|r| (r.get(0), r.get(1))).collect();
        ids.iter()
            .filter_map(|id| by_id.remove(id))
            .map(|v| Ok(serde_json::from_value(v)?))
            .collect()
    }

    fn for_each_chunk(&self, f: &mut dyn FnMut(Chunk)) -> crate::ledger::Result<()> {
        let rows = self
            .0
            .client()
            .query(
                "SELECT chunk FROM rag_chunks WHERE repo = $1 ORDER BY id",
                &[&self.0.repo],
            )
            .map_err(backend)?;
        for r in rows {
            f(serde_json::from_value(r.get::<_, Value>(0))?);
        }
        Ok(())
    }

    fn chunk_count(&self) -> crate::ledger::Result<u64> {
        let n: i64 = self
            .0
            .client()
            .query_one("SELECT count(*) FROM rag_chunks WHERE repo = $1", &[&self.0.repo])
            .map_err(backend)?
            .get(0);
        Ok(n as u64)
    }

    fn meta(&self, key: &str) -> crate::ledger::Result<Option<String>> {
        self.0.text_opt(
            "SELECT value FROM rag_meta WHERE repo = $1 AND key = $2",
            &[&self.0.repo, &key],
        )
    }

    fn embedding(&self, key: &str) -> crate::ledger::Result<Option<Vec<f32>>> {
        Ok(self.embeddings(&[key.to_string()])?.pop().flatten())
    }

    fn embeddings(&self, keys: &[String]) -> crate::ledger::Result<Vec<Option<Vec<f32>>>> {
        let (models, hashes): (Vec<&str>, Vec<&str>) = keys.iter().map(|k| split_key(k)).unzip();
        let rows = self
            .0
            .client()
            .query(
                "SELECT e.model, e.hash, e.vec
                 FROM unnest($1::text[], $2::text[]) AS k(model, hash)
                 JOIN rag_embeddings e ON e.model = k.model AND e.hash = k.hash",
                &[&models, &hashes],
            )
            .map_err(backend)?;
        let mut found: HashMap<(String, String), Vec<f32>> = rows
            .into_iter()
            .map(|r| ((r.get(0), r.get(1)), r.get::<_, Vector>(2).to_vec()))
            .collect();
        Ok(keys
            .iter()
            .map(|k| {
                let (m, h) = split_key(k);
                found.remove(&(m.to_string(), h.to_string()))
            })
            .collect())
    }

    fn embedding_count(&self) -> crate::ledger::Result<u64> {
        let n: i64 = self
            .0
            .client()
            .query_one("SELECT count(*) FROM rag_embeddings", &[])
            .map_err(backend)?
            .get(0);
        Ok(n as u64)
    }

    fn community_files(&self) -> crate::ledger::Result<BTreeMap<String, u32>> {
        let rows = self
            .0
            .client()
            .query(
                "SELECT path, cid FROM rag_community_files WHERE repo = $1",
                &[&self.0.repo],
            )
            .map_err(backend)?;
        Ok(rows
            .into_iter()
            .map(|r| (r.get(0), r.get::<_, i32>(1) as u32))
            .collect())
    }

    fn community_of_chunk(&self, chunk: &str) -> crate::ledger::Result<Option<u32>> {
        Ok(self
            .0
            .client()
            .query_opt(
                "SELECT cid FROM rag_community_chunks WHERE repo = $1 AND chunk = $2",
                &[&self.0.repo, &chunk],
            )
            .map_err(backend)?
            .map(|r| r.get::<_, i32>(0) as u32))
    }

    fn community(&self, id: u32) -> crate::ledger::Result<Option<Community>> {
        let row = self
            .0
            .client()
            .query_opt(
                "SELECT record FROM rag_communities WHERE repo = $1 AND id = $2",
                &[&self.0.repo, &(id as i32)],
            )
            .map_err(backend)?;
        Ok(match row {
            Some(r) => Some(serde_json::from_value(r.get::<_, Value>(0))?),
            None => None,
        })
    }

    fn communities(&self) -> crate::ledger::Result<Vec<Community>> {
        let rows = self
            .0
            .client()
            .query(
                "SELECT record FROM rag_communities WHERE repo = $1 ORDER BY id",
                &[&self.0.repo],
            )
            .map_err(backend)?;
        rows.into_iter()
            .map(|r| Ok(serde_json::from_value(r.get::<_, Value>(0))?))
            .collect()
    }

    fn llm_cache(&self, model: &str, prompt_hash: &str) -> crate::ledger::Result<Option<String>> {
        self.0.text_opt(
            "SELECT value FROM rag_llm_cache WHERE model = $1 AND prompt_hash = $2",
            &[&model, &prompt_hash],
        )
    }

    fn concepts(&self, path: &str) -> crate::ledger::Result<Option<String>> {
        self.0.text_opt(
            "SELECT concepts FROM rag_concepts WHERE repo = $1 AND path = $2",
            &[&self.0.repo, &path],
        )
    }

    fn batch(&self) -> crate::ledger::Result<Box<dyn LedgerWrite + '_>> {
        Ok(Box::new(PgLedgerWrite {
            pg: &self.0,
            ops: Vec::new(),
        }))
    }
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

pub(crate) fn apply_ledger(tx: &mut Transaction<'_>, repo: i32, op: LedgerOp) -> Result<(), postgres::Error> {
    match op {
        LedgerOp::PutFile(path, entry) => tx.execute(
            "INSERT INTO rag_files (repo, path, entry) VALUES ($1, $2, $3)
             ON CONFLICT (repo, path) DO UPDATE SET entry = EXCLUDED.entry",
            &[&repo, &path, &entry],
        )?,
        LedgerOp::RemoveFile(path) => tx.execute(
            "DELETE FROM rag_files WHERE repo = $1 AND path = $2",
            &[&repo, &path],
        )?,
        LedgerOp::PutChunk(id, path, chunk) => tx.execute(
            "INSERT INTO rag_chunks (repo, id, path, chunk) VALUES ($1, $2, $3, $4)
             ON CONFLICT (repo, id) DO UPDATE SET path = EXCLUDED.path, chunk = EXCLUDED.chunk",
            &[&repo, &id, &path, &chunk],
        )?,
        LedgerOp::RemoveChunk(id) => tx.execute(
            "DELETE FROM rag_chunks WHERE repo = $1 AND id = $2",
            &[&repo, &id],
        )?,
        LedgerOp::PutEmbedding(model, hash, vec) => tx.execute(
            "INSERT INTO rag_embeddings (model, hash, vec) VALUES ($1, $2, $3)
             ON CONFLICT (model, hash) DO NOTHING",
            &[&model, &hash, &Vector::from(vec)],
        )?,
        LedgerOp::SetMeta(key, value) => tx.execute(
            "INSERT INTO rag_meta (repo, key, value) VALUES ($1, $2, $3)
             ON CONFLICT (repo, key) DO UPDATE SET value = EXCLUDED.value",
            &[&repo, &key, &value],
        )?,
        LedgerOp::PutLlmCache(model, hash, value) => tx.execute(
            "INSERT INTO rag_llm_cache (model, prompt_hash, value) VALUES ($1, $2, $3)
             ON CONFLICT (model, prompt_hash) DO UPDATE SET value = EXCLUDED.value",
            &[&model, &hash, &value],
        )?,
        LedgerOp::PutConcepts(path, concepts) => tx.execute(
            "INSERT INTO rag_concepts (repo, path, concepts) VALUES ($1, $2, $3)
             ON CONFLICT (repo, path) DO UPDATE SET concepts = EXCLUDED.concepts",
            &[&repo, &path, &concepts],
        )?,
        LedgerOp::RemoveConcepts(path) => tx.execute(
            "DELETE FROM rag_concepts WHERE repo = $1 AND path = $2",
            &[&repo, &path],
        )?,
        LedgerOp::PutCommunities(records, by_file, by_chunk) => {
            for table in ["rag_communities", "rag_community_files", "rag_community_chunks"] {
                tx.execute(&format!("DELETE FROM {table} WHERE repo = $1"), &[&repo])?;
            }
            for (id, record) in records {
                tx.execute(
                    "INSERT INTO rag_communities (repo, id, record) VALUES ($1, $2, $3)",
                    &[&repo, &id, &record],
                )?;
            }
            let (paths, ids): (Vec<String>, Vec<i32>) = by_file.into_iter().unzip();
            tx.execute(
                "INSERT INTO rag_community_files (repo, path, cid)
                 SELECT $1, * FROM unnest($2::text[], $3::int[])",
                &[&repo, &paths, &ids],
            )?;
            let (chunks, ids): (Vec<String>, Vec<i32>) = by_chunk.into_iter().unzip();
            tx.execute(
                "INSERT INTO rag_community_chunks (repo, chunk, cid)
                 SELECT $1, * FROM unnest($2::text[], $3::int[]) ON CONFLICT DO NOTHING",
                &[&repo, &chunks, &ids],
            )?
        }
        LedgerOp::ClearIndex => {
            tx.execute("DELETE FROM rag_files WHERE repo = $1", &[&repo])?;
            tx.execute("DELETE FROM rag_chunks WHERE repo = $1", &[&repo])?
        }
    };
    Ok(())
}
