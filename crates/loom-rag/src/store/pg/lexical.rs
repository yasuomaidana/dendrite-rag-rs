//! BM25 in SQL over the code_terms postings.

use diesel::upsert::excluded;

use super::schema::{rag_docs, rag_postings};
use super::*;
use crate::bm25::Bm25Params;

pub(super) struct PgLexical(pub(super) Arc<Pg>);

impl LexicalStore for PgLexical {
    fn search(&self, query: &str, limit: usize, params: &Bm25Params) -> tantivy::Result<Vec<(String, f32)>> {
        let terms = code_terms(&sanitize(query));
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        raw::bm25(&mut self.0.conn(), self.0.repo, &terms, limit, params).map_err(lexical_err)
    }

    fn writer(&self) -> tantivy::Result<Box<dyn LexicalWrite + '_>> {
        Ok(Box::new(PgLexicalWrite {
            pg: &self.0,
            ops: Vec::new(),
        }))
    }

    fn len(&self) -> u64 {
        rag_docs::table
            .filter(rag_docs::repo.eq(self.0.repo))
            .count()
            .get_result::<i64>(&mut *self.0.conn())
            .map(|n| n as u64)
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

#[derive(Insertable)]
#[diesel(table_name = rag_postings)]
struct NewPosting<'a> {
    repo: i32,
    term: &'a str,
    field: i16,
    chunk: &'a str,
    tf: i32,
}

pub(crate) fn apply_lexical(tx: &mut PgConnection, repo: i32, op: LexicalOp) -> Result<(), DbError> {
    match op {
        LexicalOp::Add(chunk) => {
            let d = Doc::of(&chunk);
            diesel::delete(
                rag_postings::table.filter(rag_postings::repo.eq(repo).and(rag_postings::chunk.eq(&d.chunk))),
            )
            .execute(tx)?;
            diesel::insert_into(rag_docs::table)
                .values((
                    rag_docs::repo.eq(repo),
                    rag_docs::chunk.eq(&d.chunk),
                    rag_docs::path.eq(&d.path),
                    rag_docs::len_text.eq(d.lens[0]),
                    rag_docs::len_symbols.eq(d.lens[1]),
                    rag_docs::len_path.eq(d.lens[2]),
                ))
                .on_conflict((rag_docs::repo, rag_docs::chunk))
                .do_update()
                .set((
                    rag_docs::path.eq(excluded(rag_docs::path)),
                    rag_docs::len_text.eq(excluded(rag_docs::len_text)),
                    rag_docs::len_symbols.eq(excluded(rag_docs::len_symbols)),
                    rag_docs::len_path.eq(excluded(rag_docs::len_path)),
                ))
                .execute(tx)?;
            let rows: Vec<NewPosting<'_>> = d
                .postings
                .iter()
                .map(|(term, field, tf)| NewPosting {
                    repo,
                    term,
                    field: *field,
                    chunk: &d.chunk,
                    tf: *tf,
                })
                .collect();
            for batch in rows.chunks(INSERT_ROWS) {
                diesel::insert_into(rag_postings::table)
                    .values(batch)
                    .execute(tx)?;
            }
        }
        LexicalOp::RemoveFile(path) => {
            let chunks: Vec<String> = rag_docs::table
                .filter(rag_docs::repo.eq(repo).and(rag_docs::path.eq(&path)))
                .select(rag_docs::chunk)
                .load(tx)?;
            diesel::delete(
                rag_postings::table.filter(
                    rag_postings::repo
                        .eq(repo)
                        .and(rag_postings::chunk.eq_any(&chunks)),
                ),
            )
            .execute(tx)?;
            diesel::delete(rag_docs::table.filter(rag_docs::repo.eq(repo).and(rag_docs::path.eq(path))))
                .execute(tx)?;
        }
        LexicalOp::Clear => {
            diesel::delete(rag_postings::table.filter(rag_postings::repo.eq(repo))).execute(tx)?;
            diesel::delete(rag_docs::table.filter(rag_docs::repo.eq(repo))).execute(tx)?;
        }
    }
    Ok(())
}
