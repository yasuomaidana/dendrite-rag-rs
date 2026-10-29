//! `loom-gate`: quality gates for CI and agents (`docs/specs/quality.md`).
//!
//!     loom-gate loc [--root .] [--config .github/dendrite/directives.toml] [--format text|json|github]
//!
//! Exit status: 0 the check passed (warnings allowed), 1 a gate failed, 2 the tool could not run.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use loom_rag::quality::config::DIRECTIVES_PATH;
use loom_rag::quality::{LocConfig, check_loc};

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
