//! Optional cross-encoder reranking (`docs/specs/retrieval.md` § Optional rerank).

use std::time::Duration;

#[cfg(feature = "candle")]
use candle_core::{DType, Device, Module, Tensor};
#[cfg(feature = "candle")]
use candle_nn::{Linear, VarBuilder, linear};
#[cfg(feature = "candle")]
use candle_transformers::models::{bert, xlm_roberta};
use serde::{Deserialize, Serialize};
#[cfg(feature = "candle")]
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

use crate::embed::{EmbedError, Result};

/// Scores `(query, document)` pairs; higher is more relevant.
pub trait Reranker: Send + Sync {
    fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>>;
}

/// Which cross-encoder backend to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RerankBackend {
    /// Cohere-style `POST {url}/v1/rerank` (e.g. LiteLLM gateway).
    #[default]
    #[serde(alias = "litellm", alias = "openai", alias = "cohere")]
    Gateway,
    /// In-process cross-encoder on candle (feature `candle`).
    Candle,
    /// Reranking disabled.
    #[serde(alias = "off")]
    None,
}

impl RerankBackend {
    pub const ALL: [RerankBackend; 3] = [RerankBackend::Gateway, RerankBackend::Candle, RerankBackend::None];

    pub fn as_str(self) -> &'static str {
        match self {
            RerankBackend::Gateway => "gateway",
            RerankBackend::Candle => "candle",
            RerankBackend::None => "none",
        }
    }

    /// Parses a backend name (aliases included).
    pub fn parse(s: &str) -> Option<RerankBackend> {
        serde_json::from_value(serde_json::Value::String(s.trim().to_ascii_lowercase())).ok()
    }

    /// Whether this build can construct the backend.
    pub fn compiled(self) -> bool {
        self != RerankBackend::Candle || cfg!(feature = "candle")
    }

    /// The model used when none is configured.
    pub fn default_model(self) -> Option<&'static str> {
        match self {
            RerankBackend::Gateway => None,
            RerankBackend::Candle => Some("BAAI/bge-reranker-base"),
            RerankBackend::None => None,
        }
    }
}

/// Reranking settings (`[rerank]` in `.dendrite/rag.toml`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RerankConfig {
    pub backend: RerankBackend,
    /// Cross-encoder model; reranking is off when unset (for gateway backend).
    pub model: Option<String>,
    /// Server with a Cohere-style `/v1/rerank`; defaults to `LITELLM_PROXY_URL`, then
    /// `http://localhost:4000`.
    pub url: Option<String>,
    /// Environment variable that holds the bearer key; defaults to the LiteLLM keys.
    pub api_key_env: Option<String>,
    /// Texts per forward pass (in-process reranker only).
    pub batch: Option<usize>,
}

impl RerankConfig {
    pub fn model(&self) -> Option<String> {
        self.model
            .clone()
            .or_else(|| self.backend.default_model().map(str::to_string))
    }

    pub fn batch(&self, default: usize) -> usize {
        self.batch.filter(|&b| b > 0).unwrap_or(default)
    }
}

/// Builds the configured reranker; `Ok(None)` when reranking is disabled or unconfigured.
pub fn from_config(cfg: &RerankConfig) -> Result<Option<Box<dyn Reranker>>> {
    if !cfg.backend.compiled() {
        return Err(EmbedError::Model(format!(
            "this dendrite-rag was built without the `{0}` feature (cargo install --features {0})",
            cfg.backend.as_str()
        )));
    }
    match cfg.backend {
        RerankBackend::None => Ok(None),
        RerankBackend::Gateway => Ok(GatewayReranker::new(cfg).map(|r| Box::new(r) as Box<dyn Reranker>)),
        #[cfg(feature = "candle")]
        RerankBackend::Candle => Ok(Some(Box::new(CandleReranker::load(cfg)?))),
        #[cfg(not(feature = "candle"))]
        RerankBackend::Candle => unreachable!("compiled() checked above"),
    }
}

/// Cohere-style `POST {base}/v1/rerank`, as served by the LiteLLM gateway.
pub struct GatewayReranker {
    agent: ureq::Agent,
    url: String,
    key: Option<String>,
    model: String,
}

impl GatewayReranker {
    /// `None` when no model is configured.
    pub fn new(cfg: &RerankConfig) -> Option<Self> {
        let model = cfg.model.clone()?;
        // Same URL and key resolution as the OpenAI-compatible embedder.
        let endpoint = crate::embed::EmbedConfig {
            backend: crate::embed::Backend::Openai,
            url: cfg.url.clone(),
            api_key_env: cfg.api_key_env.clone(),
            ..Default::default()
        };
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            .http_status_as_error(false)
            .build()
            .into();
        Some(Self {
            agent,
            url: endpoint.url().unwrap_or_default() + "/v1/rerank",
            key: endpoint.api_key(),
            model,
        })
    }
}

#[derive(Serialize)]
struct Req<'a> {
    model: &'a str,
    query: &'a str,
    documents: &'a [String],
    top_n: usize,
}

#[derive(Deserialize)]
struct Item {
    index: usize,
    relevance_score: f32,
}

#[derive(Deserialize)]
struct Resp {
    results: Vec<Item>,
}

/// Maps a rerank response onto per-document scores (documents missing from it get -inf).
fn scores(resp: Resp, n: usize) -> Result<Vec<f32>> {
    let mut out = vec![f32::NEG_INFINITY; n];
    for item in resp.results {
        *out.get_mut(item.index)
            .ok_or_else(|| EmbedError::Response(format!("rerank index {} out of range", item.index)))? =
            item.relevance_score;
    }
    Ok(out)
}

impl Reranker for GatewayReranker {
    fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>> {
        let mut req = self.agent.post(&self.url);
        if let Some(k) = &self.key {
            req = req.header("Authorization", &format!("Bearer {k}"));
        }
        let mut resp = req
            .send_json(Req {
                model: &self.model,
                query,
                documents,
                top_n: documents.len(),
            })
            .map_err(|e| EmbedError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(EmbedError::Http(format!(
                "{status}: {}",
                body.chars().take(300).collect::<String>()
            )));
        }
        let body: Resp = resp
            .body_mut()
            .read_json()
            .map_err(|e| EmbedError::Response(e.to_string()))?;
        scores(body, documents.len())
    }
}

#[cfg(feature = "candle")]
const MAX_TOKENS: usize = 512;

#[cfg(feature = "candle")]
fn model_err(e: impl std::fmt::Display) -> EmbedError {
    EmbedError::Model(e.to_string())
}

/// The cross-encoder architectures a reranker checkpoint can use, by `model_type`.
#[cfg(feature = "candle")]
enum CrossEncoder {
    /// `xlm-roberta`: the BGE rerankers. Positions start after the padding id, as in
    /// RoBERTa, which candle's `xlm_roberta` model implements.
    XlmRoberta(xlm_roberta::XLMRobertaForSequenceClassification),
    /// `bert`: MiniLM-style cross-encoders with a linear head on the pooled `[CLS]` token.
    Bert {
        model: bert::BertModel,
        pooler: Linear,
        classifier: Linear,
    },
}

/// In-process cross-encoder on candle (e.g. `BAAI/bge-reranker-base`).
#[cfg(feature = "candle")]
pub struct CandleReranker {
    model: CrossEncoder,
    tokenizer: Tokenizer,
    device: Device,
    id: String,
    batch: usize,
}

#[cfg(feature = "candle")]
impl CandleReranker {
    pub fn load(cfg: &RerankConfig) -> Result<Self> {
        let model = cfg
            .model()
            .ok_or_else(|| EmbedError::Model("candle reranker requires a model".into()))?;
        let (config_path, tokenizer_path, weights) = crate::embed::candle::model_files(&model)?;
        let config_text = std::fs::read_to_string(&config_path).map_err(model_err)?;
        let model_type = serde_json::from_str::<serde_json::Value>(&config_text)
            .map_err(model_err)?
            .get("model_type")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let device = crate::embed::candle::device()?;
        // SAFETY: the weights file is not modified while it is mapped.
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[weights], DType::F32, &device) }
            .map_err(model_err)?;
        let parse = |e: serde_json::Error| EmbedError::Model(format!("{model} config: {e}"));
        let cross_encoder = match model_type.as_str() {
            "xlm-roberta" => {
                let c: xlm_roberta::Config = serde_json::from_str(&config_text).map_err(parse)?;
                CrossEncoder::XlmRoberta(
                    xlm_roberta::XLMRobertaForSequenceClassification::new(1, &c, vb).map_err(model_err)?,
                )
            }
            "bert" => {
                let c: bert::Config = serde_json::from_str(&config_text).map_err(parse)?;
                CrossEncoder::Bert {
                    model: bert::BertModel::load(vb.pp("bert"), &c).map_err(model_err)?,
                    pooler: linear(c.hidden_size, c.hidden_size, vb.pp("bert.pooler.dense"))
                        .map_err(model_err)?,
                    classifier: linear(c.hidden_size, 1, vb.pp("classifier")).map_err(model_err)?,
                }
            }
            other => {
                return Err(EmbedError::Model(format!(
                    "{model}: unsupported cross-encoder model_type {other:?} (xlm-roberta or bert)"
                )));
            }
        };

        let mut tokenizer = Tokenizer::from_file(&tokenizer_path).map_err(model_err)?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: MAX_TOKENS,
                ..Default::default()
            }))
            .map_err(model_err)?;
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            ..Default::default()
        }));

        Ok(Self {
            model: cross_encoder,
            tokenizer,
            device,
            id: format!("candle:{model}"),
            batch: cfg.batch(16),
        })
    }

    pub fn model_id(&self) -> &str {
        &self.id
    }

    fn forward(&self, pairs: &[(String, String)]) -> Result<Vec<f32>> {
        let enc = self
            .tokenizer
            .encode_batch(pairs.to_vec(), true)
            .map_err(model_err)?;
        let rows = |f: &dyn Fn(&tokenizers::Encoding) -> &[u32]| -> Result<Tensor> {
            let data: Vec<Tensor> = enc
                .iter()
                .map(|e| Tensor::new(f(e), &self.device))
                .collect::<candle_core::Result<_>>()
                .map_err(model_err)?;
            Tensor::stack(&data, 0).map_err(model_err)
        };
        let ids = rows(&|e| e.get_ids())?;
        let mask = rows(&|e| e.get_attention_mask())?;
        let logits = match &self.model {
            CrossEncoder::XlmRoberta(m) => {
                let types = ids.zeros_like().map_err(model_err)?;
                m.forward(&ids, &mask, &types).map_err(model_err)?
            }
            CrossEncoder::Bert {
                model,
                pooler,
                classifier,
            } => {
                let types = rows(&|e| e.get_type_ids())?;
                let hidden = model.forward(&ids, &types, Some(&mask)).map_err(model_err)?;
                let cls = hidden
                    .narrow(1, 0, 1)
                    .and_then(|t| t.squeeze(1))
                    .map_err(model_err)?;
                let pooled = pooler.forward(&cls).and_then(|t| t.tanh()).map_err(model_err)?;
                classifier.forward(&pooled).map_err(model_err)?
            }
        };
        logits
            .flatten_all()
            .and_then(|t| t.to_dtype(DType::F32))
            .and_then(|t| t.to_vec1::<f32>())
            .map_err(model_err)
    }
}

#[cfg(feature = "candle")]
impl Reranker for CandleReranker {
    fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>> {
        if documents.is_empty() {
            return Ok(Vec::new());
        }
        let q = crate::embed::truncate(query);
        let pairs: Vec<(String, String)> = documents
            .iter()
            .map(|d| (q.to_string(), crate::embed::truncate(d).to_string()))
            .collect();
        let mut out = Vec::with_capacity(documents.len());
        for chunk in pairs.chunks(self.batch) {
            out.extend(self.forward(chunk)?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_results_by_index() {
        let resp: Resp = serde_json::from_str(
            r#"{"results":[{"index":2,"relevance_score":0.9},{"index":0,"relevance_score":0.1}]}"#,
        )
        .unwrap();
        assert_eq!(scores(resp, 3).unwrap(), vec![0.1, f32::NEG_INFINITY, 0.9]);
        let bad: Resp = serde_json::from_str(r#"{"results":[{"index":5,"relevance_score":1}]}"#).unwrap();
        assert!(scores(bad, 3).is_err());
    }

    #[test]
    fn rerank_backend_parsing_and_aliases() {
        assert_eq!(RerankBackend::parse("gateway"), Some(RerankBackend::Gateway));
        assert_eq!(RerankBackend::parse("litellm"), Some(RerankBackend::Gateway));
        assert_eq!(RerankBackend::parse("openai"), Some(RerankBackend::Gateway));
        assert_eq!(RerankBackend::parse("candle"), Some(RerankBackend::Candle));
        assert_eq!(RerankBackend::parse("none"), Some(RerankBackend::None));
        assert_eq!(RerankBackend::parse("off"), Some(RerankBackend::None));
        assert_eq!(RerankBackend::parse("word2vec"), None);
        for b in RerankBackend::ALL {
            assert_eq!(RerankBackend::parse(b.as_str()), Some(b));
        }
    }

    #[test]
    fn default_config_produces_no_reranker() {
        let cfg = RerankConfig::default();
        assert_eq!(cfg.backend, RerankBackend::Gateway);
        assert!(from_config(&cfg).unwrap().is_none());
    }

    #[test]
    fn none_backend_produces_no_reranker() {
        let cfg = RerankConfig {
            backend: RerankBackend::None,
            model: Some("BAAI/bge-reranker-base".into()),
            ..Default::default()
        };
        assert!(from_config(&cfg).unwrap().is_none());
    }

    #[test]
    fn uncompiled_backend_reports_error() {
        if !cfg!(feature = "candle") {
            let cfg = RerankConfig {
                backend: RerankBackend::Candle,
                ..Default::default()
            };
            let err = from_config(&cfg).err().expect("must fail without candle feature");
            assert!(err.to_string().contains("candle"), "{err}");
        }
    }

    #[cfg(feature = "candle")]
    #[test]
    fn candle_reranker_rejects_missing_model_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), r#"{"model_type":"bert"}"#).unwrap();
        let err = CandleReranker::load(&RerankConfig {
            backend: RerankBackend::Candle,
            model: Some(dir.path().to_str().unwrap().into()),
            ..Default::default()
        })
        .err()
        .unwrap();
        assert!(
            err.to_string().contains("tokenizer.json") || err.to_string().contains("safetensors"),
            "{err}"
        );
    }

    /// Downloads BAAI/bge-reranker-base once: `DENDRITE_RAG_LIVE_CANDLE=1 cargo test
    /// --features candle -- --ignored`. Checks that relevant documents score higher than irrelevant ones.
    #[cfg(feature = "candle")]
    #[test]
    #[ignore]
    fn live_candle_bge_reranker_scores() {
        if std::env::var("DENDRITE_RAG_LIVE_CANDLE").is_err() {
            return;
        }
        let r = CandleReranker::load(&RerankConfig {
            backend: RerankBackend::Candle,
            model: Some("BAAI/bge-reranker-base".into()),
            ..Default::default()
        })
        .unwrap();
        let query = "what is panda?";
        let docs = vec![
            "hi".to_string(),
            "The giant panda (Ailuropoda melanoleuca) is a bear species endemic to China.".to_string(),
        ];
        let scores = r.rerank(query, &docs).unwrap();
        assert_eq!(scores.len(), 2);
        assert!(
            scores[1] > scores[0],
            "relevant passage must score higher: {scores:?}"
        );
        assert!(
            scores[0] < 0.0,
            "unrelated text must score negative: {}",
            scores[0]
        );
        assert!(
            scores[1] > 0.0,
            "relevant passage must score positive: {}",
            scores[1]
        );
        // Logits of the reference model (transformers, PyTorch, CPU, f32).
        for (got, want) in scores.iter().zip([-8.1544, 4.2618]) {
            assert!((got - want).abs() < 0.05, "{scores:?} differs from the reference");
        }
    }
}
