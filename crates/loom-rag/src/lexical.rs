//! Tantivy BM25 index with a code-aware tokenizer (`docs/specs/retrieval.md` § Lexical index).

use std::path::Path;

use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{
    Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value,
};
use tantivy::tokenizer::{TextAnalyzer, Token, TokenStream, Tokenizer};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term, doc};

use crate::chunk::Chunk;
use crate::store::{LexicalStore, LexicalWrite};

pub(crate) const TOKENIZER: &str = "code";
const WRITER_HEAP: usize = 50_000_000;

/// Splits identifiers on `_`, case changes and letter/digit boundaries, lowercases every
/// part and also emits the whole identifier: `parseHTTPResponse2` → `parsehttpresponse2`,
/// `parse`, `http`, `response`, `2`.
#[derive(Clone, Default)]
pub struct CodeTokenizer;

pub struct CodeTokenStream {
    tokens: Vec<Token>,
    index: usize,
}

impl Tokenizer for CodeTokenizer {
    type TokenStream<'a> = CodeTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> CodeTokenStream {
        CodeTokenStream {
            tokens: tokenize(text),
            index: 0,
        }
    }
}

impl TokenStream for CodeTokenStream {
    fn advance(&mut self) -> bool {
        self.index += 1;
        self.index <= self.tokens.len()
    }

    fn token(&self) -> &Token {
        &self.tokens[self.index - 1]
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.tokens[self.index - 1]
    }
}

const MAX_TOKEN_LEN: usize = 64;

/// Produces exactly the terms the [`CodeTokenizer`] Tantivy tokenizer emits, in order.
pub fn code_terms(text: &str) -> Vec<String> {
    tokenize(text).into_iter().map(|t| t.text).collect()
}

/// The tokens [`CodeTokenizer`] produces, in order.
pub fn tokenize(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut position = 0;
    let mut push = |out: &mut Vec<Token>, from: usize, to: usize, text: String, advance: bool| {
        if text.is_empty() || text.len() > MAX_TOKEN_LEN {
            return;
        }
        out.push(Token {
            offset_from: from,
            offset_to: to,
            position,
            text,
            position_length: 1,
        });
        if advance {
            position += 1;
        }
    };
    for (start, word) in words(text) {
        let parts = split_identifier(word);
        if parts.len() > 1 {
            // The whole identifier shares the first part's position.
            push(&mut out, start, start + word.len(), compact(word), false);
        }
        for (off, part) in parts {
            push(
                &mut out,
                start + off,
                start + off + part.len(),
                part.to_lowercase(),
                true,
            );
        }
    }
    out
}

/// The whole identifier without separators, lowercased.
fn compact(word: &str) -> String {
    word.chars()
        .filter(|c| *c != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

/// Maximal runs of alphanumerics and underscores, with their byte offsets.
fn words(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut start = None;
    let mut out = Vec::new();
    for (i, c) in text.char_indices() {
        let word_char = c.is_alphanumeric() || c == '_';
        match (word_char, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                out.push((s, &text[s..i]));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s, &text[s..]));
    }
    out.into_iter()
}

/// Splits one word into its parts, with byte offsets relative to the word.
fn split_identifier(word: &str) -> Vec<(usize, &str)> {
    let chars: Vec<(usize, char)> = word.char_indices().collect();
    let mut parts = Vec::new();
    let mut start: Option<usize> = None;
    for (k, &(i, c)) in chars.iter().enumerate() {
        if c == '_' {
            if let Some(s) = start.take() {
                parts.push((s, &word[s..i]));
            }
            continue;
        }
        if let Some(s) = start {
            let prev = chars[k - 1].1;
            let next = chars.get(k + 1).map(|&(_, n)| n);
            let boundary = (prev.is_lowercase() && c.is_uppercase())
                || (prev.is_uppercase() && c.is_uppercase() && next.is_some_and(char::is_lowercase))
                || (prev.is_alphabetic() && c.is_numeric())
                || (prev.is_numeric() && c.is_alphabetic());
            if boundary && prev != '_' {
                parts.push((s, &word[s..i]));
                start = Some(i);
            }
        } else {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        parts.push((s, &word[s..]));
    }
    parts
}

/// Weight of a match in the defined symbol names against one in the text. It also decides
/// how code competes with documentation (which defines no symbols) in one ranking. Higher
/// values let natural-language words that happen to name a function (`test`, `check`)
/// outrank the text that answers; lower ones let documentation crowd out code. 2.5 has the
/// best mean MRR over the five golden sets for both corpora in one ranking (eval/RESULTS.md).
pub const SYMBOL_BOOST: f32 = 2.5;
/// Weight of a match in the file path.
pub const PATH_BOOST: f32 = 1.5;

/// The lexical index of one project.
pub struct LexicalIndex {
    index: Index,
    reader: IndexReader,
    f_id: Field,
    f_file: Field,
    f_path: Field,
    f_symbols: Field,
    f_text: Field,
}

impl LexicalIndex {
    /// Opens the index in `dir`, creating it when absent.
    pub fn open(dir: &Path) -> tantivy::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let index = match Index::open_in_dir(dir) {
            Ok(i) => i,
            Err(_) => Index::create_in_dir(dir, schema())?,
        };
        index
            .tokenizers()
            .register(TOKENIZER, TextAnalyzer::from(CodeTokenizer));
        let s = index.schema();
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok(Self {
            f_id: s.get_field("chunk_id")?,
            f_file: s.get_field("file")?,
            f_path: s.get_field("path")?,
            f_symbols: s.get_field("symbols")?,
            f_text: s.get_field("text")?,
            index,
            reader,
        })
    }

    /// Starts a batch of changes; call [`LexicalWriter::commit`] to publish them.
    pub fn writer(&self) -> tantivy::Result<LexicalWriter<'_>> {
        Ok(LexicalWriter {
            index: self,
            writer: self.index.writer(WRITER_HEAP)?,
        })
    }

    /// BM25 search; returns `(chunk_id, score)` best first.
    pub fn search(&self, query: &str, limit: usize) -> tantivy::Result<Vec<(String, f32)>> {
        let mut parser = QueryParser::for_index(&self.index, vec![self.f_text, self.f_symbols, self.f_path]);
        parser.set_field_boost(self.f_symbols, SYMBOL_BOOST);
        parser.set_field_boost(self.f_path, PATH_BOOST);
        let (q, _errors) = parser.parse_query_lenient(&sanitize(query));
        let searcher = self.reader.searcher();
        let top = searcher.search(&q, &TopDocs::with_limit(limit).order_by_score())?;
        top.into_iter()
            .map(|(score, addr)| {
                let d: TantivyDocument = searcher.doc(addr)?;
                let id = d
                    .get_first(self.f_id)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                Ok((id, score))
            })
            .collect()
    }

    /// Number of indexed chunks.
    pub fn len(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Query syntax characters become spaces, so any user text is a plain bag of terms.
pub(crate) fn sanitize(query: &str) -> String {
    query
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '_' { c } else { ' ' })
        .collect()
}

fn schema() -> Schema {
    let code = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(TOKENIZER)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    let mut b = Schema::builder();
    b.add_text_field("chunk_id", STRING | STORED);
    b.add_text_field("file", STRING);
    b.add_text_field("path", code.clone());
    b.add_text_field("symbols", code.clone());
    b.add_text_field("text", code);
    b.build()
}

/// A batch of index changes.
pub struct LexicalWriter<'a> {
    index: &'a LexicalIndex,
    writer: IndexWriter,
}

impl LexicalWriter<'_> {
    pub fn add(&mut self, chunk: &Chunk) -> tantivy::Result<()> {
        let i = self.index;
        self.writer.add_document(doc!(
            i.f_id => chunk.id.as_str(),
            i.f_file => chunk.path.as_str(),
            i.f_path => chunk.path.as_str(),
            i.f_symbols => chunk.symbols.join(" "),
            i.f_text => chunk.indexed_text(),
        ))?;
        Ok(())
    }

    /// Removes every chunk of `path`.
    pub fn remove_file(&mut self, path: &str) {
        self.writer
            .delete_term(Term::from_field_text(self.index.f_file, path));
    }

    pub fn clear(&mut self) -> tantivy::Result<()> {
        self.writer.delete_all_documents()?;
        Ok(())
    }

    pub fn commit(mut self) -> tantivy::Result<()> {
        self.writer.commit()?;
        self.writer.wait_merging_threads()?;
        self.index.reader.reload()
    }
}

impl LexicalStore for LexicalIndex {
    fn search(&self, query: &str, limit: usize) -> tantivy::Result<Vec<(String, f32)>> {
        self.search(query, limit)
    }

    fn writer(&self) -> tantivy::Result<Box<dyn LexicalWrite + '_>> {
        Ok(Box::new(self.writer()?))
    }

    fn len(&self) -> u64 {
        self.len()
    }

    fn is_empty(&self) -> bool {
        self.is_empty()
    }
}

impl LexicalWrite for LexicalWriter<'_> {
    fn add(&mut self, chunk: &Chunk) -> tantivy::Result<()> {
        self.add(chunk)
    }

    fn remove_file(&mut self, path: &str) {
        self.remove_file(path);
    }

    fn clear(&mut self) -> tantivy::Result<()> {
        self.clear()
    }

    fn commit(self: Box<Self>) -> tantivy::Result<()> {
        (*self).commit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{ChunkOptions, chunk_file};
    use crate::lang::Lang;

    fn texts(s: &str) -> Vec<String> {
        code_terms(s)
    }

    #[test]
    fn code_terms_matches_tokenizer_output() {
        assert_eq!(
            code_terms("parseHTTPResponse2"),
            ["parsehttpresponse2", "parse", "http", "response", "2"]
        );
        let sample = "fooBar_baz 123";
        let from_tokens: Vec<String> = tokenize(sample).into_iter().map(|t| t.text).collect();
        assert_eq!(code_terms(sample), from_tokens);
    }

    #[test]
    fn splits_identifiers_and_keeps_the_whole() {
        assert_eq!(
            texts("parseHTTPResponse2"),
            ["parsehttpresponse2", "parse", "http", "response", "2"]
        );
        assert_eq!(texts("max_file_bytes"), ["maxfilebytes", "max", "file", "bytes"]);
        assert_eq!(texts("Locate(name)"), ["locate", "name"]);
        assert_eq!(texts("über_größe"), ["übergröße", "über", "größe"]);
    }

    #[test]
    fn positions_share_the_first_part() {
        let t = tokenize("fooBar baz");
        assert_eq!(t.iter().map(|t| t.position).collect::<Vec<_>>(), [0, 0, 1, 2]);
    }

    #[test]
    fn indexes_searches_and_removes() {
        let dir = tempfile::tempdir().unwrap();
        let idx = LexicalIndex::open(dir.path()).unwrap();
        let a = chunk_file(
            "pkg/sidecar/sidecar.go",
            Lang::Go,
            "package sidecar\n\nfunc LocateBinary(name string) string { return name }\n",
            ChunkOptions::default(),
        );
        let b = chunk_file(
            "pkg/rag/indexer.go",
            Lang::Go,
            "package rag\n\nfunc IndexProject(root string) error { return nil }\n",
            ChunkOptions::default(),
        );
        let mut w = idx.writer().unwrap();
        for c in a.iter().chain(&b) {
            w.add(c).unwrap();
        }
        w.commit().unwrap();

        let hits = idx.search("locate binary", 5).unwrap();
        assert_eq!(hits[0].0, a[0].id);
        let hits = idx.search("index_project", 5).unwrap();
        assert_eq!(hits[0].0, b[0].id);
        assert!(idx.search("(unbalanced \"quote", 5).is_ok());

        let mut w = idx.writer().unwrap();
        w.remove_file("pkg/rag/indexer.go");
        w.commit().unwrap();
        assert!(idx.search("IndexProject", 5).unwrap().is_empty());
        assert_eq!(idx.len(), a.len() as u64);

        // Reopening keeps the data.
        drop(idx);
        let idx = LexicalIndex::open(dir.path()).unwrap();
        assert_eq!(idx.search("sidecar", 5).unwrap().len(), 1);
    }
}
