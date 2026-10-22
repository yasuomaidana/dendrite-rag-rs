//! JSON over HTTP, shared by the server backends.

use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{EmbedError, Result};

pub(crate) struct Client {
    agent: ureq::Agent,
    key: Option<String>,
}

impl Client {
    pub fn new(key: Option<String>) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(120)))
            .http_status_as_error(false)
            .build()
            .into();
        Self { agent, key }
    }

    /// POSTs `body` and decodes the JSON answer; non-2xx statuses become errors that
    /// carry the start of the response body.
    pub fn post<B: Serialize, R: DeserializeOwned>(&self, url: &str, body: &B) -> Result<R> {
        let mut req = self.agent.post(url);
        if let Some(k) = &self.key {
            req = req.header("Authorization", &format!("Bearer {k}"));
        }
        let mut resp = req
            .send_json(body)
            .map_err(|e| EmbedError::Http(format!("{url}: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(EmbedError::Http(format!(
                "{url}: {status}: {}",
                body.chars().take(300).collect::<String>()
            )));
        }
        resp.body_mut()
            .read_json()
            .map_err(|e| EmbedError::Response(format!("{url}: {e}")))
    }
}

/// Checks that a server answered one vector per input.
pub(crate) fn expect_len(got: usize, want: usize) -> Result<()> {
    if got == want {
        Ok(())
    } else {
        Err(EmbedError::Response(format!(
            "asked for {want} vectors, got {got}"
        )))
    }
}
