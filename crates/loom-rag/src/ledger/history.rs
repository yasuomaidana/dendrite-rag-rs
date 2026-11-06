//! Searchable git history (`docs/specs/history.md`): one document per (commit, file) with
//! the commit message and that file's diff, in a separate Tantivy index.
//!
//! History is read with the `git` CLI (`git log -p`), streamed line by line; repository
//! object access is not reimplemented here.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{
    Field, INDEXED, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value,
};
use tantivy::tokenizer::TextAnalyzer;
use tantivy::{Index, IndexReader, ReloadPolicy, TantivyDocument, doc};

use crate::lexical::{CodeTokenizer, TOKENIZER, sanitize};

/// Commits read on the first run (later runs read only new commits).
pub const DEFAULT_MAX_COMMITS: usize = 2000;
/// Diff text kept per (commit, file) document.
const MAX_DIFF_CHARS: usize = 2000;
/// Commits touching more files than this keep only their message (mass renames, vendoring).
const MAX_FILES_PER_COMMIT: usize = 50;
const WRITER_HEAP: usize = 30_000_000;

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("git: {0}")]
    Git(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Index(#[from] tantivy::TantivyError),
}

pub type Result<T> = std::result::Result<T, HistoryError>;

/// One retrieved (commit, file) change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryHit {
    pub sha: String,
    /// Commit time, Unix seconds.
    pub time: i64,
    pub author: String,
    pub subject: String,
    /// Changed file, or empty for a message-only document.
    pub path: String,
    /// The (truncated) diff of `path` in this commit.
    pub diff: String,
    pub score: f32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HistoryReport {
    pub commits: usize,
    pub documents: usize,
    /// The newest indexed commit.
    pub head: Option<String>,
    pub rebuilt: bool,
}

struct Fields {
    sha: Field,
    time: Field,
    author: Field,
    subject: Field,
    path: Field,
    message: Field,
    diff: Field,
}

pub struct HistoryIndex {
    index: Index,
    reader: IndexReader,
    f: Fields,
}

/// A parsed commit with its per-file diffs.
#[derive(Debug, Default, PartialEq)]
struct Commit {
    sha: String,
    time: i64,
    author: String,
    subject: String,
    body: String,
    files: Vec<(String, String)>,
}

impl HistoryIndex {
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let index = match Index::open_in_dir(dir) {
            Ok(i) => i,
            Err(_) => Index::create_in_dir(dir, schema())?,
        };
        index
            .tokenizers()
            .register(TOKENIZER, TextAnalyzer::from(CodeTokenizer));
        let s = index.schema();
        let f = Fields {
            sha: s.get_field("sha")?,
            time: s.get_field("time")?,
            author: s.get_field("author")?,
            subject: s.get_field("subject")?,
            path: s.get_field("path")?,
            message: s.get_field("message")?,
            diff: s.get_field("diff")?,
        };
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok(Self { index, reader, f })
    }

    /// Indexes commits newer than `since` (all of them, up to `max_commits`, when `since` is
    /// `None` or no longer an ancestor of `HEAD`, e.g. after a rebase).
    pub fn update(&self, repo: &Path, since: Option<&str>, max_commits: usize) -> Result<HistoryReport> {
        let Some(head) = rev_parse(repo, "HEAD")? else {
            return Ok(HistoryReport::default()); // no commits yet
        };
        if since == Some(head.as_str()) {
            return Ok(HistoryReport {
                head: Some(head),
                ..HistoryReport::default()
            });
        }
        let incremental = since.filter(|s| is_ancestor(repo, s, &head));
        let mut writer = self.index.writer(WRITER_HEAP)?;
        let mut report = HistoryReport {
            head: Some(head.clone()),
            ..HistoryReport::default()
        };
        if incremental.is_none() {
            writer.delete_all_documents()?;
            report.rebuilt = true;
        }
        let range = incremental.map_or_else(|| head.clone(), |s| format!("{s}..{head}"));
        for_each_commit(repo, &range, max_commits, |c| {
            report.commits += 1;
            let message = format!("{}\n{}", c.subject, c.body);
            let files: Vec<(String, String)> = if c.files.is_empty() || c.files.len() > MAX_FILES_PER_COMMIT {
                vec![(String::new(), String::new())] // message-only document
            } else {
                c.files
            };
            for (path, diff) in files {
                writer.add_document(doc!(
                    self.f.sha => c.sha.as_str(),
                    self.f.time => c.time,
                    self.f.author => c.author.as_str(),
                    self.f.subject => c.subject.as_str(),
                    self.f.path => path,
                    self.f.message => message.as_str(),
                    self.f.diff => diff,
                ))?;
                report.documents += 1;
            }
            Ok(())
        })?;
        writer.commit()?;
        writer.wait_merging_threads()?;
        self.reader.reload()?;
        Ok(report)
    }

    /// BM25 over messages (boosted), paths and diffs; best first.
    pub fn search(&self, query: &str, top_k: usize) -> Result<Vec<HistoryHit>> {
        let mut parser = QueryParser::for_index(&self.index, vec![self.f.message, self.f.path, self.f.diff]);
        parser.set_field_boost(self.f.message, 2.0);
        parser.set_field_boost(self.f.path, 1.5);
        let (q, _) = parser.parse_query_lenient(&sanitize(query));
        let searcher = self.reader.searcher();
        let top = searcher.search(&q, &TopDocs::with_limit(top_k).order_by_score())?;
        let text = |d: &TantivyDocument, f: Field| {
            d.get_first(f)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let mut hits = top
            .into_iter()
            .map(|(score, addr)| {
                let d: TantivyDocument = searcher.doc(addr)?;
                Ok(HistoryHit {
                    sha: text(&d, self.f.sha),
                    time: d
                        .get_first(self.f.time)
                        .and_then(|v| v.as_i64())
                        .unwrap_or_default(),
                    author: text(&d, self.f.author),
                    subject: text(&d, self.f.subject),
                    path: text(&d, self.f.path),
                    diff: text(&d, self.f.diff),
                    score,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        // Documents of one commit often tie (only the message matched): newest commit
        // first, then path, so results do not depend on segment order.
        hits.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(b.time.cmp(&a.time))
                .then_with(|| a.path.cmp(&b.path))
        });
        Ok(hits)
    }

    pub fn len(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn schema() -> Schema {
    let code = |stored: bool| {
        let o = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(TOKENIZER)
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        );
        if stored { o.set_stored() } else { o }
    };
    let mut b = Schema::builder();
    b.add_text_field("sha", STRING | STORED);
    b.add_i64_field("time", INDEXED | STORED);
    b.add_text_field("author", STORED);
    b.add_text_field("subject", STORED);
    b.add_text_field("path", code(true));
    b.add_text_field("message", code(false));
    b.add_text_field("diff", code(true));
    b.build()
}

fn git(repo: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(repo);
    c
}

fn rev_parse(repo: &Path, rev: &str) -> Result<Option<String>> {
    let out = git(repo)
        .args(["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")])
        .output()
        .map_err(|e| HistoryError::Git(format!("cannot run git: {e}")))?;
    if !out.status.success() {
        // Not a repository is an error; a repository without commits is not.
        let inside = git(repo)
            .args(["rev-parse", "--git-dir"])
            .output()?
            .status
            .success();
        return if inside {
            Ok(None)
        } else {
            Err(HistoryError::Git(format!(
                "{} is not a git repository",
                repo.display()
            )))
        };
    }
    Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_string()))
}

fn is_ancestor(repo: &Path, ancestor: &str, head: &str) -> bool {
    git(repo)
        .args(["merge-base", "--is-ancestor", ancestor, head])
        .status()
        .is_ok_and(|s| s.success())
}

const RECORD: char = '\u{1e}';
const FIELD: char = '\u{1f}';

/// Streams `git log -p` for `range`, newest first, calling `f` once per commit.
fn for_each_commit(
    repo: &Path,
    range: &str,
    max_commits: usize,
    mut f: impl FnMut(Commit) -> std::result::Result<(), tantivy::TantivyError>,
) -> Result<()> {
    let mut child = git(repo)
        .args([
            "log",
            "--no-merges",
            "--no-color",
            "--no-ext-diff",
            "--unified=1",
            "-p",
            &format!("--max-count={max_commits}"),
            // Header line: RS sha US time US author US subject, then the body until a lone US.
            "--format=%x1e%H%x1f%at%x1f%an%x1f%s%n%b%x1f",
            range,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| HistoryError::Git(format!("cannot run git: {e}")))?;
    let reader = BufReader::new(child.stdout.take().expect("piped stdout"));
    let mut parser = LogParser::default();
    for line in reader.split(b'\n') {
        let line = String::from_utf8_lossy(&line?).into_owned();
        if let Some(done) = parser.push(&line) {
            f(done)?;
        }
    }
    if let Some(done) = parser.finish() {
        f(done)?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(HistoryError::Git(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(())
}

/// Incremental parser for the `git log` format above.
#[derive(Default)]
struct LogParser {
    cur: Option<Commit>,
    in_body: bool,
    file: Option<(String, String)>,
}

impl LogParser {
    /// Feeds one line; returns a commit when the next one starts.
    fn push(&mut self, line: &str) -> Option<Commit> {
        if let Some(header) = line.strip_prefix(RECORD) {
            let done = self.finish();
            let mut parts = header.splitn(4, FIELD);
            self.cur = Some(Commit {
                sha: parts.next().unwrap_or_default().to_string(),
                time: parts.next().and_then(|t| t.parse().ok()).unwrap_or_default(),
                author: parts.next().unwrap_or_default().to_string(),
                subject: parts.next().unwrap_or_default().to_string(),
                ..Commit::default()
            });
            self.in_body = true;
            return done;
        }
        let cur = self.cur.as_mut()?;
        if self.in_body {
            if let Some(rest) = line.strip_suffix(FIELD) {
                cur.body.push_str(rest);
                self.in_body = false;
            } else {
                cur.body.push_str(line);
                cur.body.push('\n');
            }
            return None;
        }
        if let Some(paths) = line.strip_prefix("diff --git ") {
            if let Some(f) = self.file.take() {
                cur.files.push(f);
            }
            // "a/<path> b/<path>": take the new side.
            let path = paths.rsplit_once(" b/").map_or(paths, |(_, b)| b).to_string();
            self.file = Some((path, String::new()));
        } else if let Some((_, diff)) = self.file.as_mut() {
            let skip = line.starts_with("index ") || line.starts_with("--- ") || line.starts_with("+++ ");
            if !skip && diff.len() < MAX_DIFF_CHARS {
                diff.push_str(line);
                diff.push('\n');
                if diff.len() > MAX_DIFF_CHARS {
                    let mut end = MAX_DIFF_CHARS;
                    while !diff.is_char_boundary(end) {
                        end -= 1;
                    }
                    diff.truncate(end);
                    diff.push_str("\n…");
                }
            }
        }
        None
    }

    fn finish(&mut self) -> Option<Commit> {
        let mut c = self.cur.take()?;
        if let Some(f) = self.file.take() {
            c.files.push(f);
        }
        c.body = c.body.trim().to_string();
        Some(c)
    }
}

#[cfg(test)]
mod tests;
