//! Retrieval-augmented generation (`docs/specs/ask.md`): the project's context pack (or top
//! hits) answered by a local model through the LiteLLM gateway, refined or answered by a paid
//! model when asked. Paid use is never decided here: when the local model fails and the
//! caller did not allow paid escalation, the answer says escalation is needed and the client
//! asks the person.

mod prompt;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

use crate::engine::{Engine, Hit, Result};
use crate::llm::Completion;
use crate::retrieve::SearchOpts;

/// Tokens of context for a question.
pub const CONTEXT_TOKENS: usize = 2500;
/// The gateway's local model when neither the request nor `[ask]` names one.
pub const DEFAULT_LOCAL_MODEL: &str = "local-coder";
pub const DEFAULT_PAID_MODEL: &str = "gemini-2.5-pro";
/// Hits used when retrieval is restricted to paths and no `top_k` is given.
const PATH_HITS: usize = 4;

/// How to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AskMode {
    /// The local model; on failure, the paid one only when `allow_paid`.
    #[default]
    Local,
    /// A local draft refined by the paid model.
    Hybrid,
    /// The paid model directly.
    Paid,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AskRequest {
    pub question: String,
    pub mode: AskMode,
    /// Answer from the top hits instead of a context pack.
    pub top_k: usize,
    /// Restrict retrieval to these paths (globs): the top hits among them, 4 by default.
    pub paths: Vec<String>,
    pub local_model: Option<String>,
    pub paid_model: Option<String>,
    /// The person already agreed to paid escalation (`dendrite ask -y`).
    pub allow_paid: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AskAnswer {
    pub answer: String,
    /// `local-rag`, `hybrid`, `paid-cloud`, or empty when escalation is needed.
    pub source_tier: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_draft: Option<String>,
    /// The local model failed and paid escalation was not allowed: ask the person, then call
    /// again with `allow_paid` or mode `paid`.
    pub escalation_needed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_error: Option<String>,
    pub escalated: bool,
    pub prompt_tokens: usize,
    pub output_tokens: usize,
    pub hits: Vec<Hit>,
}

/// A chat-completions endpoint (the LiteLLM gateway in production).
pub trait Gateway {
    fn complete(
        &self,
        model: &str,
        system: &str,
        prompt: &str,
        temperature: f32,
    ) -> std::result::Result<Completion, String>;
}

impl Gateway for crate::llm::Client {
    fn complete(
        &self,
        model: &str,
        system: &str,
        prompt: &str,
        temperature: f32,
    ) -> std::result::Result<Completion, String> {
        self.chat_with(model, Some(system), prompt, temperature)
            .map_err(|e| e.to_string())
    }
}

/// A usable local answer: non-empty and not an engine's canned placeholder.
fn usable(c: std::result::Result<Completion, String>) -> std::result::Result<Completion, String> {
    let c = c?;
    if c.text.trim().is_empty() {
        return Err("empty answer".into());
    }
    if c.text.contains(prompt::CANNED_REPLY) {
        return Err("engine returned generic stub response without answering question".into());
    }
    Ok(c)
}

impl Engine {
    /// Answers `req.question` from this project through `gateway`.
    pub fn ask(&self, req: &AskRequest, gateway: &dyn Gateway) -> Result<AskAnswer> {
        let hits = if req.top_k > 0 || !req.paths.is_empty() {
            let opts = SearchOpts {
                top_k: if req.top_k > 0 { req.top_k } else { PATH_HITS },
                paths: req.paths.clone(),
                ..SearchOpts::default()
            };
            self.search(&req.question, &opts)?
        } else {
            self.context(&req.question, CONTEXT_TOKENS)?.spans
        };
        let local_model = req
            .local_model
            .clone()
            .or_else(|| self.cfg.ask.local_model.clone())
            .unwrap_or_else(|| DEFAULT_LOCAL_MODEL.to_string());
        let paid_model = req
            .paid_model
            .clone()
            .or_else(|| self.cfg.ask.paid_model.clone())
            .unwrap_or_else(|| DEFAULT_PAID_MODEL.to_string());
        let user = prompt::rag_prompt(&req.question, &hits);
        let finish = |c: Completion, tier: &str, model: String, escalated: bool| AskAnswer {
            answer: prompt::enhance(&req.question, &c.text, &hits),
            source_tier: tier.to_string(),
            model,
            escalated,
            prompt_tokens: c.prompt_tokens,
            output_tokens: c.output_tokens,
            hits: hits.clone(),
            ..AskAnswer::default()
        };
        let paid = |p: &str| gateway.complete(&paid_model, prompt::PAID_SYSTEM_PROMPT, p, 0.2);

        if req.mode == AskMode::Paid {
            let c = paid(&user).map_err(|e| ask_error(&paid_model, e))?;
            return Ok(finish(c, "paid-cloud", paid_model.clone(), true));
        }
        let local = usable(gateway.complete(&local_model, prompt::SYSTEM_PROMPT, &user, 0.2));
        let local_error = match local {
            Ok(draft) if req.mode == AskMode::Hybrid => {
                let draft_answer = prompt::enhance(&req.question, &draft.text, &hits);
                let refine = prompt::refinement_prompt(&req.question, &hits, &draft_answer);
                return Ok(match paid(&refine) {
                    Ok(c) => AskAnswer {
                        local_draft: Some(draft_answer),
                        prompt_tokens: draft.prompt_tokens + c.prompt_tokens,
                        ..finish(c, "hybrid", format!("{local_model} -> {paid_model}"), true)
                    },
                    // The draft is still an answer when refinement fails.
                    Err(e) => AskAnswer {
                        local_error: Some(format!("paid refinement failed: {e}")),
                        ..finish(draft, "local-rag", local_model.clone(), false)
                    },
                });
            }
            Ok(c) => return Ok(finish(c, "local-rag", local_model.clone(), false)),
            Err(e) => e,
        };
        if !req.allow_paid {
            return Ok(AskAnswer {
                escalation_needed: true,
                local_error: Some(local_error),
                model: local_model,
                hits,
                ..AskAnswer::default()
            });
        }
        let c = paid(&user).map_err(|e| ask_error(&paid_model, e))?;
        Ok(AskAnswer {
            local_error: Some(local_error),
            ..finish(c, "paid-cloud", paid_model.clone(), true)
        })
    }
}

fn ask_error(model: &str, e: String) -> crate::engine::Error {
    crate::engine::Error::Other(anyhow::anyhow!("paid model {model} failed: {e}"))
}
