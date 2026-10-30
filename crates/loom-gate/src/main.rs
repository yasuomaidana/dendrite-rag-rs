//! `loom-gate`: quality gates for CI and agents (`docs/specs/quality.md`).
//!
//!     loom-gate loc    [--root .] [--config .github/dendrite/directives.toml] [--format text|json|github]
//!     loom-gate clones [--root .] [--config ...] [--format ...] [--min-lines N] [--max-percent P]
//!     loom-gate rstest [--root .] [--format ...]
//!
//! Exit status: 0 the check passed (warnings allowed), 1 a gate failed, 2 the tool could not run.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use loom_rag::quality::config::DIRECTIVES_PATH;
use loom_rag::quality::{DupConfig, LocConfig, check_clones, check_loc, check_rstest};

#[derive(Parser)]
#[command(name = "loom-gate", version, about = "Quality gates over a source tree")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Fail when a code file has more production lines than the limit (tests, comments and blanks do not count).
    Loc(LocArgs),
    /// Fail when too much code is repeated: blocks that are the same up to names and constants.
    Clones(CloneArgs),
    /// Fail when Rust tests repeat each other or set up through a helper that should be an rstest fixture.
    Rstest(RstestArgs),
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Text,
    Json,
    Github,
}

#[derive(clap::Args)]
struct LocArgs {
    /// Project root.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Directives file with the `[loc]` limits (default: `<root>/.github/dendrite/directives.toml`).
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "text")]
    format: Format,
    /// Override `loc.max_production_lines`.
    #[arg(long)]
    max: Option<usize>,
    /// Override `loc.warn_at`.
    #[arg(long)]
    warn_at: Option<usize>,
    /// Extra glob (relative to the root) of files not to count; repeatable.
    #[arg(long)]
    exclude: Vec<String>,
}

#[derive(clap::Args)]
struct CloneArgs {
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Directives file with the `[duplication]` limits (default: `<root>/.github/dendrite/directives.toml`).
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "text")]
    format: Format,
    /// Override `duplication.min_lines`.
    #[arg(long)]
    min_lines: Option<usize>,
    /// Override `duplication.min_tokens`.
    #[arg(long)]
    min_tokens: Option<usize>,
    /// Override `duplication.max_percent`.
    #[arg(long)]
    max_percent: Option<f64>,
    /// Override `duplication.include_tests`.
    #[arg(long)]
    include_tests: Option<bool>,
    /// Glob (relative to the root) of files not checked; repeatable.
    #[arg(long)]
    exclude: Vec<String>,
}

#[derive(clap::Args)]
struct RstestArgs {
    #[arg(long, default_value = ".")]
    root: PathBuf,
    #[arg(long, value_enum, default_value = "text")]
    format: Format,
    /// Glob (relative to the root) of files not checked; repeatable.
    #[arg(long)]
    exclude: Vec<String>,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("loom-gate: {e:#}");
            ExitCode::from(2)
        }
    }
}

/// Runs the command; `true` when its gate passed.
fn run(cli: Cli) -> anyhow::Result<bool> {
    match cli.command {
        Command::Loc(a) => loc(a),
        Command::Clones(a) => clones(a),
        Command::Rstest(a) => rstest(a),
    }
}

fn loc(a: LocArgs) -> anyhow::Result<bool> {
    let file = a.config.clone().unwrap_or_else(|| a.root.join(DIRECTIVES_PATH));
    let mut cfg = LocConfig::load(&file)?;
    cfg.max_production_lines = a.max.unwrap_or(cfg.max_production_lines);
    cfg.warn_at = a.warn_at.unwrap_or(cfg.warn_at);
    cfg.exclude.extend(a.exclude);
    cfg.validate()?;
    let report = check_loc(&a.root, &cfg)?;
    print!(
        "{}",
        match a.format {
            Format::Text => report.to_text(),
            Format::Json => report.to_json() + "\n",
            Format::Github => report.to_github(),
        }
    );
    Ok(report.passed())
}

fn clones(a: CloneArgs) -> anyhow::Result<bool> {
    let file = a.config.clone().unwrap_or_else(|| a.root.join(DIRECTIVES_PATH));
    let mut cfg = DupConfig::load(&file)?;
    cfg.min_lines = a.min_lines.unwrap_or(cfg.min_lines);
    cfg.min_tokens = a.min_tokens.unwrap_or(cfg.min_tokens);
    cfg.max_percent = a.max_percent.unwrap_or(cfg.max_percent);
    cfg.include_tests = a.include_tests.unwrap_or(cfg.include_tests);
    cfg.validate()?;
    let report = check_clones(&a.root, &cfg, &a.exclude)?;
    print!(
        "{}",
        match a.format {
            Format::Text => report.to_text(),
            Format::Json => report.to_json() + "\n",
            Format::Github => report.to_github(),
        }
    );
    Ok(report.passed())
}

fn rstest(a: RstestArgs) -> anyhow::Result<bool> {
    let report = check_rstest(&a.root, &a.exclude)?;
    print!(
        "{}",
        match a.format {
            Format::Text => report.to_text(),
            Format::Json => report.to_json() + "\n",
            Format::Github => report.to_github(),
        }
    );
    Ok(report.passed())
}
