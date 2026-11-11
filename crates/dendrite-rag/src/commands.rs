//! The command-line subcommands: `Command::run` hands each to a small function of its own.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dendrite_rag::rpc;
use dendrite_rag::server::Server;
use dendrite_rag::settings::Settings;
use loom_rag::engine::Engine;
use loom_rag::graph::EdgeKind;
use loom_rag::retrieve::SearchOpts;

use crate::{Command, CorpusArg, DetailArg, GraphOp, KindArg, ModeArg, print_hits, print_value};

type Done = anyhow::Result<()>;

fn open(root: &Path) -> anyhow::Result<Engine> {
    Ok(Engine::open(root, dendrite_rag::settings::resolve(root)?)?)
}

fn print_json<T: serde::Serialize>(v: &T) -> Done {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

impl Command {
    pub(crate) fn run(self, json: bool) -> Done {
        match self {
            Command::Serve => serve(),
            Command::Index {
                root,
                full,
                embed_budget_ms,
                embed_budget_tokens,
                focus,
            } => index(&root, full, embed_budget_ms, embed_budget_tokens, focus, json),
            Command::Search {
                root,
                query,
                top_k,
                mode,
                corpus,
                paths,
                min_score,
            } => search(&root, &query, top_k, mode, corpus, paths, min_score, json),
            Command::Context {
                root,
                query,
                budget,
                corpus,
                detail,
                docs_share,
            } => context(&root, &query, budget, corpus, detail, docs_share, json),
            Command::Show { root, ids } => show(&root, &ids, json),
            Command::Symbols { root, name, refs } => symbols(&root, &name, refs, json),
            Command::Grep {
                root,
                pattern,
                paths,
                max,
            } => grep(&root, &pattern, &paths, max, json),
            Command::History { root, query, top_k } => history(&root, &query, top_k, json),
            Command::Graph { op } => graph(op),
            Command::Sync { root } => sync(&root, json),
            #[cfg(feature = "postgres")]
            Command::Push { root, dry_run } => push(&root, dry_run, json),
            Command::Pull { root } => pull(&root, json),
            Command::Status { root } => status(&root, json),
            Command::Embedders => embedders(json),
            Command::Settings { root } => settings(&root, json),
            Command::Purge { root } => purge(&root),
        }
    }
}

fn show(root: &Path, ids: &[String], json: bool) -> Done {
    print_hits(&open(root)?.chunks(ids)?, json, true)
}

fn sync(root: &Path, json: bool) -> Done {
    let pending = open(root)?.sync()?;
    print_value(&serde_json::json!({ "pending": pending }), json)
}

#[cfg(feature = "postgres")]
fn push(root: &Path, dry_run: bool, json: bool) -> Done {
    print_value(&open(root)?.push(dry_run)?, json)
}

fn pull(root: &Path, json: bool) -> Done {
    print_value(&open(root)?.pull()?, json)
}

fn status(root: &Path, json: bool) -> Done {
    print_value(&open(root)?.status()?, json)
}

fn purge(root: &Path) -> Done {
    open(root)?.purge()?;
    println!("removed");
    Ok(())
}

fn serve() -> Done {
    rpc::serve(
        &mut Server::per_project(),
        io::BufReader::new(io::stdin()),
        io::stdout().lock(),
    )?;
    Ok(())
}

fn index(
    root: &Path,
    full: bool,
    embed_budget_ms: Option<u64>,
    embed_budget_tokens: Option<usize>,
    focus: Option<String>,
    json: bool,
) -> Done {
    let budget = embed_budget_ms.map(Duration::from_millis);
    print_value(
        &open(root)?.index_focused_budgeted(full, budget, embed_budget_tokens, focus.as_deref())?,
        json,
    )
}

#[allow(clippy::too_many_arguments)]
fn search(
    root: &Path,
    query: &str,
    top_k: usize,
    mode: ModeArg,
    corpus: CorpusArg,
    paths: Vec<String>,
    min_score: Option<f32>,
    json: bool,
) -> Done {
    let opts = SearchOpts {
        top_k,
        mode: mode.into(),
        corpus: corpus.into(),
        paths,
        min_score,
        offset: 0,
        weights: None,
    };
    print_hits(&open(root)?.search(query, &opts)?, json, false)
}

fn context(
    root: &Path,
    query: &str,
    budget: usize,
    corpus: CorpusArg,
    detail: DetailArg,
    docs_share: Option<f32>,
    json: bool,
) -> Done {
    let share = Settings::load(root)?.ask.docs_share(docs_share);
    let pack = open(root)?.context_shared(query, budget, corpus.into(), detail.into(), share)?;
    if !json {
        println!("~{} tokens in {} spans\n", pack.tokens, pack.spans.len());
    }
    print_hits(&pack.spans, json, true)
}

fn symbols(root: &Path, name: &str, refs: bool, json: bool) -> Done {
    let e = open(root)?;
    let hits = if refs {
        e.references(name)?
    } else {
        e.definitions(name)?
    };
    print_hits(&hits, json, false)
}

fn grep(root: &Path, pattern: &str, paths: &[String], max: usize, json: bool) -> Done {
    let matches = open(root)?.grep(pattern, paths, max)?;
    if json {
        return print_json(&matches);
    }
    for m in matches {
        println!("{}:{}: {}", m.path, m.line, m.text);
    }
    Ok(())
}

fn history(root: &Path, query: &str, top_k: usize, json: bool) -> Done {
    let hits = open(root)?.search_history(query, top_k)?;
    if json {
        return print_json(&hits);
    }
    for h in hits {
        let path = if h.path.is_empty() {
            String::new()
        } else {
            format!("  {}", h.path)
        };
        println!(
            "{:.2}  {} {}{path}",
            h.score,
            &h.sha[..h.sha.len().min(10)],
            h.subject
        );
    }
    Ok(())
}

fn graph_root(op: &GraphOp) -> PathBuf {
    match op {
        GraphOp::Communities { root, .. }
        | GraphOp::GodNodes { root, .. }
        | GraphOp::Path { root, .. }
        | GraphOp::Explain { root, .. } => root.clone(),
    }
}

fn graph(op: GraphOp) -> Done {
    let mut e = open(&graph_root(&op))?;
    e.index_with(false, Some(Duration::ZERO))?;
    let value = match op {
        GraphOp::Communities { limit, min_size, .. } => {
            serde_json::to_value(e.graph_communities(limit, min_size)?)?
        }
        GraphOp::GodNodes {
            min_in_degree, limit, ..
        } => serde_json::to_value(e.graph_god_nodes(min_in_degree, limit)?)?,
        GraphOp::Path {
            from,
            to,
            kinds,
            max_hops,
            ..
        } => graph_path(&e, &from, &to, kinds, max_hops)?,
        GraphOp::Explain { target, .. } => serde_json::to_value(e.graph_explain(&target)?)?,
    };
    print_json(&value)
}

fn graph_path(
    e: &Engine,
    from: &str,
    to: &str,
    kinds: Vec<KindArg>,
    max_hops: usize,
) -> anyhow::Result<serde_json::Value> {
    let kinds: Vec<EdgeKind> = kinds.into_iter().map(Into::into).collect();
    Ok(serde_json::to_value(e.graph_path(from, to, &kinds, max_hops)?)?)
}

fn embedders(json: bool) -> Done {
    let backends = loom_rag::embed::backends();
    if json {
        return print_json(&backends);
    }
    for b in backends {
        println!(
            "{:<10} {:<13} {:<34} {}",
            b.backend,
            if b.compiled { "available" } else { "not compiled" },
            b.default_model.unwrap_or("-"),
            b.description
        );
    }
    Ok(())
}

fn settings(root: &Path, json: bool) -> Done {
    let s = Settings::load(root)?;
    if json {
        return print_json(&s);
    }
    let file = Settings::path(root);
    let found = if file.is_file() {
        "found"
    } else {
        "not found, defaults"
    };
    println!("# {} ({found})", file.display());
    println!(
        "# model in effect: {} via {}",
        s.embed.model(),
        s.embed.backend.as_str()
    );
    print!("{}", toml::to_string_pretty(&s)?);
    Ok(())
}
