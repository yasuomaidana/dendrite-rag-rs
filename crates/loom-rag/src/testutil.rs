//! Shared test fixtures.

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::embed::{EmbedError, Embedder, Result, normalize};

/// A small Go + Markdown project.
pub fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("pkg/sidecar")).unwrap();
    fs::create_dir_all(root.join("pkg/rag")).unwrap();
    fs::write(
        root.join("pkg/sidecar/sidecar.go"),
        "package sidecar\n\n// LocateBinary finds a sidecar executable.\nfunc LocateBinary(name string) string { return name }\n",
    )
    .unwrap();
    fs::write(
        root.join("pkg/rag/indexer.go"),
        "package rag\n\nfunc IndexProject(root string) error { return nil }\n",
    )
    .unwrap();
    fs::write(root.join("README.md"), "# Demo\n\nA project used in tests.\n").unwrap();
    dir
}

/// Words the synthetic corpus draws identifiers and comments from.
const WORDS: [&str; 48] = [
    "parse", "index", "chunk", "vector", "ledger", "merge", "token", "query", "graph", "commit", "stream",
    "buffer", "cache", "embed", "score", "rank", "fusion", "lexical", "dense", "symbol", "resolve", "import",
    "module", "client", "server", "request", "response", "error", "retry", "budget", "window", "segment",
    "writer", "reader", "schema", "migrate", "journal", "replay", "mirror", "shard", "batch", "worker",
    "queue", "lease", "prompt", "model", "config", "status",
];

/// A deterministic Go project of `files` files in packages of 50, each with a few
/// functions named and commented from [`WORDS`] and calling each other: a corpus for
/// benchmarks and memory gates that needs no checkout.
pub fn synthetic_project(files: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let word = |n: u64| WORDS[(n % WORDS.len() as u64) as usize];
    for f in 0..files {
        let pkg = format!("p{}", f / 50);
        let d = dir.path().join("pkg").join(&pkg);
        fs::create_dir_all(&d).unwrap();
        let mut src = format!("package {pkg}\n\nimport \"fmt\"\n");
        for i in 0..4 {
            let (a, b, c) = (word(next()), word(next()), word(next()));
            let name = format!("{}{}{}{f}x{i}", cap(a), cap(b), cap(c));
            let callee = format!("{}{}{}", cap(word(next())), cap(word(next())), cap(word(next())));
            src.push_str(&format!(
                "\n// {name} will {a} the {b} before it can {c} each {} in the {}.\nfunc {name}(n int) error {{\n\tif n > {} {{\n\t\treturn fmt.Errorf(\"{a} {b}: %d\", n)\n\t}}\n\t_ = {callee}\n\treturn nil\n}}\n",
                word(next()),
                word(next()),
                next() % 100
            ));
        }
        fs::write(d.join(format!("f{f}.go")), src).unwrap();
    }
    dir
}

fn cap(w: &str) -> String {
    let mut c = w.chars();
    c.next()
        .map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
}

/// Feature-hashed bag of code tokens: deterministic, and texts sharing identifiers land
/// close together. Setting `fail` makes every document call error.
pub struct HashEmbedder {
    pub fail: AtomicBool,
    /// Makes query embedding fail (the gateway went down after indexing).
    pub fail_queries: AtomicBool,
    pub calls: Arc<AtomicUsize>,
    pub model: &'static str,
}

impl Default for HashEmbedder {
    fn default() -> Self {
        Self::new()
    }
}

impl HashEmbedder {
    pub fn new() -> Self {
        Self::with_model("hash-64")
    }

    pub fn with_model(model: &'static str) -> Self {
        Self {
            fail: false.into(),
            fail_queries: false.into(),
            calls: Arc::new(AtomicUsize::new(0)),
            model,
        }
    }

    pub fn vector(text: &str) -> Vec<f32> {
        let mut v = vec![0f32; 64];
        for t in crate::lexical::tokenize(text) {
            let h = blake3::hash(t.text.as_bytes());
            v[h.as_bytes()[0] as usize % 64] += 1.0;
        }
        normalize(v)
    }
}

impl Embedder for HashEmbedder {
    fn model_id(&self) -> &str {
        self.model
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(EmbedError::Http("gateway down".into()));
        }
        self.calls.fetch_add(texts.len(), Ordering::SeqCst);
        Ok(texts.iter().map(|t| Self::vector(t)).collect())
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        if self.fail_queries.load(Ordering::SeqCst) {
            return Err(EmbedError::Http("gateway down".into()));
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Self::vector(text))
    }
}

/// One HTTP request captured by [`HttpStub`].
pub struct Captured {
    /// Request line and headers.
    pub head: String,
    pub body: serde_json::Value,
}

/// A local HTTP server that answers every request with a fixed JSON body and records the
/// requests, for checking the wire format of the server backends.
pub struct HttpStub {
    pub url: String,
    requests: std::sync::Arc<std::sync::Mutex<Vec<Captured>>>,
}

impl HttpStub {
    pub fn serve(response: &'static str) -> Self {
        Self::serve_status(200, "OK", response)
    }

    pub fn serve_status(status: u16, status_text: &'static str, response: &'static str) -> Self {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut head = String::new();
                let mut len = 0;
                let mut chunked = false;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    let lower = line.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                    chunked |= lower.starts_with("transfer-encoding:") && lower.contains("chunked");
                    head.push_str(&line);
                }
                let mut body = Vec::new();
                if chunked {
                    loop {
                        let mut size = String::new();
                        reader.read_line(&mut size).unwrap();
                        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
                        let mut part = vec![0; n + 2];
                        reader.read_exact(&mut part).unwrap();
                        if n == 0 {
                            break;
                        }
                        body.extend_from_slice(&part[..n]);
                    }
                } else {
                    body.resize(len, 0);
                    reader.read_exact(&mut body).unwrap();
                }
                seen.lock().unwrap().push(Captured {
                    head,
                    body: serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
                });
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} {status_text}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}",
                    response.len()
                );
            }
        });
        Self { url, requests }
    }

    /// The first request received.
    pub fn request(&self) -> Captured {
        let mut r = self.requests.lock().unwrap();
        assert!(!r.is_empty(), "no request reached the stub");
        r.remove(0)
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

/// A PostgreSQL 16 + pgvector database for the shared-tier tests: `LOOM_TEST_PG_URL` when
/// set (CI's service container), otherwise a container started for this test with
/// testcontainers and removed when the value drops.
#[cfg(all(test, feature = "postgres"))]
pub struct TestPg {
    pub url: String,
    _container: Option<testcontainers::Container<testcontainers::GenericImage>>,
}

/// Starts (or names) the test database; `None` when neither the variable nor Docker is
/// available, and the test then passes without doing anything.
#[cfg(all(test, feature = "postgres"))]
pub fn test_pg() -> Option<TestPg> {
    use testcontainers::core::{IntoContainerPort, WaitFor};
    use testcontainers::runners::SyncRunner;
    use testcontainers::{GenericImage, ImageExt};

    if let Some(url) = std::env::var("LOOM_TEST_PG_URL").ok().filter(|u| !u.is_empty()) {
        return Some(TestPg {
            url,
            _container: None,
        });
    }
    let started = GenericImage::new("pgvector/pgvector", "pg16")
        .with_exposed_port(5432.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_USER", "rag")
        .with_env_var("POSTGRES_PASSWORD", "rag")
        .with_env_var("POSTGRES_DB", "ragtest")
        .start();
    let container = match started {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skipping: no LOOM_TEST_PG_URL and no Docker for a pgvector container ({e})");
            return None;
        }
    };
    let port = container.get_host_port_ipv4(5432.tcp()).ok()?;
    let url = format!("postgresql://rag:rag@127.0.0.1:{port}/ragtest");
    // The first boot restarts the server after initdb: wait until it answers queries.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if let Ok(mut c) = postgres::Client::connect(&url, postgres::NoTls)
            && c.simple_query("SELECT 1").is_ok()
        {
            break;
        }
        if std::time::Instant::now() > deadline {
            eprintln!("skipping: the pgvector container did not accept connections");
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    Some(TestPg {
        url,
        _container: Some(container),
    })
}

/// `n` pseudo-random unit vectors of `dim` components, reproducible from `seed`.
pub fn rand_vecs(n: usize, dim: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut s = seed;
    (0..n)
        .map(|_| {
            normalize(
                (0..dim)
                    .map(|_| {
                        s = s
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(1442695040888963407);
                        ((s >> 33) as f32 / u32::MAX as f32) - 0.25
                    })
                    .collect(),
            )
        })
        .collect()
}

/// The indexes of the `k` vectors closest to `q` by exact dot product.
pub fn exact(vs: &[Vec<f32>], q: &[f32], k: usize) -> Vec<usize> {
    let mut s: Vec<(usize, f32)> = vs
        .iter()
        .enumerate()
        .map(|(i, v)| (i, v.iter().zip(q).map(|(a, b)| a * b).sum()))
        .collect();
    s.sort_by(|a, b| b.1.total_cmp(&a.1));
    s.into_iter().take(k).map(|(i, _)| i).collect()
}

/// A temporary directory holding `files` (`relative path`, `contents`), parents created.
pub fn write_files(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (rel, body) in files {
        let path = dir.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    dir
}

/// An embedder that delegates to a [`HashEmbedder`] the test keeps a handle on, so two engines
/// (or the test itself) can share one model and count its calls.
pub struct SharedEmbedder(pub Arc<HashEmbedder>);

impl Embedder for SharedEmbedder {
    fn model_id(&self) -> &str {
        self.0.model_id()
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.0.embed_documents(texts)
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.0.embed_query(text)
    }
}

/// `prefix` plus the current time in nanoseconds: a repository name no earlier run used.
pub fn unique(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{prefix}-{nanos}")
}

/// Storage settings for repository `repo` on the PostgreSQL server at `url`.
pub fn pg_storage(mode: crate::store::StorageMode, url: &str, repo: &str) -> crate::store::StorageConfig {
    crate::store::StorageConfig {
        mode,
        repo: Some(repo.to_string()),
        pg_url: Some(url.to_string()),
        ..crate::store::StorageConfig::default()
    }
}

/// An engine over `root` whose index lives in `index` and which stores as `storage` says. It
/// embeds with `embedder` when given (sharing its call counts), else with its own hash model.
pub fn engine_with(
    root: &std::path::Path,
    index: &str,
    storage: crate::store::StorageConfig,
    embedder: Option<&Arc<HashEmbedder>>,
) -> crate::engine::Engine {
    let cfg = crate::engine::Config {
        index_dir: Some(root.join(index)),
        storage,
        ..crate::engine::Config::default()
    };
    let embedder: Box<dyn Embedder> = match embedder {
        Some(e) => Box::new(SharedEmbedder(e.clone())),
        None => Box::new(HashEmbedder::new()),
    };
    crate::engine::Engine::open_with_embedder(root, cfg, Some(embedder)).unwrap()
}

/// What every ledger backend must keep across a reopen: files, chunks, embeddings and metadata,
/// and a cleared index that still holds its embeddings. `open` opens the same store each call.
pub fn assert_ledger_round_trip<L: crate::store::LedgerStore>(open: impl Fn() -> L) {
    use crate::chunk::{ChunkOptions, chunk_file};
    use crate::ledger::{FileEntry, SCHEMA_VERSION};

    let chunks = chunk_file(
        "a.go",
        crate::lang::Lang::Go,
        "package a\n\nfunc F() {}\n",
        ChunkOptions::default(),
    );
    {
        let ledger = open();
        let mut b = ledger.batch().unwrap();
        let entry = FileEntry {
            hash: "h".into(),
            size: 3,
            mtime_ns: 7,
            chunk_ids: vec![chunks[0].id.clone()],
            imports: vec!["fmt".into()],
        };
        b.put_file("a.go", &entry).unwrap();
        b.put_chunk(&chunks[0]).unwrap();
        b.put_embedding("m:x", &[1.0, -0.5]).unwrap();
        b.set_meta("merkle_root", "abc").unwrap();
        b.commit().unwrap();
    }
    let ledger = open();
    assert_eq!(ledger.files().unwrap()["a.go"].mtime_ns, 7);
    assert_eq!(ledger.chunk(&chunks[0].id).unwrap().unwrap(), chunks[0]);
    assert_eq!(ledger.embedding("m:x").unwrap().unwrap(), vec![1.0, -0.5]);
    assert_eq!(ledger.meta("merkle_root").unwrap().as_deref(), Some("abc"));
    assert_eq!(ledger.meta("schema").unwrap().as_deref(), Some(SCHEMA_VERSION));

    let mut b = ledger.batch().unwrap();
    b.clear_index().unwrap();
    b.commit().unwrap();
    assert!(ledger.files().unwrap().is_empty());
    assert_eq!(ledger.chunk_count().unwrap(), 0);
    assert_eq!(ledger.embedding_count().unwrap(), 1);
}
