use std::cell::RefCell;
use std::collections::HashMap;

use super::*;
use crate::engine::Config;
use crate::testutil::project;

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
        ..AskRequest::default()
    }
}

const LOCAL: &str = "LocateBinary is defined in pkg/sidecar/sidecar.go.";
const PAID: &str = "LocateBinary lives in pkg/sidecar/sidecar.go and finds the sidecar.";

#[test]
fn local_answers_with_numbered_sources() {
    let (_d, e) = engine();
    let g = ScriptedGateway::new(&[("local", Ok(LOCAL))]);
    let a = e.ask(&request(AskMode::Local, false), &g).unwrap();
    assert_eq!((a.source_tier.as_str(), a.answer.as_str()), ("local-rag", LOCAL));
    assert!(!a.escalated && !a.hits.is_empty());
    let (_, system, prompt) = g.calls.borrow()[0].clone();
    assert_eq!(system, prompt::SYSTEM_PROMPT);
    assert!(
        prompt.contains("[1] Source: ") && prompt.ends_with("Answer:"),
        "{prompt}"
    );
}

#[test]
fn a_failed_local_model_asks_before_paying() {
    let (_d, e) = engine();
    let g = ScriptedGateway::new(&[("local", Err("down")), ("paid", Ok(PAID))]);
    let a = e.ask(&request(AskMode::Local, false), &g).unwrap();
    assert!(a.escalation_needed && a.answer.is_empty());
    assert_eq!(g.models(), ["local"], "nothing paid without consent");

    let a = e.ask(&request(AskMode::Local, true), &g).unwrap();
    assert_eq!((a.source_tier.as_str(), a.answer.as_str()), ("paid-cloud", PAID));
    assert_eq!(a.local_error.as_deref(), Some("down"));
}

#[test]
fn canned_local_replies_count_as_failures() {
    let (_d, e) = engine();
    let canned = "I processed your request safely with zero cloud tokens consumed.";
    let g = ScriptedGateway::new(&[("local", Ok(canned))]);
    assert!(
        e.ask(&request(AskMode::Local, false), &g)
            .unwrap()
            .escalation_needed
    );
}

#[test]
fn hybrid_refines_the_local_draft() {
    let (_d, e) = engine();
    let g = ScriptedGateway::new(&[("local", Ok(LOCAL)), ("paid", Ok(PAID))]);
    let a = e.ask(&request(AskMode::Hybrid, false), &g).unwrap();
    assert_eq!(a.source_tier, "hybrid");
    assert_eq!(a.local_draft.as_deref(), Some(LOCAL));
    assert_eq!(a.model, "local -> paid");
    assert_eq!(a.prompt_tokens, 20);
    let refine = g.calls.borrow()[1].2.clone();
    assert!(refine.contains("Initial Local Draft:\n") && refine.ends_with("Refined Answer:"));

    // A failed refinement still returns the draft.
    let g = ScriptedGateway::new(&[("local", Ok(LOCAL)), ("paid", Err("quota"))]);
    let a = e.ask(&request(AskMode::Hybrid, false), &g).unwrap();
    assert_eq!((a.source_tier.as_str(), a.answer.as_str()), ("local-rag", LOCAL));
}

#[test]
fn paid_mode_skips_the_local_model() {
    let (_d, e) = engine();
    let g = ScriptedGateway::new(&[("paid", Ok(PAID))]);
    let a = e.ask(&request(AskMode::Paid, false), &g).unwrap();
    assert_eq!(a.source_tier, "paid-cloud");
    assert_eq!(g.models(), ["paid"]);
    assert_eq!(g.calls.borrow()[0].1, prompt::PAID_SYSTEM_PROMPT);
    assert!(
        e.ask(&request(AskMode::Paid, false), &ScriptedGateway::new(&[]))
            .is_err()
    );
}

#[test]
fn command_questions_get_the_documented_commands() {
    let (_d, e) = engine();
    let g = ScriptedGateway::new(&[("local", Ok("Check the sidecar status from the CLI."))]);
    let req = AskRequest {
        question: "how to check the sidecar status".into(),
        ..request(AskMode::Local, false)
    };
    let a = e.ask(&req, &g).unwrap();
    assert!(a.answer.contains("$ dendrite rag status"), "{}", a.answer);
}
