use std::collections::HashMap;

use super::*;

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let m: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |k| m.get(k).cloned()
}

const FILE_TEXT: &str = r#"
[embed]
backend = "ollama"
model = "jina/jina-embeddings-v2-base-code"
url = "http://gpu-box:11434"
pooling = "mean"

[rerank]
model = "bge-reranker-v2-m3"

[index]
max_file_bytes = 2048
"#;

#[test]
fn reads_the_project_file() {
    let s = Settings::parse(FILE_TEXT).unwrap();
    assert_eq!(s.embed.backend, Backend::Ollama);
    assert_eq!(s.embed.url(), Some("http://gpu-box:11434".into()));
    let cfg = s.config();
    assert_eq!(cfg.walk.max_file_bytes, 2048);
    assert_eq!(cfg.rerank.model.as_deref(), Some("bge-reranker-v2-m3"));
}

#[test]
fn rejects_unknown_keys_and_backends() {
    assert!(Settings::parse("[embed]\nbakend = \"ollama\"\n").is_err());
    assert!(Settings::parse("[embed]\nbackend = \"word2vec\"\n").is_err());
    let mut s = Settings::default();
    assert!(
        s.apply_env(env(&[("DENDRITE_RAG_EMBED_BACKEND", "word2vec")]))
            .is_err()
    );
}

#[test]
fn environment_overrides_the_file() {
    let mut s = Settings::parse(FILE_TEXT).unwrap();
    s.apply_env(env(&[("DENDRITE_RAG_EMBED_MODEL", "nomic-embed-text")]))
        .unwrap();
    assert_eq!(s.embed.backend, Backend::Ollama);
    assert_eq!(s.embed.model(), "nomic-embed-text");
    assert_eq!(s.embed.url(), Some("http://gpu-box:11434".into()));

    // Another backend: the file's ollama model and URL no longer apply.
    let mut s = Settings::parse(FILE_TEXT).unwrap();
    s.apply_env(env(&[
        ("DENDRITE_RAG_EMBED_BACKEND", "hf"),
        ("DENDRITE_RAG_EMBED_POOLING", "CLS"),
    ]))
    .unwrap();
    assert_eq!(s.embed.backend, Backend::Hf);
    assert_eq!(s.embed.model(), "BAAI/bge-base-en-v1.5");
    assert_eq!(s.embed.url, None);
    assert_eq!(s.embed.pooling, Some(Pooling::Cls));
}

#[test]
fn missing_file_means_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let s = Settings::load(dir.path()).unwrap();
    if std::env::var("DENDRITE_RAG_EMBED_BACKEND").is_err() {
        assert_eq!(s.embed.backend, Backend::Auto);
    }
}

#[test]
fn parses_query_cache_and_applies_env() {
    let toml_with_cache = r#"
[embed]
backend = "ollama"

[index]
query_cache = 512
"#;
    let mut s = Settings::parse(toml_with_cache).unwrap();
    assert_eq!(s.index.query_cache, Some(512));
    assert_eq!(s.config().query_cache, 512);

    // Environment overrides file.
    s.apply_env(env(&[("DENDRITE_RAG_QUERY_CACHE", "0")])).unwrap();
    assert_eq!(s.index.query_cache, Some(0));
    assert_eq!(s.config().query_cache, 0);

    // Env on default settings.
    let mut s2 = Settings::default();
    s2.apply_env(env(&[("DENDRITE_RAG_QUERY_CACHE", "1024")]))
        .unwrap();
    assert_eq!(s2.index.query_cache, Some(1024));
    assert_eq!(s2.config().query_cache, 1024);
}

#[test]
fn parses_rerank_backend_and_applies_env() {
    let toml_with_rerank = r#"
[embed]
backend = "none"

[rerank]
backend = "candle"
model = "BAAI/bge-reranker-base"
"#;
    let mut s = Settings::parse(toml_with_rerank).unwrap();
    assert_eq!(s.rerank.backend, RerankBackend::Candle);
    assert_eq!(s.rerank.model.as_deref(), Some("BAAI/bge-reranker-base"));

    // Environment overrides file.
    s.apply_env(env(&[("DENDRITE_RAG_RERANK_BACKEND", "none")]))
        .unwrap();
    assert_eq!(s.rerank.backend, RerankBackend::None);
    // Changed backend drops file model
    assert_eq!(s.rerank.model, None);

    // Reject unknown backend
    let mut s2 = Settings::default();
    assert!(
        s2.apply_env(env(&[("DENDRITE_RAG_RERANK_BACKEND", "word2vec")]))
            .is_err()
    );
}

#[test]
fn graph_resolution_from_file_and_env() {
    let mut s = Settings::parse("[graph]\nresolution = 0.5\n").unwrap();
    assert_eq!(s.config().graph_resolution, 0.5);
    s.apply_env(env(&[("DENDRITE_RAG_GRAPH_RESOLUTION", "2")]))
        .unwrap();
    assert_eq!(s.config().graph_resolution, 2.0);
    assert!(
        s.apply_env(env(&[("DENDRITE_RAG_GRAPH_RESOLUTION", "0")]))
            .is_err()
    );
}

#[test]
fn graph_llm_settings_and_env_overrides() {
    let toml_text = r#"
[graph.llm]
enabled = true
model = "deepseek-chat"
summaries = true
concepts = false
"#;
    let mut s = Settings::parse(toml_text).unwrap();
    assert!(s.graph.llm.enabled);
    assert_eq!(s.graph.llm.model.as_deref(), Some("deepseek-chat"));
    assert!(s.graph.llm.summaries);
    assert!(!s.graph.llm.concepts);
    assert!(s.validate().is_ok());

    // Environment overrides
    s.apply_env(env(&[
        ("DENDRITE_RAG_GRAPH_LLM_ENABLED", "false"),
        ("DENDRITE_RAG_GRAPH_LLM_MODEL", "gpt-4o-mini"),
        ("DENDRITE_RAG_GRAPH_LLM_SUMMARIES", "0"),
        ("DENDRITE_RAG_GRAPH_LLM_CONCEPTS", "true"),
    ]))
    .unwrap();
    assert!(!s.graph.llm.enabled);
    assert_eq!(s.graph.llm.model.as_deref(), Some("gpt-4o-mini"));
    assert!(!s.graph.llm.summaries);
    assert!(s.graph.llm.concepts);

    // Enabled without model fails validation
    let mut s_no_model = Settings::parse("[graph.llm]\nenabled = true\n").unwrap();
    assert!(s_no_model.validate().is_err());
    assert!(
        s_no_model
            .apply_env(env(&[("DENDRITE_RAG_GRAPH_LLM_ENABLED", "true")]))
            .is_err()
    );

    // Unknown keys rejected
    assert!(Settings::parse("[graph.llm]\nunknown_key = true\n").is_err());
    assert!(Settings::parse("[graph]\nunknown_key = true\n").is_err());
}

#[test]
fn parses_vector_index_and_ef_search_and_applies_env() {
    let toml_text = r#"
[index]
vector_index = "flat"
hnsw_ef_search = 128
"#;
    let mut s = Settings::parse(toml_text).unwrap();
    assert_eq!(s.index.vector_index, Some(VectorIndexChoice::Flat));
    assert_eq!(s.index.hnsw_ef_search, Some(128));
    let cfg = s.config();
    assert_eq!(cfg.vector_index, VectorIndexChoice::Flat);
    assert_eq!(cfg.hnsw_ef_search, 128);

    s.apply_env(env(&[
        ("DENDRITE_RAG_VECTOR_INDEX", "auto"),
        ("DENDRITE_RAG_HNSW_EF_SEARCH", "256"),
    ]))
    .unwrap();
    assert_eq!(s.index.vector_index, Some(VectorIndexChoice::Auto));
    assert_eq!(s.index.hnsw_ef_search, Some(256));
    assert_eq!(s.config().vector_index, VectorIndexChoice::Auto);
    assert_eq!(s.config().hnsw_ef_search, 256);

    // Unknown vector index
    let mut s2 = Settings::default();
    assert!(
        s2.apply_env(env(&[("DENDRITE_RAG_VECTOR_INDEX", "invalid")]))
            .is_err()
    );

    #[cfg(not(feature = "hnsw"))]
    {
        let mut s3 = Settings::default();
        assert!(
            s3.apply_env(env(&[("DENDRITE_RAG_VECTOR_INDEX", "hnsw")]))
                .is_err()
        );
        assert!(Settings::parse("[index]\nvector_index = \"hnsw\"\n").is_err());
    }

    #[cfg(feature = "hnsw")]
    {
        let mut s3 = Settings::default();
        assert!(
            s3.apply_env(env(&[("DENDRITE_RAG_VECTOR_INDEX", "hnsw")]))
                .is_ok()
        );
        assert!(Settings::parse("[index]\nvector_index = \"hnsw\"\n").is_ok());
    }
}

#[test]
fn fusion_weights_from_file_and_env() {
    let mut s = Settings::parse("[fusion]\ndense = 2.0\n").unwrap();
    s.apply_env(env(&[("DENDRITE_RAG_FUSION_COMMUNITY", "0.25")]))
        .unwrap();
    let w = s
        .config()
        .fusion
        .resolve(loom_rag::retrieve::FusionWeights::default());
    assert_eq!((w.lexical, w.dense, w.symbol, w.community), (1.0, 2.0, 1.5, 0.25));
    assert!(
        s.apply_env(env(&[("DENDRITE_RAG_FUSION_LEXICAL", "-1")]))
            .is_err()
    );
    assert!(Settings::parse("[fusion]\nbogus = 1\n").is_err());
}

#[test]
fn docs_share_from_file_and_env() {
    let mut s = Settings::parse("[ask]\ndocs_share = 0.5\n").unwrap();
    s.apply_env(env(&[])).unwrap();
    assert_eq!(s.ask.docs_share(None), 0.5);
    assert_eq!(
        s.ask.docs_share(Some(0.9)),
        0.9,
        "a request overrides the project"
    );
    s.apply_env(env(&[("DENDRITE_RAG_ASK_DOCS_SHARE", "0")])).unwrap();
    assert_eq!(s.ask.docs_share(None), 0.0);
    assert_eq!(AskSettings::default().docs_share(None), DEFAULT_DOCS_SHARE);
    assert!(
        s.apply_env(env(&[("DENDRITE_RAG_ASK_DOCS_SHARE", "1.5")]))
            .is_err()
    );
    let mut s = Settings::parse("[ask]\ndocs_share = -0.1\n").unwrap();
    assert!(s.apply_env(env(&[])).is_err());
}

#[test]
fn lexical_bm25_from_file_and_environment() {
    let mut s = Settings::parse("[lexical]\nk1 = 0.9\nb = 0.4\n").unwrap();
    assert_eq!((s.lexical.k1, s.lexical.b, s.lexical.q), (0.9, 0.4, 1.0));
    s.apply_env(env(&[("DENDRITE_RAG_BM25_Q", "0.2")])).unwrap();
    assert_eq!(s.config().bm25.q, 0.2);
    assert!(s.apply_env(env(&[("DENDRITE_RAG_BM25_B", "2")])).is_err());
    assert!(
        Settings::parse("[lexical]\nk3 = 1\n").is_err(),
        "unknown keys are errors"
    );
}

#[test]
fn storage_ledger_from_file_and_environment() {
    let mut s = Settings::parse("[storage]\nledger = \"sqlite\"\n").unwrap();
    assert_eq!(s.config().storage.ledger, loom_rag::store::LedgerChoice::Sqlite);
    s.apply_env(env(&[("DENDRITE_RAG_STORAGE_LEDGER", "redb")]))
        .unwrap();
    assert_eq!(s.config().storage.ledger, loom_rag::store::LedgerChoice::Redb);
    assert!(
        s.apply_env(env(&[("DENDRITE_RAG_STORAGE_LEDGER", "lmdb")]))
            .is_err()
    );
}
