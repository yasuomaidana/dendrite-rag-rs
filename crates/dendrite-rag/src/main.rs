use std::path::PathBuf;

use clap::{Parser, Subcommand};
use loom_rag::corpus::CorpusFilter;
use loom_rag::engine::Hit;
use loom_rag::graph::EdgeKind;
use loom_rag::retrieve::{Detail, Mode};

mod commands;

/// Retrieval engine for source-code repositories.
///
/// With no subcommand it serves JSON-RPC 2.0 over stdio. Each project chooses its embedding
/// backend and model in `.dendrite/rag.toml`, overridden by DENDRITE_RAG_EMBED_BACKEND,
/// DENDRITE_RAG_EMBED_MODEL, DENDRITE_RAG_EMBED_URL and the other DENDRITE_RAG_* variables;
/// `dendrite-rag embedders` lists the backends of this build.
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
        /// Stop embedding after this many tokens; later runs continue.
        #[arg(long)]
        embed_budget_tokens: Option<usize>,
        /// Under a budget, embed the chunks this question matches first.
        #[arg(long)]
        focus: Option<String>,
    },
    /// Search a project.
    Search {
        root: PathBuf,
        query: String,
        #[arg(short = 'k', long, default_value_t = 10)]
        top_k: usize,
        #[arg(long, value_enum, default_value = "hybrid")]
        mode: ModeArg,
        /// Search code, documentation (Markdown, txt, rst), or both in one ranking.
        #[arg(long, value_enum, default_value = "all")]
        corpus: CorpusArg,
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
        /// Pack code, documentation (Markdown, txt, rst), or both (each through its own
        /// pipeline, with part of the budget kept for documentation: --docs-share).
        #[arg(long, value_enum, default_value = "all")]
        corpus: CorpusArg,
        /// Text of retrieved spans with outlines of their graph neighbours (auto), text of
        /// every span (full), or outlines only (map).
        #[arg(long, value_enum, default_value = "auto")]
        detail: DetailArg,
        /// Share of an `all` pack kept for documentation, 0 to 1 (default: `[ask] docs_share`
        /// in .dendrite/rag.toml, else 0.2).
        #[arg(long, value_parser = parse_docs_share)]
        docs_share: Option<f32>,
    },
    /// The full text of chunks by id (the bodies behind `context --detail map` outlines).
    Show {
        root: PathBuf,
        #[arg(required = true)]
        ids: Vec<String>,
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
    /// Search commit messages and per-file diffs (indexes new commits first).
    History {
        root: PathBuf,
        query: String,
        #[arg(short = 'k', long, default_value_t = 10)]
        top_k: usize,
    },
    /// Code graph queries (indexes changed files first, without waiting for embeddings).
    Graph {
        #[command(subcommand)]
        op: GraphOp,
    },
    /// Send commits journaled while the shared tier was unreachable (edge mode).
    Sync { root: PathBuf },
    /// Upload the index to the shared tier, record the commit, compact the tier.
    #[cfg(feature = "postgres")]
    Push {
        root: PathBuf,
        /// Report what would change, change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Fetch every missing embedding (from the shared tier in edge mode) for offline use.
    Pull { root: PathBuf },
    /// Index statistics.
    Status { root: PathBuf },
    /// Embedding backends of this build, with their default models.
    Embedders,
    /// The settings in effect for a project (file, then environment).
    Settings { root: PathBuf },
    /// Delete a project's index.
    Purge { root: PathBuf },
}

#[derive(Subcommand)]
enum GraphOp {
    /// Communities of files, largest first.
    Communities {
        root: PathBuf,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long, default_value_t = 2)]
        min_size: usize,
    },
    /// Symbols with the most users.
    GodNodes {
        root: PathBuf,
        #[arg(long, default_value_t = loom_rag::graph::query::GOD_NODE_IN_DEGREE)]
        min_in_degree: usize,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Shortest path between two symbols or files.
    Path {
        root: PathBuf,
        from: String,
        to: String,
        /// Edge kinds to follow (repeatable): call, import, type. Default: all.
        #[arg(long = "kind", value_enum)]
        kinds: Vec<KindArg>,
        #[arg(long, default_value_t = 6)]
        max_hops: usize,
    },
    /// Callers, callees, imports and community of a symbol or file.
    Explain { root: PathBuf, target: String },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum KindArg {
    Call,
    Import,
    Type,
}

impl From<KindArg> for EdgeKind {
    fn from(k: KindArg) -> Self {
        match k {
            KindArg::Call => EdgeKind::Call,
            KindArg::Import => EdgeKind::Import,
            KindArg::Type => EdgeKind::Type,
        }
    }
}

fn parse_docs_share(s: &str) -> Result<f32, String> {
    match s.parse::<f32>() {
        Ok(v) if dendrite_rag::settings::valid_docs_share(v) => Ok(v),
        _ => Err(format!("{s:?} is not a number from 0 to 1")),
    }
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum CorpusArg {
    All,
    Code,
    Docs,
}

impl From<CorpusArg> for CorpusFilter {
    fn from(c: CorpusArg) -> Self {
        match c {
            CorpusArg::All => CorpusFilter::All,
            CorpusArg::Code => CorpusFilter::Code,
            CorpusArg::Docs => CorpusFilter::Docs,
        }
    }
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum DetailArg {
    Auto,
    Full,
    Map,
}

impl From<DetailArg> for Detail {
    fn from(d: DetailArg) -> Self {
        match d {
            DetailArg::Auto => Detail::Auto,
            DetailArg::Full => Detail::Full,
            DetailArg::Map => Detail::Map,
        }
    }
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
    cli.command.unwrap_or(Command::Serve).run(cli.json)
}
