//! Configuration types shared by the engine and the programs that configure it.

use serde::{Deserialize, Serialize};

/// Optional LLM layer for graph community summaries and concepts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GraphLlmSettings {
    pub enabled: bool,
    pub model: Option<String>,
    pub summaries: bool,
    pub concepts: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VectorIndexChoice {
    #[default]
    Auto,
    Flat,
    Hnsw,
}

impl VectorIndexChoice {
    pub const ALL: [VectorIndexChoice; 3] = [
        VectorIndexChoice::Auto,
        VectorIndexChoice::Flat,
        VectorIndexChoice::Hnsw,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Flat => "flat",
            Self::Hnsw => "hnsw",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "flat" => Some(Self::Flat),
            "hnsw" => Some(Self::Hnsw),
            _ => None,
        }
    }
}
