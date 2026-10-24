//! `tokenizer.json` loading and batch encoding for the candle encoders and rerankers:
//! truncated to a token limit and padded on the right to the longest input of a batch.

use std::path::Path;

use tokenizers::pipeline::{EncodeOptions, Inputs, Override, PipelineTokenizer};
use tokenizers::{PaddingParams, PaddingStrategy, TruncationParams};

use super::Result;
use super::candle::model_err;

pub(crate) struct Tokenizer {
    inner: PipelineTokenizer,
    options: EncodeOptions,
}

/// Token ids and attention masks of a padded batch, one row per input.
pub(crate) struct Batch {
    pub ids: Vec<Vec<u32>>,
    pub mask: Vec<Vec<u32>>,
    /// Segment ids: 0 for the first sequence of a pair, 1 for the second.
    pub types: Vec<Vec<u32>>,
}

impl Tokenizer {
    /// Reads `tokenizer.json`, upgrading configs written by older tokenizers versions.
    pub fn load(path: &Path, max_tokens: usize) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).map_err(|e| model_err(format!("{}: {e}", path.display())))?;
        let canonical = tokenizers::canonicalize_str(&text).map_err(model_err)?;
        let inner = tokenizers::from_json(&canonical).map_err(model_err)?;
        let options = EncodeOptions {
            padding: Override::With(PaddingParams {
                strategy: PaddingStrategy::BatchLongest,
                ..Default::default()
            }),
            truncation: Override::With(TruncationParams {
                max_length: max_tokens,
                ..Default::default()
            }),
            ..Default::default()
        };
        Ok(Self { inner, options })
    }

    /// Encodes texts (or `(query, document)` pairs) with special tokens.
    pub fn encode(&self, inputs: impl Into<Inputs>) -> Result<Batch> {
        let encodings = self
            .inner
            .encode(inputs, &self.options)
            .wait()
            .map_err(model_err)?;
        let mut batch = Batch {
            ids: Vec::with_capacity(encodings.len()),
            mask: Vec::with_capacity(encodings.len()),
            types: Vec::with_capacity(encodings.len()),
        };
        for e in &encodings {
            batch.ids.push(e.ids().iter().map(|t| t.id()).collect());
            batch.mask.push(match e.attention_mask() {
                Some(m) => m.iter().map(|&b| u32::from(b)).collect(),
                None => vec![1; e.len()],
            });
            batch.types.push(match e.type_ids() {
                Some(t) => t.iter().map(|&b| u32::from(b)).collect(),
                None => vec![0; e.len()],
            });
        }
        Ok(batch)
    }

    /// Id of a special token written literally (`"<|endoftext|>"`), or `None` when the
    /// vocabulary does not know it. A post-processor may add the same token around the
    /// text, so the encoding only has to consist of that one id.
    pub fn token_id(&self, token: &str) -> Result<Option<u32>> {
        let ids = self.encode(vec![token.to_string()])?.ids.remove(0);
        Ok(match ids.first() {
            Some(&id) if ids.iter().all(|&i| i == id) => Some(id),
            _ => None,
        })
    }
}
