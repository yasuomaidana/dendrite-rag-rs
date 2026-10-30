use std::cell::RefCell;
use std::collections::HashMap;

use super::*;
use crate::settings::AskSettings;
use loom_rag::engine::Config;
use loom_rag::testutil::project;
use rstest::{fixture, rstest};

/// Answers by model name and records every call as `(model, system, prompt)`.
struct ScriptedGateway {
    answers: HashMap<&'static str, std::result::Result<&'static str, &'static str>>,
    calls: RefCell<Vec<(String, String, String)>>,
}

impl ScriptedGateway {
    fn new(answers: &[(&'static str, std::result::Result<&'static str, &'static str>)]) -> Self {
        Self {
            answers: answers.iter().cloned().collect(),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn models(&self) -> Vec<String> {
        self.calls.borrow().iter().map(|c| c.0.clone()).collect()
    }
}

impl Gateway for ScriptedGateway {
    fn complete(
        &self,
        model: &str,
        system: &str,
        prompt: &str,
        _t: f32,
    ) -> std::result::Result<Completion, String> {
        self.calls
            .borrow_mut()
            .push((model.to_string(), system.to_string(), prompt.to_string()));
        match self.answers.get(model) {
            Some(Ok(text)) => Ok(Completion {
                text: text.to_string(),
                prompt_tokens: 10,
                output_tokens: 5,
            }),
            Some(Err(e)) => Err(e.to_string()),
            None => Err(format!("unknown model {model}")),
        }
    }
}

#[fixture]
fn engine() -> (tempfile::TempDir, Engine) {
    let dir = project();
    std::fs::write(
        dir.path().join("README.md"),
        "# Sidecar\n\nLocate the binary with:\n\n```sh\ndendrite rag status\n```\n",
    )
    .unwrap();
    let mut e = Engine::open_with_embedder(dir.path(), Config::default(), None).unwrap();
    e.index(false).unwrap();
    (dir, e)
}

fn request(mode: AskMode, allow_paid: bool) -> AskRequest {
    AskRequest {
        question: "where is LocateBinary defined".into(),
        mode,
        local_model: Some("local".into()),
        paid_model: Some("paid".into()),
        allow_paid,
        // These tests script the gateway per call; the answer cache has its own test.
        fresh: true,
        ..AskRequest::default()
    }
}

const LOCAL: &str = "LocateBinary is defined in pkg/sidecar/sidecar.go.";
const PAID: &str = "LocateBinary lives in pkg/sidecar/sidecar.go and finds the sidecar.";

#[rstest]
fn local_answers_with_numbered_sources(engine: (tempfile::TempDir, Engine)) {
    let (_d, e) = engine;
    let g = ScriptedGateway::new(&[("local", Ok(LOCAL))]);
    let a = ask(&e, &AskSettings::default(), &request(AskMode::Local, false), &g).unwrap();
    assert_eq!((a.source_tier.as_str(), a.answer.as_str()), ("local-rag", LOCAL));
    assert!(!a.escalated && !a.hits.is_empty());
    let (_, system, prompt) = g.calls.borrow()[0].clone();
    assert_eq!(system, prompt::SYSTEM_PROMPT);
    assert!(
        prompt.contains("[1] Source: ") && prompt.ends_with("Answer:"),
        "{prompt}"
    );
}

#[rstest]
fn a_failed_local_model_asks_before_paying(engine: (tempfile::TempDir, Engine)) {
    let (_d, e) = engine;
    let g = ScriptedGateway::new(&[("local", Err("down")), ("paid", Ok(PAID))]);
    let a = ask(&e, &AskSettings::default(), &request(AskMode::Local, false), &g).unwrap();
    assert!(a.escalation_needed && a.answer.is_empty());
    assert_eq!(g.models(), ["local"], "nothing paid without consent");

    let a = ask(&e, &AskSettings::default(), &request(AskMode::Local, true), &g).unwrap();
    assert_eq!((a.source_tier.as_str(), a.answer.as_str()), ("paid-cloud", PAID));
    assert_eq!(a.local_error.as_deref(), Some("down"));
}

#[rstest]
fn canned_local_replies_count_as_failures(engine: (tempfile::TempDir, Engine)) {
    let (_d, e) = engine;
    let canned = "I processed your request safely with zero cloud tokens consumed.";
    let g = ScriptedGateway::new(&[("local", Ok(canned))]);
    assert!(
        ask(&e, &AskSettings::default(), &request(AskMode::Local, false), &g)
            .unwrap()
            .escalation_needed
    );
}

#[rstest]
fn hybrid_refines_the_local_draft(engine: (tempfile::TempDir, Engine)) {
    let (_d, e) = engine;
    let g = ScriptedGateway::new(&[("local", Ok(LOCAL)), ("paid", Ok(PAID))]);
    let a = ask(&e, &AskSettings::default(), &request(AskMode::Hybrid, false), &g).unwrap();
    assert_eq!(a.source_tier, "hybrid");
    assert_eq!(a.local_draft.as_deref(), Some(LOCAL));
    assert_eq!(a.model, "local -> paid");
    assert_eq!(a.prompt_tokens, 20);
    let refine = g.calls.borrow()[1].2.clone();
    assert!(refine.contains("Initial Local Draft:\n") && refine.ends_with("Refined Answer:"));

    // A failed refinement still returns the draft.
    let g = ScriptedGateway::new(&[("local", Ok(LOCAL)), ("paid", Err("quota"))]);
    let a = ask(&e, &AskSettings::default(), &request(AskMode::Hybrid, false), &g).unwrap();
    assert_eq!((a.source_tier.as_str(), a.answer.as_str()), ("local-rag", LOCAL));
}

#[rstest]
fn paths_restrict_retrieval(engine: (tempfile::TempDir, Engine)) {
    let (_d, e) = engine;
    let g = ScriptedGateway::new(&[("local", Ok(LOCAL))]);
    let req = AskRequest {
        question: "how do I check the sidecar status".into(),
        paths: vec!["README.md".into()],
        ..request(AskMode::Local, false)
    };
    let a = ask(&e, &AskSettings::default(), &req, &g).unwrap();
    assert!(
        !a.hits.is_empty() && a.hits.iter().all(|h| h.path == "README.md"),
        "{:?}",
        a.hits
    );
}

#[rstest]
fn paid_mode_skips_the_local_model(engine: (tempfile::TempDir, Engine)) {
    let (_d, e) = engine;
    let g = ScriptedGateway::new(&[("paid", Ok(PAID))]);
    let a = ask(&e, &AskSettings::default(), &request(AskMode::Paid, false), &g).unwrap();
    assert_eq!(a.source_tier, "paid-cloud");
    assert_eq!(g.models(), ["paid"]);
    assert_eq!(g.calls.borrow()[0].1, prompt::PAID_SYSTEM_PROMPT);
    assert!(
        ask(
            &e,
            &AskSettings::default(),
            &request(AskMode::Paid, false),
            &ScriptedGateway::new(&[])
        )
        .is_err()
    );
}

#[rstest]
fn command_questions_get_the_documented_commands(engine: (tempfile::TempDir, Engine)) {
    let (_d, e) = engine;
    let g = ScriptedGateway::new(&[("local", Ok("Check the sidecar status from the CLI."))]);
    let req = AskRequest {
        question: "how to check the sidecar status".into(),
        ..request(AskMode::Local, false)
    };
    let a = ask(&e, &AskSettings::default(), &req, &g).unwrap();
    assert!(a.answer.contains("$ dendrite rag status"), "{}", a.answer);
}

#[rstest]
fn code_corpus_keeps_documentation_out_of_the_prompt(engine: (tempfile::TempDir, Engine)) {
    let (_dir, e) = engine;
    let gateway = ScriptedGateway::new(&[(
        DEFAULT_LOCAL_MODEL,
        Ok("Call LocateBinary with the sidecar name."),
    )]);
    let request_in = |corpus| AskRequest {
        question: "locate the sidecar binary".into(),
        corpus,
        ..AskRequest::default()
    };
    let code = ask(
        &e,
        &AskSettings::default(),
        &request_in(CorpusFilter::Code),
        &gateway,
    )
    .unwrap();
    assert!(!code.hits.is_empty());
    assert!(code.hits.iter().all(|h| h.path != "README.md"), "{:?}", code.hits);
    let all = ask(
        &e,
        &AskSettings::default(),
        &request_in(CorpusFilter::All),
        &gateway,
    )
    .unwrap();
    assert!(all.hits.iter().any(|h| h.path == "README.md"));
    assert!(all.timings.total_ms >= all.timings.local_ms);
}

#[test]
fn the_same_question_twice_is_served_from_warm_caches() {
    use std::sync::atomic::Ordering;
    let dir = project();
    let embedder = loom_rag::testutil::HashEmbedder::new();
    let calls = embedder.calls.clone();
    let mut e = Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(embedder))).unwrap();
    e.index(false).unwrap();
    let gateway = ScriptedGateway::new(&[(
        DEFAULT_LOCAL_MODEL,
        Ok("Call LocateBinary with the sidecar name."),
    )]);
    let req = AskRequest {
        question: "locate the sidecar binary".into(),
        corpus: CorpusFilter::Code,
        ..AskRequest::default()
    };

    let embeds_before = calls.load(Ordering::SeqCst);
    let first = ask(&e, &AskSettings::default(), &req, &gateway).unwrap();
    let embeds_first = calls.load(Ordering::SeqCst);
    let second = ask(&e, &AskSettings::default(), &req, &gateway).unwrap();

    assert_eq!(
        embeds_first,
        embeds_before + 1,
        "the first ask embeds the question once"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        embeds_first,
        "the second ask reuses the query vector"
    );
    assert_eq!(first.hits, second.hits, "same question, same context");
    assert_eq!(first.answer, second.answer);
    assert!(
        !first.cached && second.cached,
        "the second answer comes from the cache"
    );
    assert_eq!(gateway.models().len(), 1, "the model is asked once");
    let fresh = ask(
        &e,
        &AskSettings::default(),
        &AskRequest {
            fresh: true,
            ..req.clone()
        },
        &gateway,
    )
    .unwrap();
    assert!(!fresh.cached);
    assert_eq!(gateway.models().len(), 2, "fresh asks the model again");
    let (t1, t2) = (
        first.timings.retrieval.total_ms,
        second.timings.retrieval.total_ms,
    );
    eprintln!("retrieval: first {t1} ms, second {t2} ms");
    // Millisecond timers jitter on a busy runner: the warm pass must not be materially slower than
    // the cold one. That the caches are used is asserted above, by the embed and model call counts.
    assert!(
        t2 <= t1 * 2 + 5 && t2 < 100,
        "warm retrieval: first {t1} ms, second {t2} ms"
    );
}
