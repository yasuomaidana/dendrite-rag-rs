//! JinaBERT v2 with query/key post-norm (`jinaai/jina-bert-v2-qk-post-norm`), the
//! architecture of `jinaai/jina-embeddings-v2-base-code`. candle-transformers ships only
//! the original JinaBERT v2 layout, so this module implements the variant:
//!
//! - embeddings: word + token type, LayerNorm (ALiBi replaces position embeddings);
//! - self-attention: `LN_q(W_q x)`, `LN_k(W_k x)`, `W_v x`, scores plus the attention mask
//!   plus the ALiBi bias `-slope_h · |i - j|`;
//! - `a = LN_o(W_o · attn + x)`, `r = LN_1(x + a)`, `out = LN_2(r + W_down(up · gelu(gate)))`
//!   where `[up | gate] = W_up_gated r`.
//!
//! Unlike candle's JinaBERT it takes an attention mask, so padded batches are exact.

use candle_core::{D, DType, Device, Module, Result, Tensor};
use candle_nn::{Embedding, LayerNorm, Linear, VarBuilder, embedding, layer_norm, linear, linear_no_bias};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub vocab_size: usize,
    pub hidden_size: usize,
    pub num_hidden_layers: usize,
    pub num_attention_heads: usize,
    pub intermediate_size: usize,
    pub type_vocab_size: usize,
    pub layer_norm_eps: f64,
    #[serde(default)]
    pub feed_forward_type: Option<String>,
}

impl Config {
    /// Whether `config.json` (already known to be JinaBERT) describes this variant: its
    /// weights carry `layer_norm_q` / `layer_norm_k`.
    pub fn is_qk_post_norm(vb: &VarBuilder) -> bool {
        vb.contains_tensor("encoder.layer.0.attention.self.layer_norm_q.weight")
    }
}

struct Layer {
    query: Linear,
    key: Linear,
    value: Linear,
    norm_q: LayerNorm,
    norm_k: LayerNorm,
    out: Linear,
    norm_out: LayerNorm,
    norm_1: LayerNorm,
    norm_2: LayerNorm,
    up_gated: Linear,
    down: Linear,
}

pub struct JinaCodeModel {
    words: Embedding,
    types: Embedding,
    norm: LayerNorm,
    layers: Vec<Layer>,
    heads: usize,
    head_dim: usize,
    intermediate: usize,
    reglu: bool,
    device: Device,
}

/// ALiBi slopes for `n` heads (the power-of-two construction with the interleaved
/// extension for other head counts).
fn slopes(n: usize) -> Vec<f32> {
    fn pow2(n: usize) -> Vec<f32> {
        let start = 2f32.powf(-(2f32.powf(-((n as f32).log2() - 3.0))));
        (0..n).map(|i| start * start.powi(i as i32)).collect()
    }
    if n.is_power_of_two() {
        return pow2(n);
    }
    let closest = 1 << (usize::BITS - 1 - n.leading_zeros());
    let mut s = pow2(closest);
    s.extend(slopes(2 * closest).into_iter().step_by(2).take(n - closest));
    s
}

impl JinaCodeModel {
    pub fn load(vb: VarBuilder, cfg: &Config) -> Result<Self> {
        let h = cfg.hidden_size;
        let eps = cfg.layer_norm_eps;
        let layers = (0..cfg.num_hidden_layers)
            .map(|i| {
                let l = vb.pp(format!("encoder.layer.{i}"));
                let a = l.pp("attention");
                Ok(Layer {
                    query: linear(h, h, a.pp("self.query"))?,
                    key: linear(h, h, a.pp("self.key"))?,
                    value: linear(h, h, a.pp("self.value"))?,
                    norm_q: layer_norm(h, eps, a.pp("self.layer_norm_q"))?,
                    norm_k: layer_norm(h, eps, a.pp("self.layer_norm_k"))?,
                    out: linear(h, h, a.pp("output.dense"))?,
                    norm_out: layer_norm(h, eps, a.pp("output.LayerNorm"))?,
                    norm_1: layer_norm(h, eps, l.pp("layer_norm_1"))?,
                    norm_2: layer_norm(h, eps, l.pp("layer_norm_2"))?,
                    up_gated: linear_no_bias(h, 2 * cfg.intermediate_size, l.pp("mlp.up_gated_layer"))?,
                    down: linear(cfg.intermediate_size, h, l.pp("mlp.down_layer"))?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            words: embedding(cfg.vocab_size, h, vb.pp("embeddings.word_embeddings"))?,
            types: embedding(cfg.type_vocab_size, h, vb.pp("embeddings.token_type_embeddings"))?,
            norm: layer_norm(h, eps, vb.pp("embeddings.LayerNorm"))?,
            layers,
            heads: cfg.num_attention_heads,
            head_dim: h / cfg.num_attention_heads,
            intermediate: cfg.intermediate_size,
            reglu: cfg.feed_forward_type.as_deref() == Some("reglu"),
            device: vb.device().clone(),
        })
    }

    /// `[batch, tokens]` ids and 0/1 mask → `[batch, tokens, hidden]`.
    pub fn forward(&self, ids: &Tensor, mask: &Tensor) -> Result<Tensor> {
        let (b, n) = ids.dims2()?;
        let mut x = self
            .norm
            .forward(&(self.words.forward(ids)? + self.types.forward(&ids.zeros_like()?)?)?)?;

        // Additive bias [b, heads, n, n]: padding mask plus ALiBi.
        let pos: Vec<f32> = (0..n * n)
            .map(|k| ((k % n) as f32 - (k / n) as f32).abs())
            .collect();
        let dist = Tensor::from_vec(pos, (1, 1, n, n), &self.device)?;
        let s = Tensor::from_vec(slopes(self.heads), (1, self.heads, 1, 1), &self.device)?;
        let alibi = dist.broadcast_mul(&s.neg()?)?;
        let pad = ((mask.to_dtype(DType::F32)? - 1.0)? * 1e9f64)?.reshape((b, 1, 1, n))?;
        let bias = alibi.broadcast_add(&pad)?;

        let split = |t: Tensor| -> Result<Tensor> {
            t.reshape((b, n, self.heads, self.head_dim))?
                .transpose(1, 2)?
                .contiguous()
        };
        let scale = (self.head_dim as f64).powf(-0.5);
        for l in &self.layers {
            let q = split(l.norm_q.forward(&l.query.forward(&x)?)?)?;
            let k = split(l.norm_k.forward(&l.key.forward(&x)?)?)?;
            let v = split(l.value.forward(&x)?)?;
            let scores = (q.matmul(&k.t()?)? * scale)?.broadcast_add(&bias)?;
            let probs = candle_nn::ops::softmax_last_dim(&scores)?;
            let ctx = probs
                .matmul(&v)?
                .transpose(1, 2)?
                .reshape((b, n, self.heads * self.head_dim))?;
            let attn = l.norm_out.forward(&(l.out.forward(&ctx)? + &x)?)?;
            let r = l.norm_1.forward(&(&x + attn)?)?;
            let ug = l.up_gated.forward(&r)?;
            let up = ug.narrow(D::Minus1, 0, self.intermediate)?;
            let gate = ug.narrow(D::Minus1, self.intermediate, self.intermediate)?;
            let act = if self.reglu {
                gate.relu()?
            } else {
                gate.gelu_erf()?
            };
            let mlp = l.down.forward(&(up * act)?)?;
            x = l.norm_2.forward(&(r + mlp)?)?;
        }
        Ok(x)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alibi_slopes_match_the_reference() {
        // 8 heads: 2^-1 … 2^-8.
        let s = slopes(8);
        assert!((s[0] - 0.5).abs() < 1e-6 && (s[7] - 1.0 / 256.0).abs() < 1e-9);
        // 12 heads: the 8-head slopes, then every other 16-head slope.
        let s12 = slopes(12);
        let s16 = slopes(16);
        assert_eq!(s12.len(), 12);
        assert_eq!(&s12[..8], &s[..]);
        assert_eq!(s12[8], s16[0]);
        assert_eq!(s12[9], s16[2]);
    }
}
