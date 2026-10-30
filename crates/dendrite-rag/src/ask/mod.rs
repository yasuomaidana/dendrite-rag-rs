//! Retrieval-augmented generation (`docs/specs/ask.md`): the project's context pack (or top
//! hits) answered by a local model through the LiteLLM gateway, refined or answered by a paid
//! model when asked. Paid use is never decided here: when the local model fails and the
//! caller did not allow paid escalation, the answer says escalation is needed and the client
//! asks the person.

mod cache;
mod prompt;
#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use loom_rag::corpus::CorpusFilter;
use loom_rag::engine::{Engine, Hit, Result};
use loom_rag::llm::Completion;
use loom_rag::retrieve::{Detail, SearchOpts, Timings};

use crate::settings::AskSettings;

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
    /// Which corpora the context comes from (default: code and documentation).
    pub corpus: CorpusFilter,
    /// Share of an `all` context pack kept for documentation, 0 to 1 (default: the project's
    /// `[ask] docs_share`, else 0.2).
    pub docs_share: Option<f32>,
    pub local_model: Option<String>,
    pub paid_model: Option<String>,
    /// The person already agreed to paid escalation (`dendrite ask -y`).
    pub allow_paid: bool,
    /// Ask the model even when the same prompt was answered before.
    pub fresh: bool,
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
    pub timings: AskTimings,
    /// The model's answers came from the cache of earlier identical prompts.
    pub cached: bool,
    /// The reranker found nothing in the project likely to answer (`[rerank] abstain`), so
    /// no model was asked and `answer` is empty.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub abstained: bool,
}

/// Where an answer spent its time, in milliseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AskTimings {
    pub retrieval: Timings,
    /// Calls to the local model.
    pub local_ms: u64,
    /// Calls to the paid model.
    pub paid_ms: u64,
    pub total_ms: u64,
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

impl Gateway for loom_rag::llm::Client {
    fn complete(
        &self,
        model: &str,
        system: &str,
        prompt: &str,
        temperature: f32,
    ) -> std::result::Result<Completion, String> {
        self.chat_with(
            model,
            Some(system),
            prompt,
            temperature,
            Some(answer_max_tokens()),
        )
        .map_err(|e| e.to_string())
    }
}

/// Longest answer `ask` requests from a model. Answers grounded in a few retrieved spans fit inside
/// it; generation is nearly all of `ask`'s wall time (a local 8B model writes about 16 tokens a second
/// here), and without a cap the gateway's ceiling of thousands of tokens applies. `DENDRITE_RAG_ASK_MAX_TOKENS` overrides it.
const DEFAULT_ANSWER_MAX_TOKENS: u32 = 600;

fn answer_max_tokens() -> u32 {
    std::env::var("DENDRITE_RAG_ASK_MAX_TOKENS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_ANSWER_MAX_TOKENS)
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

/// Answers `req.question` from the project `engine` opened through `gateway`; `settings`
/// name the models a request leaves out.
pub fn ask(
    engine: &Engine,
    settings: &AskSettings,
    req: &AskRequest,
    gateway: &dyn Gateway,
) -> Result<AskAnswer> {
    let started = Instant::now();
    let mut retrieval = Timings::default();
    let hits = if req.top_k > 0 || !req.paths.is_empty() {
        let opts = SearchOpts {
            top_k: if req.top_k > 0 { req.top_k } else { PATH_HITS },
            paths: req.paths.clone(),
            corpus: req.corpus,
            ..SearchOpts::default()
        };
        engine.search_timed(&req.question, &opts, &mut retrieval)?
    } else {
        let share = settings.docs_share(req.docs_share);
        let pack = engine.context_shared(&req.question, CONTEXT_TOKENS, req.corpus, Detail::Auto, share)?;
        retrieval = pack.timings;
        pack.spans
    };
    if retrieval.abstained() {
        return Ok(AskAnswer {
            abstained: true,
            timings: AskTimings {
                retrieval,
                total_ms: started.elapsed().as_millis() as u64,
                ..AskTimings::default()
            },
            ..AskAnswer::default()
        });
    }
    let local_ms = Cell::new(0);
    let paid_ms = Cell::new(0);
    let cached = cache::CachedGateway {
        inner: gateway,
        ledger: engine.ledger(),
        fresh: req.fresh,
        hits: Cell::new(0),
    };
    let mut answer = answer(settings, req, &cached, hits, &local_ms, &paid_ms)?;
    answer.cached = cached.hits.get() > 0;
    answer.timings = AskTimings {
        retrieval,
        local_ms: local_ms.get(),
        paid_ms: paid_ms.get(),
        total_ms: started.elapsed().as_millis() as u64,
    };
    Ok(answer)
}

/// Answers from retrieved `hits`, adding the time of local and paid calls to the counters.
fn answer(
    settings: &AskSettings,
    req: &AskRequest,
    gateway: &dyn Gateway,
    hits: Vec<Hit>,
    local_ms: &Cell<u64>,
    paid_ms: &Cell<u64>,
) -> Result<AskAnswer> {
    let local_model = req
        .local_model
        .clone()
        .or_else(|| settings.local_model.clone())
        .unwrap_or_else(|| DEFAULT_LOCAL_MODEL.to_string());
    let paid_model = req
        .paid_model
        .clone()
        .or_else(|| settings.paid_model.clone())
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
    let paid = |p: &str| {
        timed(paid_ms, || {
            gateway.complete(&paid_model, prompt::PAID_SYSTEM_PROMPT, p, 0.2)
        })
    };

    if req.mode == AskMode::Paid {
        let c = paid(&user).map_err(|e| ask_error(&paid_model, e))?;
        return Ok(finish(c, "paid-cloud", paid_model.clone(), true));
    }
    let local = usable(timed(local_ms, || {
        gateway.complete(&local_model, prompt::SYSTEM_PROMPT, &user, 0.2)
    }));
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

/// Runs `f`, adding its duration in milliseconds to `acc`.
fn timed<T>(acc: &Cell<u64>, f: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let out = f();
    acc.set(acc.get() + started.elapsed().as_millis() as u64);
    out
}

fn ask_error(model: &str, e: String) -> loom_rag::engine::Error {
    loom_rag::engine::Error::Other(anyhow::anyhow!("paid model {model} failed: {e}"))
}

#[cfg(test)]
mod max_tokens_tests {
    use super::*;

    #[test]
    fn the_answer_cap_has_a_default_and_an_override() {
        // SAFETY: this is the only test in the crate that touches this variable.
        unsafe { std::env::remove_var("DENDRITE_RAG_ASK_MAX_TOKENS") };
        assert_eq!(answer_max_tokens(), DEFAULT_ANSWER_MAX_TOKENS);
        unsafe { std::env::set_var("DENDRITE_RAG_ASK_MAX_TOKENS", "300") };
        assert_eq!(answer_max_tokens(), 300);
        unsafe { std::env::set_var("DENDRITE_RAG_ASK_MAX_TOKENS", "0") };
        assert_eq!(answer_max_tokens(), DEFAULT_ANSWER_MAX_TOKENS, "zero means unset");
        unsafe { std::env::set_var("DENDRITE_RAG_ASK_MAX_TOKENS", "lots") };
        assert_eq!(answer_max_tokens(), DEFAULT_ANSWER_MAX_TOKENS);
        unsafe { std::env::remove_var("DENDRITE_RAG_ASK_MAX_TOKENS") };
    }
}
