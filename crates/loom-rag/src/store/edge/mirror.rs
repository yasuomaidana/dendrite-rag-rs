//! The lexical index and vectors of edge mode: local reads, writes mirrored to the
//! shared tier through the journal.

use super::*;
use crate::bm25::Bm25Params;

// ---------------------------------------------------------------------------------------------
// Lexical

pub(super) struct EdgeLexical {
    pub(super) local: Box<dyn LexicalStore>,
    pub(super) edge: Arc<Edge>,
}

impl LexicalStore for EdgeLexical {
    fn search(&self, query: &str, limit: usize, params: &Bm25Params) -> tantivy::Result<Vec<(String, f32)>> {
        self.local.search(query, limit, params)
    }

    fn writer(&self) -> tantivy::Result<Box<dyn LexicalWrite + '_>> {
        Ok(Box::new(EdgeLexicalWrite {
            local: self.local.writer()?,
            ops: Vec::new(),
            edge: &self.edge,
        }))
    }

    fn len(&self) -> u64 {
        self.local.len()
    }
}

struct EdgeLexicalWrite<'a> {
    local: Box<dyn LexicalWrite + 'a>,
    ops: Vec<LexicalOp>,
    edge: &'a Edge,
}

impl LexicalWrite for EdgeLexicalWrite<'_> {
    fn add(&mut self, chunk: &Chunk) -> tantivy::Result<()> {
        self.local.add(chunk)?;
        self.ops.push(LexicalOp::Add(Box::new(chunk.clone())));
        Ok(())
    }

    fn remove_file(&mut self, path: &str) {
        self.local.remove_file(path);
        self.ops.push(LexicalOp::RemoveFile(path.to_string()));
    }

    fn clear(&mut self) -> tantivy::Result<()> {
        self.local.clear()?;
        self.ops.push(LexicalOp::Clear);
        Ok(())
    }

    fn commit(self: Box<Self>) -> tantivy::Result<()> {
        let EdgeLexicalWrite { local, ops, edge } = *self;
        local.commit()?;
        if !ops.is_empty() {
            edge.send(SyncBatch::Lexical(ops))
                .map_err(|e| tantivy::TantivyError::SystemError(format!("edge journal: {e}")))?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Vectors

pub(super) struct EdgeVectors {
    pub(super) local: Box<dyn VectorIndex>,
    pub(super) pending: Mutex<Vec<VectorOp>>,
    pub(super) edge: Arc<Edge>,
}

impl EdgeVectors {
    pub(super) fn push(&self, op: VectorOp) {
        self.pending.lock().expect("edge vectors lock").push(op);
    }
}

impl VectorIndex for EdgeVectors {
    fn upsert(&mut self, id: &str, tag: &str, vector: &[f32]) {
        self.local.upsert(id, tag, vector);
        self.push(VectorOp::Upsert(id.to_string(), tag.to_string(), vector.to_vec()));
    }

    fn remove(&mut self, id: &str) -> bool {
        self.push(VectorOp::Remove(id.to_string()));
        self.local.remove(id)
    }

    fn contains(&self, id: &str) -> bool {
        self.local.contains(id)
    }

    fn search(&self, query: &[f32], k: usize) -> Vec<(String, f32)> {
        self.local.search(query, k)
    }

    fn len(&self) -> usize {
        self.local.len()
    }

    fn dim(&self) -> usize {
        self.local.dim()
    }

    fn ids(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        self.local.ids()
    }

    fn tag(&self, id: &str) -> Option<&str> {
        self.local.tag(id)
    }

    fn clear(&mut self) {
        self.local.clear();
        self.push(VectorOp::Clear);
    }

    fn persist(&self) -> std::io::Result<()> {
        self.local.persist()?;
        let ops = std::mem::take(&mut *self.pending.lock().expect("edge vectors lock"));
        if ops.is_empty() {
            return Ok(());
        }
        self.edge.send(SyncBatch::Vectors(ops))
    }
}
