//! Shared test fixtures.

use std::fs;
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

/// Feature-hashed bag of code tokens: deterministic, and texts sharing identifiers land
/// close together. Setting `fail` makes every document call error.
pub struct HashEmbedder {
    pub fail: AtomicBool,
    /// Makes query embedding fail (the gateway went down after indexing).
    pub fail_queries: AtomicBool,
    pub calls: AtomicUsize,
}

impl HashEmbedder {
    pub fn new() -> Self {
        Self {
            fail: false.into(),
            fail_queries: false.into(),
            calls: 0.into(),
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
        "hash-64"
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
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}",
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
}
