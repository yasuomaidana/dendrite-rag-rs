//! Retrieval evaluation suite: quality, tokens read, and latency against baselines.

pub mod abstain;
pub mod baselines;
pub mod contextual;
pub mod golden;
pub mod methods;
pub mod metrics;
pub mod repeat;
pub mod report;
pub mod runs;
pub mod stats;
pub mod swebench;

/// Compiles the Rust examples of the crate README as doctests, so they cannot drift from the code.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
