use super::Backend;
use serde::Serialize;

/// How one embedding backend is described when backends are listed.
#[derive(Debug, Clone, Serialize)]
pub struct BackendInfo {
    pub backend: &'static str,
    pub compiled: bool,
    pub default_model: Option<&'static str>,
    pub default_url: Option<String>,
    pub description: &'static str,
}

pub fn backends() -> Vec<BackendInfo> {
    Backend::ALL
        .iter()
        .map(|&b| BackendInfo {
            backend: b.as_str(),
            compiled: b.compiled(),
            default_model: b.default_model(),
            default_url: b.default_url(),
            description: b.describe(),
        })
        .collect()
}
