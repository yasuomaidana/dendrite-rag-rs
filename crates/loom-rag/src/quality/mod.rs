//! Code-quality gates over a source tree: production lines per file (`docs/specs/quality.md`).
//!
//! A file's *production* lines are its non-blank lines outside comments and outside test code. Test code is
//! found with tree-sitter, so a Rust `#[cfg(test)] mod tests` in the same file does not count against it.

pub mod check;
pub mod config;
pub mod loc;
pub mod regions;
pub mod report;

pub use check::{Status, check_loc};
pub use config::LocConfig;
pub use loc::{LocCount, count_lines, is_test_path};
pub use report::LocReport;

#[cfg(test)]
mod tests;
