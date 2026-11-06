//! The `rstest` gate: Rust tests must use the `rstest` crate (`check`) and the crate must declare it (`dep`).

pub mod check;
pub mod dep;

#[cfg(test)]
mod dep_tests;
#[cfg(test)]
mod tests;
