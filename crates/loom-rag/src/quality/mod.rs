//! Code-quality gates over a source tree: production lines per file (`docs/specs/quality.md`).
//!
//! A file's *production* lines are its non-blank lines outside comments and outside test code. Test code is
//! found with tree-sitter, so a Rust `#[cfg(test)] mod tests` in the same file does not count against it.

pub mod check;
pub mod clone_report;
pub mod clones;
pub mod config;
pub mod loc;
pub mod regions;
pub mod report;
pub mod rstest_check;
pub mod rstest_dep;
pub mod syntax;

pub use check::{Status, check_loc};
pub use clones::{CloneReport, check_clones};
pub use config::{DupConfig, LocConfig};
pub use loc::{LocCount, count_lines, is_test_path};
pub use report::LocReport;
pub use rstest_check::{RstestReport, check_rstest};

#[cfg(test)]
mod clone_tests;
#[cfg(test)]
mod rstest_dep_tests;
#[cfg(test)]
mod rstest_tests;
#[cfg(test)]
mod tests;
