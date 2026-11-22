//! `brief` (`docs/specs/brief.md`): a short, cited reading brief for an agent that is about to
//! explore a project. A retrieval map of the question and the grep matches of the identifiers in it
//! are condensed by a local model through the LiteLLM gateway; when that model fails a fallback
//! model the caller names writes the brief, and when there is none the deterministic map and
//! matches are returned. No model name that costs money is compiled in: the caller chooses it.

mod derive;
mod prompt;
#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::collections::HashMap;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use loom_rag::corpus::CorpusFilter;
use loom_rag::engine::{Engine, Hit, Result};
use loom_rag::retrieve::lookup::GrepMatch;
use loom_rag::retrieve::{Detail, SearchOpts, Timings};

use crate::ask::cache::CachedGateway;
use crate::ask::{DEFAULT_LOCAL_MODEL, Gateway, timed, usable};
use crate::settings::{AskSettings, BriefSettings};

/// Tokens of retrieval map in the prompt, and the most spans kept from it.
pub const MAP_TOKENS: usize = 1500;
pub const MAP_HITS: usize = 12;
/// Grep matches in a brief unless the request or `[brief] max_matches` says otherwise.
pub const MAX_MATCHES: usize = 60;
/// Most matches kept from one file, so one noisy file cannot fill the brief.
const PER_FILE: usize = 5;
/// Longest match text in a brief.
const MATCH_CHARS: usize = 200;
/// The grep is asked for this many times the matches wanted before the per-file cap thins them.
const OVERSAMPLE: usize = 4;
/// Longest brief requested from a model (`[brief] max_tokens`, `DENDRITE_RAG_BRIEF_MAX_TOKENS`).
pub const DEFAULT_MAX_TOKENS: u32 = 400;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BriefRequest {
    pub question: String,
    /// Regular expressions to grep for; derived from the question when empty.
    pub patterns: Vec<String>,
    /// Restrict the map and the grep to these paths (globs).
    pub paths: Vec<String>,
    pub corpus: CorpusFilter,
    /// Most grep matches (default `[brief] max_matches`, else [`MAX_MATCHES`]).
    pub max_matches: Option<usize>,
    pub local_model: Option<String>,
    /// The model that writes the brief when the local one fails. None means no model is paid for.
    pub fallback_model: Option<String>,
    /// Use `fallback_model` when the local model fails (default true).
    pub allow_fallback: bool,
    /// Ask the model even when the same prompt was answered before.
    pub fresh: bool,
}

impl Default for BriefRequest {
    fn default() -> Self {
        BriefRequest {
            question: String::new(),
            patterns: Vec::new(),
            paths: Vec::new(),
            corpus: CorpusFilter::default(),
            max_matches: None,
            local_model: None,
            fallback_model: None,
            allow_fallback: true,
            fresh: false,
        }
    }
}

/// Who wrote the brief.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BriefTier {
    Local,
    Fallback,
    /// No model: the ranked spans and the matches, as they are.
    Raw,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BriefTimings {
    pub retrieval: Timings,
    pub grep_ms: u64,
    pub model_ms: u64,
    pub total_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BriefAnswer {
    pub brief: String,
    pub source_tier: BriefTier,
    /// The model that wrote the brief; empty for `raw`.
    pub model: String,
    pub hits: Vec<Hit>,
    pub matches: Vec<GrepMatch>,
    /// The regular expressions the grep used, given or derived.
    pub patterns: Vec<String>,
    pub prompt_tokens: usize,
    pub output_tokens: usize,
    /// Why the local model did not write the brief.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_error: Option<String>,
    /// Things that went wrong without stopping the brief (an invalid pattern, an uncited answer).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub cached: bool,
    pub timings: BriefTimings,
}

/// The models a request leaves out come from `[brief]`, then `[ask]`, then the default.
fn models(req: &BriefRequest, ask: &AskSettings, brief: &BriefSettings) -> (String, Option<String>) {
    let local = req
        .local_model
        .clone()
        .or_else(|| brief.local_model.clone())
        .or_else(|| ask.local_model.clone())
        .unwrap_or_else(|| DEFAULT_LOCAL_MODEL.to_string());
    let fallback = req
        .fallback_model
        .clone()
        .or_else(|| brief.fallback_model.clone())
        .filter(|_| req.allow_fallback);
    (local, fallback)
}

/// The map of the question: spans as outlines, or, restricted to paths, the top hits among them.
fn map(engine: &Engine, ask: &AskSettings, req: &BriefRequest) -> Result<(Vec<Hit>, Timings)> {
    if !req.paths.is_empty() {
        let opts = SearchOpts {
            top_k: MAP_HITS,
            paths: req.paths.clone(),
            corpus: req.corpus,
            ..SearchOpts::default()
        };
        let mut timings = Timings::default();
        let hits = engine.search_timed(&req.question, &opts, &mut timings)?;
        return Ok((hits, timings));
    }
    let share = ask.docs_share(None);
    let pack = engine.context_shared(&req.question, MAP_TOKENS, req.corpus, Detail::Map, share)?;
    let mut hits = pack.spans;
    hits.truncate(MAP_HITS);
    Ok((hits, pack.timings))
}

/// At most `PER_FILE` matches of a file and `max` in all, each cut to `MATCH_CHARS`.
fn thin(matches: Vec<GrepMatch>, max: usize) -> Vec<GrepMatch> {
    let mut per_file: HashMap<String, usize> = HashMap::new();
    let mut out = Vec::new();
    for mut m in matches {
        let n = per_file.entry(m.path.clone()).or_default();
        if *n >= PER_FILE {
            continue;
        }
        *n += 1;
        if let Some((i, _)) = m.text.char_indices().nth(MATCH_CHARS) {
            m.text.truncate(i);
            m.text.push('…');
        }
        out.push(m);
        if out.len() == max {
            break;
        }
    }
    out
}

/// The grep matches of `patterns`; an invalid pattern is a warning, not a failed brief.
fn find(
    engine: &Engine,
    patterns: &[String],
    paths: &[String],
    max: usize,
    warnings: &mut Vec<String>,
) -> Vec<GrepMatch> {
    if patterns.is_empty() {
        return Vec::new();
    }
    match engine.grep(&derive::alternation(patterns), paths, max * OVERSAMPLE) {
        Ok(found) => thin(found, max),
        Err(e) => {
            warnings.push(format!("grep skipped: {e}"));
            Vec::new()
        }
    }
}

/// What a model wrote.
struct Written {
    text: String,
    tier: BriefTier,
    model: String,
    prompt_tokens: usize,
    output_tokens: usize,
}

/// The brief from the local model, else from `fallback`; the error says why the local one failed
/// (and why the fallback did too).
fn write(
    gateway: &dyn Gateway,
    cap: u32,
    (local, fallback): (&str, Option<&str>),
    user: &str,
    model_ms: &Cell<u64>,
) -> (Option<Written>, Option<String>) {
    let ask = |model: &str, tier: BriefTier| {
        usable(timed(model_ms, || {
            gateway.complete_capped(model, prompt::SYSTEM_PROMPT, user, 0.1, cap)
        }))
        .map(|c| Written {
            text: c.text,
            tier,
            model: model.to_string(),
            prompt_tokens: c.prompt_tokens,
            output_tokens: c.output_tokens,
        })
    };
    let local_error = match ask(local, BriefTier::Local) {
        Ok(w) => return (Some(w), None),
        Err(e) => e,
    };
    let Some(fallback) = fallback else {
        return (None, Some(local_error));
    };
    match ask(fallback, BriefTier::Fallback) {
        Ok(w) => (Some(w), Some(local_error)),
        Err(e) => (
            None,
            Some(format!("{local_error}; fallback {fallback} failed: {e}")),
        ),
    }
}

/// Condenses the map and the grep matches for `req.question` into a cited brief.
pub fn brief(
    engine: &Engine,
    ask: &AskSettings,
    settings: &BriefSettings,
    req: &BriefRequest,
    gateway: &dyn Gateway,
) -> Result<BriefAnswer> {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let (hits, retrieval) = map(engine, ask, req)?;
    let patterns = if req.patterns.is_empty() {
        derive::patterns(&req.question)
    } else {
        req.patterns.clone()
    };
    let max = req
        .max_matches
        .filter(|n| *n > 0)
        .or(settings.max_matches)
        .unwrap_or(MAX_MATCHES);
    let grep_started = Instant::now();
    let matches = find(engine, &patterns, &req.paths, max, &mut warnings);
    let grep_ms = grep_started.elapsed().as_millis() as u64;

    let cached = CachedGateway {
        inner: gateway,
        ledger: engine.ledger(),
        fresh: req.fresh,
        hits: Cell::new(0),
    };
    let (local, fallback) = models(req, ask, settings);
    let model_ms = Cell::new(0);
    let user = prompt::user_prompt(&req.question, &hits, &matches);
    let cap = settings.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
    let (written, local_error) = write(&cached, cap, (&local, fallback.as_deref()), &user, &model_ms);

    let known = prompt::known_paths(&hits, &matches);
    let (text, tier, model, prompt_tokens, output_tokens) = match written {
        Some(w) => {
            let kept = prompt::keep_cited(&w.text, &known);
            if kept.is_empty() {
                warnings.push(format!("{} cited only paths the input did not contain", w.model));
                (
                    prompt::raw_brief(&hits, &matches),
                    BriefTier::Raw,
                    String::new(),
                    w.prompt_tokens,
                    w.output_tokens,
                )
            } else {
                (kept, w.tier, w.model, w.prompt_tokens, w.output_tokens)
            }
        }
        None => (
            prompt::raw_brief(&hits, &matches),
            BriefTier::Raw,
            String::new(),
            0,
            0,
        ),
    };
    Ok(BriefAnswer {
        brief: text,
        source_tier: tier,
        model,
        hits,
        matches,
        patterns,
        prompt_tokens,
        output_tokens,
        local_error,
        warnings,
        cached: cached.hits.get() > 0,
        timings: BriefTimings {
            retrieval,
            grep_ms,
            model_ms: model_ms.get(),
            total_ms: started.elapsed().as_millis() as u64,
        },
    })
}
