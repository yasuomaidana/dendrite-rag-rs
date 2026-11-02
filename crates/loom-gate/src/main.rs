//! `loom-gate`: quality gates for CI and agents (`docs/specs/quality.md`).
//!
//!     loom-gate loc    [--root .] [--config .github/dendrite/directives.toml] [--format text|json|github]
//!     loom-gate clones [--root .] [--config ...] [--format ...] [--min-lines N] [--max-percent P]
//!     loom-gate complexity [--root .] [--config ...] [--format ...] [--max-cyclomatic N] [--max-cognitive N]
//!     loom-gate structure [--root .] [--config ...] [--format ...] [--max-files N] [--max-lines N] [--max-prefix-group N]
//!     loom-gate rstest [--root .] [--format ...]
//!
//! Exit status: 0 the check passed (warnings allowed), 1 a gate failed, 2 the tool could not run.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use loom_rag::quality::config::DIRECTIVES_PATH;
use loom_rag::quality::{
    CloneReport, ComplexityConfig, ComplexityReport, DupConfig, LocConfig, LocReport, RstestReport,
    StructureConfig, StructureReport, check_clones, check_complexity, check_loc, check_rstest,
    check_structure,
};

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
    /// Fail when a function is more complex than the limits (cyclomatic and cognitive).
    Complexity(ComplexityArgs),
    /// Fail when a directory is a pile of files: too many modules, too many lines, or a sub-module's worth of
    /// files sharing a name prefix.
    Structure(StructureArgs),
    /// Fail when Rust tests repeat each other or set up through a helper that should be an rstest fixture.
    Rstest(RstestArgs),
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Text,
    Json,
    Github,
}

/// What every gate takes: where the project is, which directives to read and how to print.
#[derive(clap::Args)]
struct Common {
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Directives file with the gate's limits (default: `<root>/.github/dendrite/directives.toml`).
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "text")]
    format: Format,
    /// Extra glob (relative to the root) of files not checked; repeatable.
    #[arg(long)]
    exclude: Vec<String>,
}

#[derive(clap::Args)]
struct LocArgs {
    #[command(flatten)]
    common: Common,
    /// Override `loc.max_production_lines`.
    #[arg(long)]
    max: Option<usize>,
    /// Override `loc.warn_at`.
    #[arg(long)]
    warn_at: Option<usize>,
}

#[derive(clap::Args)]
struct CloneArgs {
    #[command(flatten)]
    common: Common,
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
}

#[derive(clap::Args)]
struct ComplexityArgs {
    #[command(flatten)]
    common: Common,
    /// Override `complexity.max_cyclomatic`.
    #[arg(long)]
    max_cyclomatic: Option<u32>,
    /// Override `complexity.max_cognitive`.
    #[arg(long)]
    max_cognitive: Option<u32>,
}

#[derive(clap::Args)]
struct StructureArgs {
    #[command(flatten)]
    common: Common,
    /// Override `structure.max_files_per_dir`.
    #[arg(long)]
    max_files: Option<usize>,
    /// Override `structure.max_dir_production_lines`.
    #[arg(long)]
    max_lines: Option<usize>,
    /// Override `structure.max_prefix_group`.
    #[arg(long)]
    max_prefix_group: Option<usize>,
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

/// A gate's limits: the directives file's (`--config`, or the project's own), changed by the flags, and valid.
fn configured<C>(
    common: &Common,
    load: fn(&Path) -> anyhow::Result<C>,
    validate: fn(&C) -> anyhow::Result<()>,
    change: impl FnOnce(&mut C),
) -> anyhow::Result<C> {
    let file = common
        .config
        .clone()
        .unwrap_or_else(|| common.root.join(DIRECTIVES_PATH));
    let mut cfg = load(&file)?;
    change(&mut cfg);
    validate(&cfg)?;
    Ok(cfg)
}

/// What every gate report can do, so the commands print and judge them the same way.
trait Render {
    fn text(&self) -> String;
    fn json(&self) -> String;
    fn github(&self) -> String;
    fn ok(&self) -> bool;
}

macro_rules! render {
    ($($report:ty),*) => {$(
        impl Render for $report {
            fn text(&self) -> String { self.to_text() }
            fn json(&self) -> String { self.to_json() }
            fn github(&self) -> String { self.to_github() }
            fn ok(&self) -> bool { self.passed() }
        }
    )*};
}

render!(
    LocReport,
    CloneReport,
    ComplexityReport,
    StructureReport,
    RstestReport
);

/// Prints `report` in `format`; `true` when its gate passed.
fn emit(report: &impl Render, format: Format) -> bool {
    match format {
        Format::Text => print!("{}", report.text()),
        Format::Json => println!("{}", report.json()),
        Format::Github => print!("{}", report.github()),
    }
    report.ok()
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
        Command::Complexity(a) => complexity(a),
        Command::Structure(a) => structure(a),
        Command::Rstest(a) => rstest(a),
    }
}

fn loc(a: LocArgs) -> anyhow::Result<bool> {
    let cfg = configured(&a.common, LocConfig::load, LocConfig::validate, |cfg| {
        cfg.max_production_lines = a.max.unwrap_or(cfg.max_production_lines);
        cfg.warn_at = a.warn_at.unwrap_or(cfg.warn_at);
        cfg.exclude.extend(a.common.exclude.clone());
    })?;
    Ok(emit(&check_loc(&a.common.root, &cfg)?, a.common.format))
}

fn clones(a: CloneArgs) -> anyhow::Result<bool> {
    let cfg = configured(&a.common, DupConfig::load, DupConfig::validate, |cfg| {
        cfg.min_lines = a.min_lines.unwrap_or(cfg.min_lines);
        cfg.min_tokens = a.min_tokens.unwrap_or(cfg.min_tokens);
        cfg.max_percent = a.max_percent.unwrap_or(cfg.max_percent);
        cfg.include_tests = a.include_tests.unwrap_or(cfg.include_tests);
    })?;
    Ok(emit(
        &check_clones(&a.common.root, &cfg, &a.common.exclude)?,
        a.common.format,
    ))
}

fn complexity(a: ComplexityArgs) -> anyhow::Result<bool> {
    let cfg = configured(
        &a.common,
        ComplexityConfig::load,
        ComplexityConfig::validate,
        |cfg| {
            cfg.exclude.extend(a.common.exclude.clone());
            cfg.max_cyclomatic = a.max_cyclomatic.unwrap_or(cfg.max_cyclomatic);
            cfg.max_cognitive = a.max_cognitive.unwrap_or(cfg.max_cognitive);
        },
    )?;
    Ok(emit(&check_complexity(&a.common.root, &cfg)?, a.common.format))
}

fn structure(a: StructureArgs) -> anyhow::Result<bool> {
    let cfg = configured(
        &a.common,
        StructureConfig::load,
        StructureConfig::validate,
        |cfg| {
            cfg.exclude.extend(a.common.exclude.clone());
            cfg.max_files_per_dir = a.max_files.unwrap_or(cfg.max_files_per_dir);
            cfg.max_dir_production_lines = a.max_lines.unwrap_or(cfg.max_dir_production_lines);
            cfg.max_prefix_group = a.max_prefix_group.unwrap_or(cfg.max_prefix_group);
        },
    )?;
    Ok(emit(&check_structure(&a.common.root, &cfg)?, a.common.format))
}

fn rstest(a: RstestArgs) -> anyhow::Result<bool> {
    let report = check_rstest(&a.root, &a.exclude)?;
    Ok(emit(&report, a.format))
}
