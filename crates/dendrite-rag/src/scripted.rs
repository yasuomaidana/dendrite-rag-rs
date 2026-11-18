//! A gateway for tests that answers by model name and records what it was asked.

use std::cell::RefCell;
use std::collections::HashMap;

use loom_rag::llm::Completion;

use crate::ask::Gateway;

/// What a scripted model answers: its text, or the error it fails with.
pub(crate) type Script = std::result::Result<&'static str, &'static str>;

/// Answers by model name and records every call as `(model, system, prompt)`, and the token cap of
/// every capped call.
pub(crate) struct ScriptedGateway {
    answers: HashMap<&'static str, Script>,
    pub(crate) calls: RefCell<Vec<(String, String, String)>>,
    pub(crate) caps: RefCell<Vec<u32>>,
}

impl ScriptedGateway {
    pub(crate) fn new(answers: &[(&'static str, Script)]) -> Self {
        Self {
            answers: answers.iter().cloned().collect(),
            calls: RefCell::new(Vec::new()),
            caps: RefCell::new(Vec::new()),
        }
    }

    pub(crate) fn models(&self) -> Vec<String> {
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

    fn complete_capped(
        &self,
        model: &str,
        system: &str,
        prompt: &str,
        t: f32,
        max_tokens: u32,
    ) -> std::result::Result<Completion, String> {
        self.caps.borrow_mut().push(max_tokens);
        self.complete(model, system, prompt, t)
    }
}
