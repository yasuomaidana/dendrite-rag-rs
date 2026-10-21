//! In-process encoders on candle: BERT (bge, MiniLM, e5, …), NomicBERT
//! (nomic-embed-text-v1/v1.5) and JinaBERT (jina-embeddings-v2, and the qk-post-norm
//! variant of jina-embeddings-v2-base-code, see [`super::jina_code`]).
//!
//! The model is a hub repository (fetched once into `~/.cache/huggingface`) or a local
//! folder holding `config.json`, `tokenizer.json` and `model.safetensors`. The
//! architecture comes from `config.json`. Inference runs on the CPU, or on Metal with the
//! `candle-metal` feature.

use std::path::{Path, PathBuf};

use candle_core::{D, DType, Device, Module, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::{bert, jina_bert, nomic_bert};
use serde::Deserialize;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

use super::{EmbedConfig, EmbedError, Embedder, Pooling, Result, normalize, truncate};

/// Tokens per input; chunks are far shorter, and attention cost grows with the square.
const MAX_TOKENS: usize = 512;

enum Arch {
    Bert(bert::BertModel),
    Nomic(nomic_bert::NomicBertModel),
    /// JinaBERT takes no attention mask, so it embeds one text per pass (no padding).
    Jina(jina_bert::BertModel),
    /// JinaBERT with query/key post-norm (jina-embeddings-v2-base-code).
    JinaCode(super::jina_code::JinaCodeModel),
}

pub struct CandleEmbedder {
    arch: Arch,
    tokenizer: Tokenizer,
    device: Device,
    id: String,
    pooling: Pooling,
    document_prefix: String,
    query_prefix: String,
    batch: usize,
}

#[derive(Deserialize)]
struct Probe {
    model_type: Option<String>,
    #[serde(default)]
    architectures: Vec<String>,
    position_embedding_type: Option<String>,
}

fn model_err(e: impl std::fmt::Display) -> EmbedError {
    EmbedError::Model(e.to_string())
}

/// `config.json`, `tokenizer.json` and the safetensors weights of a folder or hub repo.
fn model_files(model: &str) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let local = Path::new(model);
    if local.is_dir() {
        return Ok((
            local.join("config.json"),
            local.join("tokenizer.json"),
            local.join("model.safetensors"),
        ));
    }
    let api = hf_hub::api::sync::ApiBuilder::from_env()
        .with_progress(false)
        .build()
        .map_err(model_err)?;
    let repo = api.model(model.to_string());
    let get = |f: &str| {
        repo.get(f)
            .map_err(|e| EmbedError::Model(format!("{model}/{f}: {e}")))
    };
    Ok((
        get("config.json")?,
        get("tokenizer.json")?,
        get("model.safetensors")?,
    ))
}

/// The device this build runs on: CUDA with `candle-cuda`, Metal with `candle-metal`,
/// otherwise the CPU.
fn device() -> Result<Device> {
    #[cfg(feature = "candle-cuda")]
    {
        Device::new_cuda(0).map_err(model_err)
    }
    #[cfg(all(feature = "candle-metal", not(feature = "candle-cuda")))]
    {
        Device::new_metal(0).map_err(model_err)
    }
    #[cfg(not(any(feature = "candle-metal", feature = "candle-cuda")))]
    {
        Ok(Device::Cpu)
    }
}

/// The GPU this build can use (`"metal"` or `"cuda"`), if it opens.
pub fn accelerator() -> Option<&'static str> {
    let gpu = if cfg!(feature = "candle-cuda") {
        "cuda"
    } else if cfg!(feature = "candle-metal") {
        "metal"
    } else {
        return None;
    };
    device().ok().map(|_| gpu)
}

impl CandleEmbedder {
    pub fn load(cfg: &EmbedConfig) -> Result<Self> {
        let model = cfg.model();
        let (config_path, tokenizer_path, weights) = model_files(&model)?;
        let config_text = std::fs::read_to_string(&config_path).map_err(model_err)?;
        let probe: Probe = serde_json::from_str(&config_text).map_err(model_err)?;
        let device = device()?;
        // SAFETY: the weights file is not modified while it is mapped.
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[weights], DType::F32, &device) }
            .map_err(model_err)?;
        let parse = |what| move |e: serde_json::Error| EmbedError::Model(format!("{what} config: {e}"));
        let jina = probe.architectures.iter().any(|a| a.starts_with("JinaBert"))
            || probe.position_embedding_type.as_deref() == Some("alibi");
        let arch = match probe.model_type.as_deref() {
            Some("nomic_bert") => {
                let c: nomic_bert::Config = serde_json::from_str(&config_text).map_err(parse("NomicBERT"))?;
                Arch::Nomic(nomic_bert::NomicBertModel::load(vb, &c).map_err(model_err)?)
            }
            Some("bert") if jina && super::jina_code::Config::is_qk_post_norm(&vb) => {
                let c: super::jina_code::Config =
                    serde_json::from_str(&config_text).map_err(parse("JinaBERT qk-post-norm"))?;
                Arch::JinaCode(super::jina_code::JinaCodeModel::load(vb, &c).map_err(model_err)?)
            }
            Some("bert") if jina => {
                let c: jina_bert::Config = serde_json::from_str(&config_text).map_err(parse("JinaBERT"))?;
                Arch::Jina(jina_bert::BertModel::new(vb, &c).map_err(model_err)?)
            }
            Some("bert") | Some("roberta") | Some("xlm-roberta") => {
                let c: bert::Config = serde_json::from_str(&config_text).map_err(parse("BERT"))?;
                Arch::Bert(bert::BertModel::load(vb, &c).map_err(model_err)?)
            }
            other => {
                return Err(EmbedError::Model(format!(
                    "{model}: model_type {other:?} is not supported by the candle backend \
                     (bert, nomic_bert or JinaBERT); use the ollama, hf or oxillama backend"
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
        let (document_prefix, query_prefix) = cfg.prefixes();
        Ok(Self {
            arch,
            tokenizer,
            device,
            id: format!("candle:{model}"),
            pooling: cfg.pooling(),
            document_prefix,
            query_prefix,
            batch: cfg.batch(16),
        })
    }

    /// One forward pass over `texts`, pooled and normalized.
    fn forward(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let enc = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
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
        let hidden = match &self.arch {
            Arch::Bert(m) => {
                let types = ids.zeros_like().map_err(model_err)?;
                m.forward(&ids, &types, Some(&mask))
            }
            Arch::Nomic(m) => m.forward(&ids, None, Some(&mask)),
            Arch::Jina(m) => m.forward(&ids),
            Arch::JinaCode(m) => m.forward(&ids, &mask),
        }
        .map_err(model_err)?;
        let pooled = pool(&hidden, &mask, self.pooling).map_err(model_err)?;
        let vectors: Vec<Vec<f32>> = pooled.to_vec2().map_err(model_err)?;
        Ok(vectors.into_iter().map(normalize).collect())
    }

    fn embed(&self, prefix: &str, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let inputs: Vec<String> = texts.iter().map(|t| format!("{prefix}{}", truncate(t))).collect();
        // Similar lengths share a batch, so little compute goes to padding.
        let mut order: Vec<usize> = (0..inputs.len()).collect();
        order.sort_by_key(|&i| inputs[i].len());
        let batch = if matches!(self.arch, Arch::Jina(_)) {
            1
        } else {
            self.batch
        };
        let mut out = vec![Vec::new(); inputs.len()];
        for group in order.chunks(batch) {
            let texts: Vec<String> = group.iter().map(|&i| inputs[i].clone()).collect();
            for (&i, v) in group.iter().zip(self.forward(&texts)?) {
                out[i] = v;
            }
        }
        Ok(out)
    }
}

/// `[batch, tokens, dim]` → `[batch, dim]`, ignoring padding.
fn pool(hidden: &Tensor, mask: &Tensor, pooling: Pooling) -> candle_core::Result<Tensor> {
    match pooling {
        Pooling::Cls => hidden.narrow(1, 0, 1)?.squeeze(1),
        Pooling::Mean => {
            let m = mask.to_dtype(hidden.dtype())?.unsqueeze(2)?;
            let sum = hidden.broadcast_mul(&m)?.sum(1)?;
            let count = m.sum(1)?.clamp(1e-9, f64::MAX)?;
            sum.broadcast_div(&count)
        }
        Pooling::Last => {
            // Padding is on the right: the last real token is at (mask sum - 1).
            let last: Vec<u32> = mask
                .to_dtype(DType::U32)?
                .sum(D::Minus1)?
                .to_vec1::<u32>()?
                .into_iter()
                .map(|n| n.saturating_sub(1))
                .collect();
            let rows = last
                .iter()
                .enumerate()
                .map(|(b, &t)| hidden.get(b)?.get(t as usize))
                .collect::<candle_core::Result<Vec<_>>>()?;
            Tensor::stack(&rows, 0)
        }
    }
}

impl Embedder for CandleEmbedder {
    fn model_id(&self) -> &str {
        &self.id
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed(&self.document_prefix, texts)
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        Ok(self.embed(&self.query_prefix, &[text.to_string()])?.remove(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pooling_ignores_padding() {
        let hidden = Tensor::new(&[[[1f32, 0.], [3., 2.], [9., 9.]]], &Device::Cpu).unwrap();
        let mask = Tensor::new(&[[1u32, 1, 0]], &Device::Cpu).unwrap();
        let v = |p| pool(&hidden, &mask, p).unwrap().to_vec2::<f32>().unwrap();
        assert_eq!(v(Pooling::Mean), vec![vec![2., 1.]]);
        assert_eq!(v(Pooling::Cls), vec![vec![1., 0.]]);
        assert_eq!(v(Pooling::Last), vec![vec![3., 2.]]);
    }

    #[test]
    fn unsupported_architectures_are_named() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), r#"{"model_type":"t5"}"#).unwrap();
        std::fs::write(dir.path().join("model.safetensors"), b"").unwrap();
        let err = CandleEmbedder::load(&EmbedConfig {
            backend: super::super::Backend::Candle,
            model: Some(dir.path().to_str().unwrap().into()),
            ..Default::default()
        })
        .err()
        .unwrap();
        assert!(
            err.to_string().contains("t5") || err.to_string().contains("safetensors"),
            "{err}"
        );
    }

    /// Downloads nomic-embed-text-v1.5 (~550 MB) once: `DENDRITE_RAG_LIVE_CANDLE=1 cargo test
    /// --features candle -- --ignored`. With Ollama running, also checks that both backends
    /// produce the same vectors.
    #[test]
    #[ignore]
    fn live_candle_nomic_matches_ollama() {
        if std::env::var("DENDRITE_RAG_LIVE_CANDLE").is_err() {
            return;
        }
        let e = CandleEmbedder::load(&EmbedConfig {
            backend: super::super::Backend::Candle,
            model: Some("nomic-ai/nomic-embed-text-v1.5".into()),
            ..Default::default()
        })
        .unwrap();
        let texts = vec![
            "func LocateBinary(name string) (string, error)".to_string(),
            "The sidecar locator searches PATH and sibling checkouts.".to_string(),
        ];
        let docs = e.embed_documents(&texts).unwrap();
        assert_eq!(docs[0].len(), 768);
        let ollama = super::super::ollama::OllamaEmbedder::new(&EmbedConfig {
            backend: super::super::Backend::Ollama,
            ..Default::default()
        });
        if let Ok(reference) = ollama.embed_documents(&texts) {
            for (a, b) in docs.iter().zip(&reference) {
                let cos: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
                assert!(cos > 0.99, "candle vs ollama cosine {cos}");
            }
        }
    }
}
