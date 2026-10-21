use std::io;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use dendrite_rag::engine::{Config, Engine, Hit};
use dendrite_rag::retrieve::{Mode, SearchOpts};
use dendrite_rag::rpc;
use dendrite_rag::server::Server;

/// Retrieval engine for source-code repositories.
///
/// With no subcommand it serves JSON-RPC 2.0 over stdio. Embeddings are configured with
/// DENDRITE_RAG_EMBED_BACKEND (gateway | model2vec | none), DENDRITE_RAG_EMBED_MODEL and
/// DENDRITE_RAG_RERANK_MODEL; the gateway is LITELLM_PROXY_URL with LITELLM_API_KEY.
#[derive(Parser)]
#[command(name = "dendrite-rag", version)]
struct Cli {
    /// Print JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Serve JSON-RPC 2.0 over stdio (the default).
    Serve,
    /// Index a project incrementally (or from scratch with --full).
    Index {
        root: PathBuf,
        #[arg(long)]
        full: bool,
        /// Stop embedding after this many milliseconds; later runs continue.
        #[arg(long)]
        embed_budget_ms: Option<u64>,
    },
    /// Search a project.
    Search {
        root: PathBuf,
        query: String,
        #[arg(short = 'k', long, default_value_t = 10)]
        top_k: usize,
        #[arg(long, value_enum, default_value = "hybrid")]
        mode: ModeArg,
        /// Glob over project-relative paths (repeatable).
        #[arg(long = "path")]
        paths: Vec<String>,
        #[arg(long)]
        min_score: Option<f32>,
    },
    /// Build a token-budgeted context pack.
    Context {
        root: PathBuf,
        query: String,
        #[arg(long, default_value_t = 4000)]
        budget: usize,
    },
    /// Where a symbol is defined (or referenced, with --refs).
    Symbols {
        root: PathBuf,
        name: String,
        #[arg(long)]
        refs: bool,
    },
    /// Regex search over indexable files.
    Grep {
        root: PathBuf,
        pattern: String,
        #[arg(long = "path")]
        paths: Vec<String>,
        #[arg(long, default_value_t = 200)]
        max: usize,
    },
    /// Index statistics.
    Status { root: PathBuf },
    /// Delete a project's index.
    Purge { root: PathBuf },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum ModeArg {
    Hybrid,
    Lexical,
    Dense,
}

impl From<ModeArg> for Mode {
    fn from(m: ModeArg) -> Self {
        match m {
            ModeArg::Hybrid => Mode::Hybrid,
            ModeArg::Lexical => Mode::Lexical,
            ModeArg::Dense => Mode::Dense,
        }
    }
}

fn print_hits(hits: &[Hit], json: bool, with_content: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(hits)?);
        return Ok(());
    }
    for h in hits {
        let scope = if h.scope.is_empty() {
            String::new()
        } else {
            format!("  {}", h.scope.join(" › "))
        };
        println!(
            "{:.4}  {}:{}-{}{scope}",
            h.score, h.path, h.start_line, h.end_line
        );
        if with_content {
            for line in h.content.lines() {
                println!("    {line}");
            }
            println!();
        }
    }
    Ok(())
}

fn print_value<T: serde::Serialize + std::fmt::Debug>(v: &T, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(v)?);
    } else {
        println!("{v:#?}");
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = Config::from_env();
    let open = |root: &PathBuf| Engine::open(root, config.clone());
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => rpc::serve(
            &mut Server::new(config.clone()),
            io::stdin().lock(),
            io::stdout().lock(),
        )?,
        Command::Index {
            root,
            full,
            embed_budget_ms,
        } => {
            let budget = embed_budget_ms.map(std::time::Duration::from_millis);
            print_value(&open(&root)?.index_with(full, budget)?, cli.json)?
        }
        Command::Search {
            root,
            query,
            top_k,
            mode,
            paths,
            min_score,
        } => {
            let opts = SearchOpts {
                top_k,
                mode: mode.into(),
                paths,
                min_score,
                offset: 0,
            };
            print_hits(&open(&root)?.search(&query, &opts)?, cli.json, false)?;
        }
        Command::Context { root, query, budget } => {
            let pack = open(&root)?.context(&query, budget)?;
            if !cli.json {
                println!("~{} tokens in {} spans\n", pack.tokens, pack.spans.len());
            }
            print_hits(&pack.spans, cli.json, true)?;
        }
        Command::Symbols { root, name, refs } => {
            let e = open(&root)?;
            let hits = if refs {
                e.references(&name)?
            } else {
                e.definitions(&name)?
            };
            print_hits(&hits, cli.json, false)?;
        }
        Command::Grep {
            root,
            pattern,
            paths,
            max,
        } => {
            let matches = open(&root)?.grep(&pattern, &paths, max)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&matches)?);
            } else {
                for m in matches {
                    println!("{}:{}: {}", m.path, m.line, m.text);
                }
            }
        }
        Command::Status { root } => print_value(&open(&root)?.status()?, cli.json)?,
        Command::Purge { root } => {
            open(&root)?.purge()?;
            println!("removed");
        }
    }
    Ok(())
}
