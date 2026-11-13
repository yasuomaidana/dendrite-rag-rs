//! The ledger on SQLite: files, chunks, embeddings, metadata and communities.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::atomic::AtomicUsize;
use std::sync::{Mutex, MutexGuard};

use diesel::prelude::*;
use diesel::result::Error as DbError;
use diesel::sqlite::SqliteConnection;

use super::schema::{
    chunks, communities, community_chunks, community_files, concepts, embeddings, files, llm_cache, meta,
};
use super::{SqliteResultExt, import, open, split_key};
use crate::chunk::Chunk;
use crate::graph::communities::Community;
use crate::ledger::{FileEntry, Result, decode_f32, encode_f32};
use crate::store::{LedgerStore, LedgerWrite};

/// One column of the row of `t` with primary key `key`, if there is one.
macro_rules! find {
    ($self:expr, $t:ident, $col:ident, $key:expr) => {
        $self.read(|c| {
            $t::table
                .find($key)
                .select($t::$col)
                .first(c)
                .optional()
                .or_backend()
        })
    };
}

/// `INSERT OR REPLACE` of one row.
macro_rules! put {
    ($conn:expr, $t:ident { $($c:ident : $v:expr),+ }) => {
        diesel::replace_into($t::table)
            .values(($($t::$c.eq($v)),+))
            .execute($conn)?
    };
}

/// `DELETE` of the row of `t` with primary key `key`, or of every row.
macro_rules! remove {
    ($conn:expr, $t:ident) => {
        diesel::delete($t::table).execute($conn)?
    };
    ($conn:expr, $t:ident, $key:expr) => {
        diesel::delete($t::table.find($key)).execute($conn)?
    };
}

/// Read connections beside the write connection.
const READERS: usize = 4;

/// The SQLite ledger: one write connection and a small pool of read connections.
pub struct SqliteLedger {
    writer: Mutex<SqliteConnection>,
    readers: Vec<Mutex<SqliteConnection>>,
    next: AtomicUsize,
}

impl fmt::Debug for SqliteLedger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqliteLedger")
            .field("readers", &self.readers.len())
            .finish_non_exhaustive()
    }
}

impl SqliteLedger {
    /// Opens (creating it if needed) the ledger at `path`. A newer file format is refused, an
    /// older content schema is cleared (embeddings are kept), and the first open next to a
    /// `ledger.redb` copies its embedding and LLM caches.
    pub fn open(path: &Path) -> Result<Self> {
        let mut writer = open::open_writer(path)?;
        import::import_redb(&mut writer, path)?;
        let mut readers = Vec::with_capacity(READERS);
        for _ in 0..READERS {
            readers.push(Mutex::new(open::open_reader(path)?));
        }
        Ok(Self {
            writer: Mutex::new(writer),
            readers,
            next: AtomicUsize::new(0),
        })
    }

    /// A free read connection if there is one, else the next in turn.
    fn reader(&self) -> MutexGuard<'_, SqliteConnection> {
        crate::store::pick_connection(&self.readers, &self.next, "sqlite reader")
    }

    fn read<T>(&self, f: impl FnOnce(&mut SqliteConnection) -> Result<T>) -> Result<T> {
        f(&mut self.reader())
    }
}

impl LedgerStore for SqliteLedger {
    fn files(&self) -> Result<BTreeMap<String, FileEntry>> {
        let rows: Vec<(String, String)> = self.read(|c| {
            files::table
                .select((files::path, files::entry))
                .load(c)
                .or_backend()
        })?;
        rows.into_iter()
            .map(|(path, entry)| Ok((path, serde_json::from_str(&entry)?)))
            .collect()
    }

    fn chunk(&self, id: &str) -> Result<Option<Chunk>> {
        let record: Option<String> = find!(self, chunks, record, id)?;
        Ok(record.map(|s| serde_json::from_str(&s)).transpose()?)
    }

    /// Loads several chunks in one read transaction; missing ids are skipped.
    fn chunks(&self, ids: &[String]) -> Result<Vec<Chunk>> {
        self.read(|c| {
            let records = c
                .transaction::<_, DbError, _>(|tx| {
                    let mut out = Vec::with_capacity(ids.len());
                    for id in ids {
                        let record: Option<String> = chunks::table
                            .find(id)
                            .select(chunks::record)
                            .first(tx)
                            .optional()?;
                        out.extend(record);
                    }
                    Ok(out)
                })
                .or_backend()?;
            records.iter().map(|s| Ok(serde_json::from_str(s)?)).collect()
        })
    }

    /// Visits every stored chunk, in id order.
    fn for_each_chunk(&self, f: &mut dyn FnMut(Chunk)) -> Result<()> {
        self.read(|c| {
            let rows = chunks::table
                .order(chunks::id)
                .select(chunks::record)
                .load_iter::<String, diesel::connection::DefaultLoadingMode>(c)
                .or_backend()?;
            for record in rows {
                f(serde_json::from_str(&record.or_backend()?)?);
            }
            Ok(())
        })
    }

    fn chunk_count(&self) -> Result<u64> {
        let n: i64 = self.read(|c| chunks::table.count().get_result(c).or_backend())?;
        Ok(n as u64)
    }

    fn meta(&self, key: &str) -> Result<Option<String>> {
        find!(self, meta, value, key)
    }

    /// A cached embedding for `key` (`model ‖ content_hash`).
    fn embedding(&self, key: &str) -> Result<Option<Vec<f32>>> {
        Ok(self.embeddings(&[key.to_string()])?.pop().flatten())
    }

    /// Several cached embeddings in one read transaction, in the order of `keys`.
    fn embeddings(&self, keys: &[String]) -> Result<Vec<Option<Vec<f32>>>> {
        self.read(|c| {
            c.transaction::<_, DbError, _>(|tx| {
                let mut out = Vec::with_capacity(keys.len());
                for key in keys {
                    let (model, hash) = split_key(key);
                    let blob: Option<Vec<u8>> = embeddings::table
                        .find((model, hash))
                        .select(embeddings::vec)
                        .first(tx)
                        .optional()?;
                    out.push(blob.map(|b| decode_f32(&b)));
                }
                Ok(out)
            })
            .or_backend()
        })
    }

    fn embedding_count(&self) -> Result<u64> {
        let n: i64 = self.read(|c| embeddings::table.count().get_result(c).or_backend())?;
        Ok(n as u64)
    }

    /// The community id of every file.
    fn community_files(&self) -> Result<BTreeMap<String, u32>> {
        let rows: Vec<(String, i32)> = self.read(|c| {
            community_files::table
                .select((community_files::path, community_files::cid))
                .load(c)
                .or_backend()
        })?;
        Ok(rows.into_iter().map(|(p, id)| (p, id as u32)).collect())
    }

    fn community_of_chunk(&self, chunk: &str) -> Result<Option<u32>> {
        let cid: Option<i32> = find!(self, community_chunks, cid, chunk)?;
        Ok(cid.map(|id| id as u32))
    }

    fn community(&self, id: u32) -> Result<Option<Community>> {
        let record: Option<String> = find!(self, communities, record, id as i32)?;
        Ok(record.map(|s| serde_json::from_str(&s)).transpose()?)
    }

    /// Every community, by id.
    fn communities(&self) -> Result<Vec<Community>> {
        let rows: Vec<String> = self.read(|c| {
            communities::table
                .order(communities::id)
                .select(communities::record)
                .load(c)
                .or_backend()
        })?;
        rows.iter().map(|s| Ok(serde_json::from_str(s)?)).collect()
    }

    /// A cached LLM completion for `(model, prompt_hash)`.
    fn llm_cache(&self, model: &str, prompt_hash: &str) -> Result<Option<String>> {
        self.read(|c| {
            llm_cache::table
                .find((model, prompt_hash))
                .select(llm_cache::value)
                .first(c)
                .optional()
                .or_backend()
        })
    }

    /// Extracted concepts for a file path (JSON list).
    fn concepts(&self, path: &str) -> Result<Option<String>> {
        find!(self, concepts, value, path)
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
        conn.immediate_transaction::<_, DbError, _>(|tx| {
            for op in self.ops {
                apply(tx, op)?;
            }
            Ok(())
        })
        .or_backend()
    }
}

fn apply(conn: &mut SqliteConnection, op: LedgerOp) -> diesel::QueryResult<()> {
    match op {
        LedgerOp::PutFile(path, entry) => {
            put!(
                conn,
                files {
                    path: path,
                    entry: entry
                }
            );
        }
        LedgerOp::RemoveFile(path) => {
            remove!(conn, files, path);
        }
        LedgerOp::PutChunk(id, path, content_hash, record) => {
            put!(
                conn,
                chunks {
                    id: id,
                    path: path,
                    content_hash: content_hash,
                    record: record
                }
            );
        }
        LedgerOp::RemoveChunk(id) => {
            remove!(conn, chunks, id);
        }
        LedgerOp::PutEmbedding(model, hash, vec) => {
            put!(
                conn,
                embeddings {
                    model: model,
                    hash: hash,
                    vec: vec
                }
            );
        }
        LedgerOp::SetMeta(key, value) => {
            put!(
                conn,
                meta {
                    key: key,
                    value: value
                }
            );
        }
        LedgerOp::PutLlmCache(model, prompt_hash, value) => {
            put!(
                conn,
                llm_cache {
                    model: model,
                    prompt_hash: prompt_hash,
                    value: value
                }
            );
        }
        LedgerOp::PutConcepts(path, value) => {
            put!(
                conn,
                concepts {
                    path: path,
                    value: value
                }
            );
        }
        LedgerOp::RemoveConcepts(path) => {
            remove!(conn, concepts, path);
        }
        LedgerOp::PutCommunities(records, by_file, by_chunk) => {
            put_communities(conn, records, by_file, by_chunk)?;
        }
        LedgerOp::ClearIndex => {
            remove!(conn, files);
            remove!(conn, chunks);
            remove!(conn, concepts);
        }
    }
    Ok(())
}

/// Replaces every community, with the community of each file and chunk.
fn put_communities(
    conn: &mut SqliteConnection,
    records: Vec<(u32, String)>,
    by_file: Vec<(String, u32)>,
    by_chunk: Vec<(String, u32)>,
) -> diesel::QueryResult<()> {
    remove!(conn, communities);
    remove!(conn, community_files);
    remove!(conn, community_chunks);
    for (id, record) in records {
        put!(
            conn,
            communities {
                id: id as i32,
                record: record
            }
        );
    }
    for (path, id) in by_file {
        put!(
            conn,
            community_files {
                path: path,
                cid: id as i32
            }
        );
    }
    for (chunk, id) in by_chunk {
        put!(
            conn,
            community_chunks {
                chunk: chunk,
                cid: id as i32
            }
        );
    }
    Ok(())
}
