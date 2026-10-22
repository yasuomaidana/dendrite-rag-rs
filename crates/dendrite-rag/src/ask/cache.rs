//! Answers cached by model and exact prompt in the project's ledger, so asking the same
//! question about unchanged code returns at once instead of waiting for the model again. A
//! changed file changes the retrieved context, hence the prompt and the key.

use std::cell::Cell;

use loom_rag::llm::Completion;
use loom_rag::store::LedgerStore;
use serde_json::{Value, json};

use super::{Gateway, prompt};

/// A gateway that answers from the cache when it can and caches usable answers.
pub(super) struct CachedGateway<'a> {
    pub inner: &'a dyn Gateway,
    pub ledger: &'a dyn LedgerStore,
    /// Skip the cache lookup (the answer is still stored).
    pub fresh: bool,
    pub hits: Cell<usize>,
}

fn key(system: &str, prompt: &str, temperature: f32) -> String {
    let text = format!("ask\u{0}{temperature}\u{0}{system}\u{0}{prompt}");
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

impl Gateway for CachedGateway<'_> {
    fn complete(
        &self,
        model: &str,
        system: &str,
        prompt: &str,
        temperature: f32,
    ) -> Result<Completion, String> {
        let key = key(system, prompt, temperature);
        if !self.fresh
            && let Ok(Some(cached)) = self.ledger.llm_cache(model, &key)
            && let Ok(v) = serde_json::from_str::<Value>(&cached)
            && let Some(text) = v["text"].as_str()
        {
            self.hits.set(self.hits.get() + 1);
            let tokens = |k: &str| v[k].as_u64().unwrap_or(0) as usize;
            return Ok(Completion {
                text: text.to_string(),
                prompt_tokens: tokens("prompt_tokens"),
                output_tokens: tokens("output_tokens"),
            });
        }
        let c = self.inner.complete(model, system, prompt, temperature)?;
        if !c.text.trim().is_empty() && !c.text.contains(prompt::CANNED_REPLY) {
            let value =
                json!({"text": c.text, "prompt_tokens": c.prompt_tokens, "output_tokens": c.output_tokens});
            // A cache that cannot be written only costs the next ask a model call.
            if let Ok(mut batch) = self.ledger.batch()
                && batch.put_llm_cache(model, &key, &value.to_string()).is_ok()
            {
                let _ = batch.commit();
            }
        }
        Ok(c)
    }
}
