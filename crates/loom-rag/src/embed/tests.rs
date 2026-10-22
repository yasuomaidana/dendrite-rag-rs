use super::*;

#[test]
fn normalizes_and_truncates() {
    let v = normalize(vec![3.0, 4.0]);
    assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
    assert_eq!(normalize(vec![0.0, 0.0]), vec![0.0, 0.0]);
    let long = "é".repeat(MAX_INPUT_BYTES);
    assert!(truncate(&long).len() <= MAX_INPUT_BYTES);
}

#[test]
fn backend_names_and_aliases_parse() {
    assert_eq!(Backend::parse("gateway"), Some(Backend::Openai));
    assert_eq!(Backend::parse(" OpenAI "), Some(Backend::Openai));
    assert_eq!(Backend::parse("tei"), Some(Backend::Hf));
    assert_eq!(Backend::parse("huggingface"), Some(Backend::Hf));
    assert_eq!(Backend::parse("off"), Some(Backend::None));
    assert_eq!(Backend::parse("oxillama"), Some(Backend::Oxillama));
    assert_eq!(Backend::parse("word2vec"), None);
    for b in Backend::ALL {
        assert_eq!(Backend::parse(b.as_str()), Some(b));
    }
}

#[test]
fn model_families_get_their_prefixes_and_pooling() {
    let cfg = EmbedConfig {
        backend: Backend::Openai,
        ..Default::default()
    };
    assert_eq!(cfg.prefixes().1, "search_query: ");
    let cfg = EmbedConfig {
        backend: Backend::Ollama,
        model: Some("bge-m3".into()),
        ..Default::default()
    };
    assert_eq!(cfg.prefixes(), (String::new(), String::new()));
    let cfg = EmbedConfig {
        model: Some("Qwen/Qwen3-Embedding-0.6B-GGUF".into()),
        query_prefix: Some("q: ".into()),
        ..Default::default()
    };
    assert_eq!(cfg.prefixes(), (String::new(), "q: ".into()));
    assert_eq!(cfg.pooling(), Pooling::Last);
    assert_eq!(model_pooling("BAAI/bge-base-en-v1.5"), Pooling::Cls);
    assert_eq!(model_pooling("nomic-ai/nomic-embed-text-v1.5"), Pooling::Mean);
}

#[test]
fn auto_prefers_gpu_then_tei_then_ollama() {
    let cfg = EmbedConfig {
        model: Some("nomic-embed-text".into()),
        ..Default::default()
    };
    assert_eq!(cfg.backend, Backend::Auto);
    // GPU builds and HF_TEI_ENDPOINT vary between machines; check the order that holds
    // for this build and environment.
    let gpu = cfg!(any(feature = "candle-metal", feature = "candle-cuda"));
    let tei = std::env::var("HF_TEI_ENDPOINT").is_ok_and(|v| !v.trim().is_empty());
    let (with_ollama, _) = resolve_auto(&cfg, |_| true);
    let (without, reason) = resolve_auto(&cfg, |_| false);
    if !gpu && !tei {
        assert_eq!(with_ollama.backend, Backend::Ollama);
        assert_eq!(with_ollama.model(), "nomic-embed-text");
        let fallback = if Backend::Candle.compiled() {
            Backend::Candle
        } else {
            Backend::None
        };
        assert_eq!(without.backend, fallback, "{reason}");
    }
    for c in [&with_ollama, &without] {
        assert!(!matches!(c.backend, Backend::Oxillama | Backend::Auto));
    }
}

#[test]
fn pools_token_states() {
    let t = vec![vec![1.0, 0.0], vec![3.0, 2.0]];
    assert_eq!(pool(&t, Pooling::Mean), vec![2.0, 1.0]);
    assert_eq!(pool(&t, Pooling::Cls), vec![1.0, 0.0]);
    assert_eq!(pool(&t, Pooling::Last), vec![3.0, 2.0]);
    assert!(pool(&[], Pooling::Mean).is_empty());
}

#[test]
fn missing_features_are_reported_not_substituted() {
    for b in Backend::ALL.into_iter().filter(|b| !b.compiled()) {
        let err = from_config(&EmbedConfig {
            backend: b,
            ..Default::default()
        })
        .err()
        .expect("an uncompiled backend must fail");
        assert!(err.to_string().contains(b.as_str()), "{err}");
    }
}
