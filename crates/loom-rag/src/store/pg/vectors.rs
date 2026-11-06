//! Vectors in pgvector, written through the shared tier's batches.

use super::*;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum VectorOp {
    Upsert(String, String, Vec<f32>),
    Remove(String),
    Clear,
}

/// The repository's vectors live in `rag_vectors`; an in-memory flat index mirrors them
/// for the scan (repository-sized), and changes reach Postgres on `persist`.
pub(super) struct PgVectors {
    pub(super) pg: Arc<Pg>,
    pub(super) flat: FlatIndex,
    pub(super) pending: Mutex<Vec<VectorOp>>,
}

impl PgVectors {
    fn push(&self, op: VectorOp) {
        self.pending.lock().expect("pending vectors lock").push(op);
    }
}

impl VectorIndex for PgVectors {
    fn upsert(&mut self, id: &str, tag: &str, vector: &[f32]) {
        self.flat.upsert(id, tag, vector);
        self.push(VectorOp::Upsert(id.to_string(), tag.to_string(), vector.to_vec()));
    }

    fn remove(&mut self, id: &str) -> bool {
        self.push(VectorOp::Remove(id.to_string()));
        self.flat.remove(id)
    }

    fn contains(&self, id: &str) -> bool {
        self.flat.contains(id)
    }

    fn search(&self, query: &[f32], k: usize) -> Vec<(String, f32)> {
        self.flat.search(query, k)
    }

    fn len(&self) -> usize {
        self.flat.len()
    }

    fn dim(&self) -> usize {
        self.flat.dim()
    }

    fn ids(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        self.flat.ids()
    }

    fn tag(&self, id: &str) -> Option<&str> {
        self.flat.tag(id)
    }

    fn clear(&mut self) {
        self.flat.clear();
        self.push(VectorOp::Clear);
    }

    fn persist(&self) -> std::io::Result<()> {
        let ops = std::mem::take(&mut *self.pending.lock().expect("pending vectors lock"));
        if ops.is_empty() {
            return Ok(());
        }
        self.pg
            .apply(SyncBatch::Vectors(ops))
            .map_err(|e| std::io::Error::other(format!("postgres: {e}")))
    }
}

pub(crate) fn apply_vector(tx: &mut Transaction<'_>, repo: i32, op: VectorOp) -> Result<(), postgres::Error> {
    match op {
        VectorOp::Upsert(id, tag, vec) => tx.execute(
            "INSERT INTO rag_vectors (repo, id, tag, vec) VALUES ($1, $2, $3, $4)
             ON CONFLICT (repo, id) DO UPDATE SET tag = EXCLUDED.tag, vec = EXCLUDED.vec",
            &[&repo, &id, &tag, &Vector::from(vec)],
        )?,
        VectorOp::Remove(id) => tx.execute(
            "DELETE FROM rag_vectors WHERE repo = $1 AND id = $2",
            &[&repo, &id],
        )?,
        VectorOp::Clear => tx.execute("DELETE FROM rag_vectors WHERE repo = $1", &[&repo])?,
    };
    Ok(())
}
