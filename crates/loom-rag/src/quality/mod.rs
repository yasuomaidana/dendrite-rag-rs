//! Code-quality gates over a source tree: production lines per file (`docs/specs/quality.md`).
//!
//! A file's *production* lines are its non-blank lines outside comments and outside test code. Test code is
//! found with tree-sitter, so a Rust `#[cfg(test)] mod tests` in the same file does not count against it.

pub mod check;
pub mod clones;
pub mod complexity;
pub mod config;
pub mod docs;
pub mod loc;
pub mod report;
pub mod rstest_gate;
pub mod structure;
pub mod syntax;

pub use check::{Status, check_loc};
pub use clones::report as clone_report;
pub use clones::{CloneReport, check_clones};
pub use complexity::{ComplexityReport, check_complexity};
pub use config::{ComplexityConfig, DupConfig, LocConfig, StructureConfig};
pub use loc::regions;
pub use loc::{LocCount, count_lines, is_test_path};
pub use report::LocReport;
pub use rstest_check::{RstestReport, check_rstest};
pub use rstest_gate::{check as rstest_check, dep as rstest_dep};
pub use structure::{StructureReport, check_structure};

#[cfg(test)]
mod tests;
