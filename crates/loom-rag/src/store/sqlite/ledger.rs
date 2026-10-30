//! The ledger on SQLite: files, chunks, embeddings, metadata and communities.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicUsize;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, Params, TransactionBehavior, params};

use super::{SqliteResultExt, import, schema, split_key};
use crate::chunk::Chunk;
use crate::graph::communities::Community;
use crate::ledger::{FileEntry, Result, decode_f32, encode_f32};
use crate::store::{LedgerStore, LedgerWrite};

/// Read connections beside the write connection.
const READERS: usize = 4;

/// The SQLite ledger: one write connection and a small pool of read connections.
#[derive(Debug)]
pub struct SqliteLedger {
    writer: Mutex<Connection>,
    readers: Vec<Mutex<Connection>>,
    next: AtomicUsize,
}

impl SqliteLedger {
    /// Opens (creating it if needed) the ledger at `path`. A newer file format is refused, an
    /// older content schema is cleared (embeddings are kept), and the first open next to a
    /// `ledger.redb` copies its embedding and LLM caches.
    pub fn open(path: &Path) -> Result<Self> {
        let mut writer = schema::open_writer(path)?;
        import::import_redb(&mut writer, path)?;
        let mut readers = Vec::with_capacity(READERS);
        for _ in 0..READERS {
            readers.push(Mutex::new(schema::open_reader(path)?));
        }
        Ok(Self {
            writer: Mutex::new(writer),
            readers,
            next: AtomicUsize::new(0),
        })
    }

    /// A free read connection if there is one, else the next in turn.
    fn reader(&self) -> MutexGuard<'_, Connection> {
        crate::store::pick_connection(&self.readers, &self.next, "sqlite reader")
    }

    fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        f(&self.reader())
    }

    /// The one text column of the row `sql` selects with `params`, if there is one.
    fn text(&self, sql: &str, params: impl Params) -> Result<Option<String>> {
        self.read(|c| {
            c.prepare_cached(sql)
                .or_backend()?
                .query_row(params, |r| r.get::<_, String>(0))
                .optional()
                .or_backend()
        })
    }

    /// The number a `SELECT count(*)` returns.
    fn count(&self, sql: &str) -> Result<u64> {
        let n = self.read(|c| c.query_row(sql, [], |r| r.get::<_, i64>(0)).or_backend())?;
        Ok(n as u64)
    }
}

impl LedgerStore for SqliteLedger {
    fn files(&self) -> Result<BTreeMap<String, FileEntry>> {
        self.read(|c| {
            let mut stmt = c.prepare_cached("SELECT path, entry FROM files").or_backend()?;
            let mut rows = stmt.query([]).or_backend()?;
            let mut out: BTreeMap<String, FileEntry> = BTreeMap::new();
            while let Some(row) = rows.next().or_backend()? {
                let entry = row.get::<_, String>(1).or_backend()?;
                out.insert(
                    row.get::<_, String>(0).or_backend()?,
                    serde_json::from_str(&entry)?,
                );
            }
            Ok(out)
        })
    }

    fn chunk(&self, id: &str) -> Result<Option<Chunk>> {
        Ok(
            match self.text("SELECT record FROM chunks WHERE id = ?1", params![id])? {
                Some(s) => Some(serde_json::from_str(&s)?),
                None => None,
            },
        )
    }

    /// Loads several chunks in one read transaction; missing ids are skipped.
    fn chunks(&self, ids: &[String]) -> Result<Vec<Chunk>> {
        self.read(|c| {
            let tx = c.unchecked_transaction().or_backend()?;
            let mut stmt = tx
                .prepare_cached("SELECT record FROM chunks WHERE id = ?1")
                .or_backend()?;
            let mut out: Vec<Chunk> = Vec::with_capacity(ids.len());
            for id in ids {
                let record = stmt
                    .query_row([id], |r| r.get::<_, String>(0))
                    .optional()
                    .or_backend()?;
                if let Some(s) = record {
                    out.push(serde_json::from_str(&s)?);
                }
            }
            Ok(out)
        })
    }

    /// Visits every stored chunk, in id order, one row at a time.
    fn for_each_chunk(&self, f: &mut dyn FnMut(Chunk)) -> Result<()> {
        self.read(|c| {
            let mut stmt = c
                .prepare_cached("SELECT record FROM chunks ORDER BY id")
                .or_backend()?;
            let mut rows = stmt.query([]).or_backend()?;
            while let Some(row) = rows.next().or_backend()? {
                let record = row.get::<_, String>(0).or_backend()?;
                f(serde_json::from_str(&record)?);
            }
            Ok(())
        })
    }

    fn chunk_count(&self) -> Result<u64> {
        self.count("SELECT count(*) FROM chunks")
    }

    fn meta(&self, key: &str) -> Result<Option<String>> {
        self.text("SELECT value FROM meta WHERE key = ?1", params![key])
    }

    /// A cached embedding for `key` (`model ‖ content_hash`).
    fn embedding(&self, key: &str) -> Result<Option<Vec<f32>>> {
        Ok(self.embeddings(&[key.to_string()])?.pop().flatten())
    }

    /// Several cached embeddings in one read transaction, in the order of `keys`.
    fn embeddings(&self, keys: &[String]) -> Result<Vec<Option<Vec<f32>>>> {
        self.read(|c| {
            let tx = c.unchecked_transaction().or_backend()?;
            let mut stmt = tx
                .prepare_cached("SELECT vec FROM embeddings WHERE model = ?1 AND hash = ?2")
                .or_backend()?;
            let mut out = Vec::with_capacity(keys.len());
            for key in keys {
                let (model, hash) = split_key(key);
                let blob = stmt
                    .query_row(params![model, hash], |r| r.get::<_, Vec<u8>>(0))
                    .optional()
                    .or_backend()?;
                out.push(blob.map(|b| decode_f32(&b)));
            }
            Ok(out)
        })
    }

    fn embedding_count(&self) -> Result<u64> {
        self.count("SELECT count(*) FROM embeddings")
    }

    /// The community id of every file.
    fn community_files(&self) -> Result<BTreeMap<String, u32>> {
        self.read(|c| {
            let mut stmt = c
                .prepare_cached("SELECT path, cid FROM community_files")
                .or_backend()?;
            let mut rows = stmt.query([]).or_backend()?;
            let mut out = BTreeMap::new();
            while let Some(row) = rows.next().or_backend()? {
                out.insert(
                    row.get::<_, String>(0).or_backend()?,
                    row.get::<_, u32>(1).or_backend()?,
                );
            }
            Ok(out)
        })
    }

    fn community_of_chunk(&self, chunk: &str) -> Result<Option<u32>> {
        self.read(|c| {
            c.prepare_cached("SELECT cid FROM community_chunks WHERE chunk = ?1")
                .or_backend()?
                .query_row([chunk], |r| r.get::<_, u32>(0))
                .optional()
                .or_backend()
        })
    }

    fn community(&self, id: u32) -> Result<Option<Community>> {
        Ok(
            match self.text("SELECT record FROM communities WHERE id = ?1", params![id])? {
                Some(s) => Some(serde_json::from_str(&s)?),
                None => None,
            },
        )
    }

    /// Every community, by id.
    fn communities(&self) -> Result<Vec<Community>> {
        self.read(|c| {
            let mut stmt = c
                .prepare_cached("SELECT record FROM communities ORDER BY id")
                .or_backend()?;
            let mut rows = stmt.query([]).or_backend()?;
            let mut out: Vec<Community> = Vec::new();
            while let Some(row) = rows.next().or_backend()? {
                out.push(serde_json::from_str(&row.get::<_, String>(0).or_backend()?)?);
            }
            Ok(out)
        })
    }

    /// A cached LLM completion for `(model, prompt_hash)`.
    fn llm_cache(&self, model: &str, prompt_hash: &str) -> Result<Option<String>> {
        self.text(
            "SELECT value FROM llm_cache WHERE model = ?1 AND prompt_hash = ?2",
            params![model, prompt_hash],
        )
    }

    /// Extracted concepts for a file path (JSON list).
    fn concepts(&self, path: &str) -> Result<Option<String>> {
        self.text("SELECT value FROM concepts WHERE path = ?1", params![path])
    }

    /// Starts a write batch; nothing is visible until it is committed.
    fn batch(&self) -> Result<Box<dyn LedgerWrite + '_>> {
        Ok(Box::new(SqliteLedgerWrite {
            ledger: self,
            ops: Vec::new(),
        }))
    }
}

/// One buffered change, applied in order when the batch commits.
enum LedgerOp {
    PutFile(String, String),
    RemoveFile(String),
    /// Id, path, content hash and the JSON record.
    PutChunk(String, String, String, String),
    RemoveChunk(String),
    PutEmbedding(String, String, Vec<u8>),
    SetMeta(String, String),
    PutLlmCache(String, String, String),
    PutConcepts(String, String),
    RemoveConcepts(String),
    /// Records, `(path, id)` and `(chunk, id)` rows.
    PutCommunities(Vec<(u32, String)>, Vec<(String, u32)>, Vec<(String, u32)>),
    ClearIndex,
}

impl LedgerOp {
    /// Every community, with the id of each member file and of its chunks.
    fn communities(communities: &[Community], files: &BTreeMap<String, FileEntry>) -> Result<Self> {
        let mut records = Vec::new();
        let mut by_file = Vec::new();
        let mut by_chunk = Vec::new();
        for c in communities {
            records.push((c.id, serde_json::to_string(c)?));
            for f in &c.files {
                by_file.push((f.clone(), c.id));
                for chunk in files.get(f).map_or(&[][..], |e| e.chunk_ids.as_slice()) {
                    by_chunk.push((chunk.clone(), c.id));
                }
            }
        }
        Ok(Self::PutCommunities(records, by_file, by_chunk))
    }
}

/// One atomic set of ledger changes, buffered until [`LedgerWrite::commit`].
struct SqliteLedgerWrite<'a> {
    ledger: &'a SqliteLedger,
    ops: Vec<LedgerOp>,
}

impl SqliteLedgerWrite<'_> {
    fn queue(&mut self, op: LedgerOp) -> Result<()> {
        self.ops.push(op);
        Ok(())
    }
}

impl LedgerWrite for SqliteLedgerWrite<'_> {
    fn put_file(&mut self, path: &str, entry: &FileEntry) -> Result<()> {
        self.queue(LedgerOp::PutFile(path.to_string(), serde_json::to_string(entry)?))
    }

    fn remove_file(&mut self, path: &str) -> Result<()> {
        self.queue(LedgerOp::RemoveFile(path.to_string()))
    }

    fn put_chunk(&mut self, chunk: &Chunk) -> Result<()> {
        self.queue(LedgerOp::PutChunk(
            chunk.id.clone(),
            chunk.path.clone(),
            chunk.content_hash.clone(),
            serde_json::to_string(chunk)?,
        ))
    }

    fn remove_chunk(&mut self, id: &str) -> Result<()> {
        self.queue(LedgerOp::RemoveChunk(id.to_string()))
    }

    fn put_embedding(&mut self, key: &str, vector: &[f32]) -> Result<()> {
        let (model, hash) = split_key(key);
        self.queue(LedgerOp::PutEmbedding(
            model.to_string(),
            hash.to_string(),
            encode_f32(vector),
        ))
    }

    fn put_llm_cache(&mut self, model: &str, prompt_hash: &str, value: &str) -> Result<()> {
        self.queue(LedgerOp::PutLlmCache(
            model.to_string(),
            prompt_hash.to_string(),
            value.to_string(),
        ))
    }

    fn put_concepts(&mut self, path: &str, json: &str) -> Result<()> {
        self.queue(LedgerOp::PutConcepts(path.to_string(), json.to_string()))
    }

    fn remove_concepts(&mut self, path: &str) -> Result<()> {
        self.queue(LedgerOp::RemoveConcepts(path.to_string()))
    }

    fn set_meta(&mut self, key: &str, value: &str) -> Result<()> {
        self.queue(LedgerOp::SetMeta(key.to_string(), value.to_string()))
    }

    /// Replaces every community, with the id of each member file and of its chunks.
    fn put_communities(
        &mut self,
        communities: &[Community],
        files: &BTreeMap<String, FileEntry>,
    ) -> Result<()> {
        self.queue(LedgerOp::communities(communities, files)?)
    }

    /// Removes every file and chunk (embeddings and LLM cache are kept: they are keyed by content).
    fn clear_index(&mut self) -> Result<()> {
        self.queue(LedgerOp::ClearIndex)
    }

    fn commit(self: Box<Self>) -> Result<()> {
        if self.ops.is_empty() {
            return Ok(());
        }
        let mut conn = self.ledger.writer.lock().expect("sqlite writer lock");
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .or_backend()?;
        for op in self.ops {
            apply(&tx, op).or_backend()?;
        }
        tx.commit().or_backend()
    }
}

/// Runs one cached statement.
fn run(conn: &Connection, sql: &str, params: impl Params) -> rusqlite::Result<()> {
    conn.prepare_cached(sql)?.execute(params)?;
    Ok(())
}

fn apply(conn: &Connection, op: LedgerOp) -> rusqlite::Result<()> {
    match op {
        LedgerOp::PutFile(path, entry) => run(
            conn,
            "INSERT OR REPLACE INTO files (path, entry) VALUES (?1, ?2)",
            params![path, entry],
        ),
        LedgerOp::RemoveFile(path) => run(conn, "DELETE FROM files WHERE path = ?1", params![path]),
        LedgerOp::PutChunk(id, path, content_hash, record) => run(
            conn,
            "INSERT OR REPLACE INTO chunks (id, path, content_hash, record) VALUES (?1, ?2, ?3, ?4)",
            params![id, path, content_hash, record],
        ),
        LedgerOp::RemoveChunk(id) => run(conn, "DELETE FROM chunks WHERE id = ?1", params![id]),
        LedgerOp::PutEmbedding(model, hash, vec) => run(
            conn,
            "INSERT OR REPLACE INTO embeddings (model, hash, vec) VALUES (?1, ?2, ?3)",
            params![model, hash, vec],
        ),
        LedgerOp::SetMeta(key, value) => run(
            conn,
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        ),
        LedgerOp::PutLlmCache(model, prompt_hash, value) => run(
            conn,
            "INSERT OR REPLACE INTO llm_cache (model, prompt_hash, value) VALUES (?1, ?2, ?3)",
            params![model, prompt_hash, value],
        ),
        LedgerOp::PutConcepts(path, value) => run(
            conn,
            "INSERT OR REPLACE INTO concepts (path, value) VALUES (?1, ?2)",
            params![path, value],
        ),
        LedgerOp::RemoveConcepts(path) => run(conn, "DELETE FROM concepts WHERE path = ?1", params![path]),
        LedgerOp::PutCommunities(records, by_file, by_chunk) => {
            conn.execute_batch(
                "DELETE FROM communities; DELETE FROM community_files; DELETE FROM community_chunks;",
            )?;
            for (id, record) in records {
                run(
                    conn,
                    "INSERT OR REPLACE INTO communities (id, record) VALUES (?1, ?2)",
                    params![id, record],
                )?;
            }
            for (path, id) in by_file {
                run(
                    conn,
                    "INSERT OR REPLACE INTO community_files (path, cid) VALUES (?1, ?2)",
                    params![path, id],
                )?;
            }
            for (chunk, id) in by_chunk {
                run(
                    conn,
                    "INSERT OR REPLACE INTO community_chunks (chunk, cid) VALUES (?1, ?2)",
                    params![chunk, id],
                )?;
            }
            Ok(())
        }
        LedgerOp::ClearIndex => {
            conn.execute_batch("DELETE FROM files; DELETE FROM chunks; DELETE FROM concepts;")
        }
    }
}
