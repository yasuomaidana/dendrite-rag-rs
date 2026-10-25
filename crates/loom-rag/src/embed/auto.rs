//! `backend = "auto"`: which embedder a project without a choice gets, and why.

#[cfg(feature = "candle")]
use super::candle;
use super::{Backend, EmbedConfig};

/// GPU-capable backends first, Rust before other runtimes: the concrete configuration
/// `backend = "auto"` stands for, and why it was chosen.
///
/// 1. `candle` when built with `candle-metal` or `candle-cuda` and the device opens;
/// 2. `hf` when `HF_TEI_ENDPOINT` names a Text Embeddings Inference server (Rust, GPU);
/// 3. `ollama` when its server answers (GPU through llama.cpp);
/// 4. `openai` through the LiteLLM gateway when `LITELLM_PROXY_URL` names one that answers
///    (any HTTP status counts: the point is that something is listening), with the
///    gateway's default embedding model;
/// 5. `candle` on the CPU when built with `candle`;
/// 6. `model2vec` (static embeddings) when built with `model2vec`;
/// 7. `none`: lexical and symbol retrieval only.
///
/// oxillama is never chosen: oxillama-runtime 0.1.4 offloads only Q4_0 single-token
/// decode to the GPU, so its embeddings run on the CPU (over 3 s per chunk measured).
/// The configured model, prefixes, pooling and batch carry over to the chosen backend.
pub fn resolve_auto(
    cfg: &EmbedConfig,
    ollama_reachable: impl Fn(&str) -> bool,
    gateway_reachable: impl Fn(&str) -> bool,
) -> (EmbedConfig, String) {
    resolve_auto_in(
        cfg,
        |k| std::env::var(k).ok(),
        ollama_reachable,
        gateway_reachable,
    )
}

/// [`resolve_auto`] with the environment injected, so the order can be tested anywhere.
pub(crate) fn resolve_auto_in(
    cfg: &EmbedConfig,
    env: impl Fn(&str) -> Option<String>,
    ollama_reachable: impl Fn(&str) -> bool,
    gateway_reachable: impl Fn(&str) -> bool,
) -> (EmbedConfig, String) {
    let var = |k: &str| env(k).filter(|v| !v.trim().is_empty());
    let with = |backend: Backend, url: Option<String>| EmbedConfig {
        backend,
        url,
        ..cfg.clone()
    };
    #[cfg(feature = "candle")]
    if let Some(device) = candle::accelerator() {
        return (with(Backend::Candle, None), format!("candle on {device}"));
    }
    if let Some(tei) = var("HF_TEI_ENDPOINT") {
        let reason = format!("Text Embeddings Inference at {tei}");
        return (with(Backend::Hf, Some(tei)), reason);
    }
    let ollama = Backend::Ollama.default_url().unwrap_or_default();
    if ollama_reachable(&ollama) {
        return (with(Backend::Ollama, None), format!("Ollama at {ollama}"));
    }
    if let Some(gateway) = var("LITELLM_PROXY_URL").filter(|g| gateway_reachable(g)) {
        let reason = format!("LiteLLM gateway at {gateway}");
        return (with(Backend::Openai, Some(gateway)), reason);
    }
    if Backend::Candle.compiled() {
        return (
            with(Backend::Candle, None),
            "candle on the CPU (no GPU build, TEI server or Ollama found)".into(),
        );
    }
    if Backend::Model2vec.compiled() {
        return (
            with(Backend::Model2vec, None),
            "model2vec in-process static embeddings (fast offline fallback)".into(),
        );
    }
    (
        with(Backend::None, None),
        "no embedder available (no GPU build of candle, TEI server or Ollama): lexical and symbol retrieval only".into(),
    )
}

/// Whether an Ollama server answers at `url` within half a second.
pub fn ollama_reachable(url: &str) -> bool {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_millis(500)))
        .build()
        .into();
    agent.get(&format!("{url}/api/version")).call().is_ok()
}

/// Whether something answers HTTP at `url` within half a second, whatever the status (a
/// gateway that wants a key still answers 401).
pub fn gateway_reachable(url: &str) -> bool {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_millis(500)))
        .http_status_as_error(false)
        .build()
        .into();
    agent.get(url).call().is_ok()
}
