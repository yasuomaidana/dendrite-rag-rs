//! BM25 in SQL over the code_terms postings.

use super::*;

pub(super) struct PgLexical(pub(super) Arc<Pg>);

impl LexicalStore for PgLexical {
    fn search(&self, query: &str, limit: usize) -> tantivy::Result<Vec<(String, f32)>> {
        let terms = code_terms(&sanitize(query));
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self
            .0
            .client()
            .query(BM25, &[&self.0.repo, &terms, &(limit as i64)])
            .map_err(lexical_err)?;
        Ok(rows.into_iter().map(|r| (r.get(0), r.get(1))).collect())
    }

    fn writer(&self) -> tantivy::Result<Box<dyn LexicalWrite + '_>> {
        Ok(Box::new(PgLexicalWrite {
            pg: &self.0,
            ops: Vec::new(),
        }))
    }

    fn len(&self) -> u64 {
        self.0
            .client()
            .query_one("SELECT count(*) FROM rag_docs WHERE repo = $1", &[&self.0.repo])
            .map(|r| r.get::<_, i64>(0) as u64)
            .unwrap_or(0)
    }
}

/// A document: its chunk, path, field lengths and `(term, field, tf)` postings.
struct Doc {
    chunk: String,
    path: String,
    lens: [i32; 3],
    postings: Vec<(String, i16, i32)>,
}

impl Doc {
    /// The fields the Tantivy index has: text (header + content), symbols, path.
    fn of(chunk: &Chunk) -> Self {
        let fields = [chunk.indexed_text(), chunk.symbols.join(" "), chunk.path.clone()];
        let mut lens = [0i32; 3];
        let mut postings = Vec::new();
        for (f, text) in fields.iter().enumerate() {
            let terms = code_terms(text);
            lens[f] = terms.len() as i32;
            let mut tf: BTreeMap<String, i32> = BTreeMap::new();
            for t in terms {
                *tf.entry(t).or_default() += 1;
            }
            postings.extend(tf.into_iter().map(|(t, n)| (t, f as i16, n)));
        }
        Self {
            chunk: chunk.id.clone(),
            path: chunk.path.clone(),
            lens,
            postings,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum LexicalOp {
    Add(Box<Chunk>),
    RemoveFile(String),
    Clear,
}

struct PgLexicalWrite<'a> {
    pg: &'a Pg,
    ops: Vec<LexicalOp>,
}

impl LexicalWrite for PgLexicalWrite<'_> {
    fn add(&mut self, chunk: &Chunk) -> tantivy::Result<()> {
        self.ops.push(LexicalOp::Add(Box::new(chunk.clone())));
        Ok(())
    }

    fn remove_file(&mut self, path: &str) {
        self.ops.push(LexicalOp::RemoveFile(path.to_string()));
    }

    fn clear(&mut self) -> tantivy::Result<()> {
        self.ops.push(LexicalOp::Clear);
        Ok(())
    }

    fn commit(self: Box<Self>) -> tantivy::Result<()> {
        self.pg.apply(SyncBatch::Lexical(self.ops)).map_err(lexical_err)
    }
}

pub(crate) fn apply_lexical(
    tx: &mut Transaction<'_>,
    repo: i32,
    op: LexicalOp,
) -> Result<(), postgres::Error> {
    match op {
        LexicalOp::Add(chunk) => {
            let d = Doc::of(&chunk);
            tx.execute(
                "DELETE FROM rag_postings WHERE repo = $1 AND chunk = $2",
                &[&repo, &d.chunk],
            )?;
            tx.execute(
                "INSERT INTO rag_docs (repo, chunk, path, len_text, len_symbols, len_path)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT (repo, chunk) DO UPDATE SET path = EXCLUDED.path,
                     len_text = EXCLUDED.len_text, len_symbols = EXCLUDED.len_symbols,
                     len_path = EXCLUDED.len_path",
                &[&repo, &d.chunk, &d.path, &d.lens[0], &d.lens[1], &d.lens[2]],
            )?;
            let mut terms = Vec::with_capacity(d.postings.len());
            let mut fields = Vec::with_capacity(d.postings.len());
            let mut tfs = Vec::with_capacity(d.postings.len());
            for (t, f, n) in d.postings {
                terms.push(t);
                fields.push(f);
                tfs.push(n);
            }
            tx.execute(
                "INSERT INTO rag_postings (repo, chunk, term, field, tf)
                 SELECT $1, $2, * FROM unnest($3::text[], $4::smallint[], $5::int[])",
                &[&repo, &d.chunk, &terms, &fields, &tfs],
            )?;
        }
        LexicalOp::RemoveFile(path) => {
            tx.execute(
                "DELETE FROM rag_postings p USING rag_docs d
                 WHERE d.repo = $1 AND d.path = $2 AND p.repo = d.repo AND p.chunk = d.chunk",
                &[&repo, &path],
            )?;
            tx.execute(
                "DELETE FROM rag_docs WHERE repo = $1 AND path = $2",
                &[&repo, &path],
            )?;
        }
        LexicalOp::Clear => {
            tx.execute("DELETE FROM rag_postings WHERE repo = $1", &[&repo])?;
            tx.execute("DELETE FROM rag_docs WHERE repo = $1", &[&repo])?;
        }
    }
    Ok(())
}
