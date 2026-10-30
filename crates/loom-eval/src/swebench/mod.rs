//! SWE-bench Verified file localization evaluation suite (DRAG-54 / DRAG-55).

pub mod instance;
pub mod metrics;
pub mod output;
pub mod pool;
pub mod runner;

pub use instance::{LeakageClass, SweInstance, load_instances};
pub use metrics::{AggregatedMetrics, InstanceScore, is_gold, path_is_gold, summarize_report};
pub use pool::{PoolingStrategy, is_test_or_migration, pool_chunk_scores};
pub use runner::{SwebenchOpts, run};
