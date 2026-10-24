//! Qwen3-Embedding (`Qwen/Qwen3-Embedding-0.6B` and its siblings) on candle-transformers'
//! `qwen3::Model`: a causal decoder whose final-normed hidden state at the last token is
//! the embedding (`Pooling::Last`; the end-of-text token is always last).
//!
//! - **One text per pass.** Without padding the causal model needs no attention mask, and
//!   `offset` is always 0.
//! - **KV cache.** `qwen3::Model::clear_kv_cache` is private; only `ModelForCausalLM`
//!   exposes one, and its `forward` runs the LM head over the last token, which an
//!   embedder does not want. `Model` is `Clone` and tensors are reference counted, so a
//!   clone shares the weights and costs one small allocation per layer. The stored model
//!   is therefore never run: it keeps empty caches, and every text runs on a fresh clone
//!   of it, so no state accumulates between texts. The mutex only guards the template.
//! - **Weights.** Computed in f32 on the CPU and f16 on Metal/CUDA ([`dtype`]).

use std::sync::{Mutex, PoisonError};

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::qwen3::{Config, Model};

use super::candle::{MAX_TOKENS, model_err, pool};
use super::tokenize::Tokenizer;
use super::{Pooling, Result, normalize};

/// The token Qwen3-Embedding expects last in every input.
const END_OF_TEXT: &str = "<|endoftext|>";

/// Weight dtype: f16 on a GPU (qwen3 supports f16/bf16 there, and only rejects f64),
/// f32 on the CPU.
pub(super) fn dtype(device: &Device) -> DType {
    if device.is_cpu() { DType::F32 } else { DType::F16 }
}

pub(super) struct Qwen3Encoder {
    /// Never run, only cloned: see the module docs.
    template: Mutex<Model>,
    end_of_text: u32,
}

/// `model.layers.0…` → `layers.0…`.
fn strip_model_prefix(name: &str) -> String {
    name.strip_prefix("model.").unwrap_or(name).to_string()
}

impl Qwen3Encoder {
    pub fn load(vb: VarBuilder, config_text: &str, tokenizer: &Tokenizer) -> Result<Self> {
        let cfg: Config =
            serde_json::from_str(config_text).map_err(|e| model_err(format!("Qwen3 config: {e}")))?;
        let end_of_text = tokenizer
            .token_id(END_OF_TEXT)?
            .ok_or_else(|| model_err(format!("Qwen3 tokenizer has no {END_OF_TEXT} token")))?;
        // `qwen3::Model::new` reads `model.embed_tokens`, `model.layers.N` and `model.norm`.
        // `ModelForCausalLM` checkpoints name them that way, but the embedding checkpoint
        // (`Qwen3Model`, no LM head) stores `embed_tokens.weight`, `layers.N…` and
        // `norm.weight`. A rename layer on the `VarBuilder` drops the `model.` the model asks
        // for when the checkpoint lacks it, so both layouts load without touching the file.
        let vb = if vb.contains_tensor("model.embed_tokens.weight") {
            vb
        } else {
            vb.rename_f(strip_model_prefix)
        };
        let model = Model::new(&cfg, vb).map_err(model_err)?;
        Ok(Self {
            template: Mutex::new(model),
            end_of_text,
        })
    }

    /// Token ids of one text: truncated to [`MAX_TOKENS`] and ending in `<|endoftext|>`,
    /// whether or not the tokenizer's post-processor appends it.
    fn token_ids(&self, tokenizer: &Tokenizer, text: &str) -> Result<Vec<u32>> {
        let mut ids = tokenizer.encode(vec![text.to_string()])?.ids.remove(0);
        if ids.last() != Some(&self.end_of_text) {
            ids.truncate(MAX_TOKENS - 1);
            ids.push(self.end_of_text);
        }
        Ok(ids)
    }

    /// The pooled, unnormalized embedding of one text.
    fn forward(&self, ids: &[u32], device: &Device, pooling: Pooling) -> candle_core::Result<Vec<f32>> {
        // The template is only ever cloned, so a poisoned lock holds no broken state.
        let mut model = self
            .template
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let input = Tensor::new(ids, device)?.unsqueeze(0)?;
        let hidden = model.forward(&input, 0)?.to_dtype(DType::F32)?;
        let mask = Tensor::ones((1, ids.len()), DType::U32, device)?;
        pool(&hidden, &mask, pooling)?
            .to_vec2::<f32>()?
            .pop()
            .ok_or_else(|| candle_core::Error::msg("empty pooling"))
    }

    /// One normalized vector per text, each from its own forward pass.
    pub fn embed(
        &self,
        tokenizer: &Tokenizer,
        device: &Device,
        pooling: Pooling,
        texts: &[String],
    ) -> Result<Vec<Vec<f32>>> {
        texts
            .iter()
            .map(|text| {
                let ids = self.token_ids(tokenizer, text)?;
                let vector = self.forward(&ids, device, pooling).map_err(model_err)?;
                Ok(normalize(vector))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_names_lose_the_model_prefix() {
        assert_eq!(
            strip_model_prefix("model.layers.0.mlp.up_proj.weight"),
            "layers.0.mlp.up_proj.weight"
        );
        assert_eq!(strip_model_prefix("model.norm.weight"), "norm.weight");
        assert_eq!(strip_model_prefix("lm_head.weight"), "lm_head.weight");
    }

    #[test]
    fn rename_layer_serves_unprefixed_checkpoints() {
        let tensors = [(
            "embed_tokens.weight".to_string(),
            Tensor::zeros((2, 3), DType::F32, &Device::Cpu).unwrap(),
        )]
        .into_iter()
        .collect();
        let vb = VarBuilder::from_tensors(tensors, DType::F32, &Device::Cpu);
        assert!(!vb.contains_tensor("model.embed_tokens.weight"));
        let vb = vb.rename_f(strip_model_prefix);
        assert!(vb.contains_tensor("model.embed_tokens.weight"));
        assert!(vb.pp("model.embed_tokens").get((2, 3), "weight").is_ok());
    }

    #[test]
    fn weights_are_f32_on_the_cpu() {
        assert_eq!(dtype(&Device::Cpu), DType::F32);
    }

    #[test]
    fn qwen3_embedding_uses_last_token_pooling_and_a_query_instruction() {
        let model = "Qwen/Qwen3-Embedding-0.6B";
        assert_eq!(super::super::model_pooling(model), Pooling::Last);
        let (doc, query) = super::super::model_prefixes(model);
        assert_eq!(doc, "");
        assert!(query.starts_with("Instruct: ") && query.ends_with("Query: "));
    }

    /// Downloads `Qwen/Qwen3-Embedding-0.6B` (1.2 GB): `LOOM_LIVE_CANDLE=1 cargo test
    /// --features candle-metal live_candle_qwen3 -- --ignored`.
    #[test]
    #[ignore]
    fn live_candle_qwen3_embedding_ranks_the_related_document_first() {
        use crate::embed::{Backend, EmbedConfig, Embedder};
        if std::env::var("LOOM_LIVE_CANDLE").is_err() {
            return;
        }
        let e = super::super::candle::CandleEmbedder::load(&EmbedConfig {
            backend: Backend::Candle,
            model: Some("Qwen/Qwen3-Embedding-0.6B".into()),
            ..Default::default()
        })
        .unwrap();
        let docs = e
            .embed_documents(&[
                "func LocateBinary(name string) (string, error) // finds a sidecar executable on PATH"
                    .to_string(),
                "The ledger stores chunk records and the embedding cache in redb.".to_string(),
            ])
            .unwrap();
        let q = e
            .embed_query("where do we look up the sidecar program on the PATH")
            .unwrap();
        assert_eq!(q.len(), 1024);
        let cos = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        assert!(
            cos(&q, &docs[0]) > cos(&q, &docs[1]),
            "{} vs {}",
            cos(&q, &docs[0]),
            cos(&q, &docs[1])
        );
    }
}
