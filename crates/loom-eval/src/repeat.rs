//! The same question asked twice, each time in a fresh `dendrite-rag` process, the way
//! `dendrite ask` runs it: start, handshake, `index` with the interactive embedding budget,
//! then `context` (and `ask` with a real model when asked). Every stage is timed, so a
//! retrieval that stalls (an embedding backlog, an index rebuilt on every call) shows up
//! next to the model time.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Instant;

use serde_json::{Value, json};

/// The embedding budget Dendrite gives each interactive call.
pub const EMBED_BUDGET_MS: u64 = 3000;

pub struct Run {
    pub spawn_ms: u64,
    pub index_ms: u64,
    pub index: Value,
    pub context_ms: u64,
    pub context: Value,
    pub ask_ms: Option<u64>,
    pub ask: Option<Value>,
}

struct Sidecar {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Sidecar {
    fn start(bin: &Path) -> anyhow::Result<Self> {
        let mut child = Command::new(bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = BufReader::new(child.stdout.take().expect("piped"));
        Ok(Self {
            child,
            stdin,
            stdout,
            next: 0,
        })
    }

    /// Calls `method`; returns the result and the wall time in milliseconds.
    fn call(&mut self, method: &str, params: Value) -> anyhow::Result<(Value, u64)> {
        let started = Instant::now();
        self.next += 1;
        let req = json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params});
        writeln!(self.stdin, "{req}")?;
        self.stdin.flush()?;
        let mut line = String::new();
        self.stdout.read_line(&mut line)?;
        let resp: Value = serde_json::from_str(&line)?;
        if let Some(e) = resp.get("error") {
            anyhow::bail!("{method}: {e}");
        }
        Ok((resp["result"].clone(), started.elapsed().as_millis() as u64))
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One fresh-process run of `question` against `root`.
pub fn run_once(bin: &Path, root: &Path, question: &str, corpus: &str, llm: bool) -> anyhow::Result<Run> {
    let started = Instant::now();
    let mut s = Sidecar::start(bin)?;
    s.call("health", json!({}))?;
    let spawn_ms = started.elapsed().as_millis() as u64;
    let root = root.display().to_string();
    let (index, index_ms) = s.call(
        "index",
        json!({"root": root, "embed_budget_ms": EMBED_BUDGET_MS, "focus": question}),
    )?;
    let (context, context_ms) = s.call(
        "context",
        json!({"root": root, "query": question, "token_budget": 2500, "corpus": corpus}),
    )?;
    let (ask, ask_ms) = if llm {
        let (a, ms) = s.call(
            "ask",
            json!({"root": root, "question": question, "corpus": corpus}),
        )?;
        (Some(a), Some(ms))
    } else {
        (None, None)
    };
    Ok(Run {
        spawn_ms,
        index_ms,
        index,
        context_ms,
        context,
        ask_ms,
        ask,
    })
}

/// Markdown rows, one per run.
pub fn report(question: &str, corpus: &str, runs: &[Run]) -> String {
    let n = |v: &Value, k: &str| v[k].as_u64().unwrap_or(0);
    let mut out = format!(
        "Question: \"{question}\" (corpus `{corpus}`, embedding budget {EMBED_BUDGET_MS} ms)\n\n\
         | run | spawn | index (open / walk / graph / embed) | embedded / pending | context (lexical / dense / symbol / expand) | ask (local model) | total |\n\
         |---|---|---|---|---|---|---|\n"
    );
    for (i, r) in runs.iter().enumerate() {
        let t = &r.context["timings"];
        let ask = r.ask_ms.map_or("—".to_string(), |ms| {
            let local = r.ask.as_ref().map_or(0, |a| n(&a["timings"], "local_ms"));
            let cached = r.ask.as_ref().is_some_and(|a| a["cached"] == true);
            format!("{ms} ms ({local} ms){}", if cached { ", cached" } else { "" })
        });
        let total = r.spawn_ms + r.index_ms + r.context_ms + r.ask_ms.unwrap_or(0);
        out.push_str(&format!(
            "| {} | {} ms | {} ms ({} / {} / {} / {}) | {} / {} | {} ms ({} / {} / {} / {}) | {ask} | {total} ms |\n",
            i + 1,
            r.spawn_ms,
            r.index_ms,
            n(&r.index, "open_ms"),
            n(&r.index, "walk_ms"),
            n(&r.index, "graph_ms"),
            n(&r.index, "embed_ms"),
            n(&r.index, "embedded"),
            n(&r.index, "embed_pending"),
            r.context_ms,
            n(t, "lexical_ms"),
            n(t, "dense_ms"),
            n(t, "symbol_ms"),
            n(t, "expand_ms"),
        ));
    }
    out
}
