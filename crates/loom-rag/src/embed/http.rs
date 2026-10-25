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

    /// POSTs `body` and decodes the JSON answer; HTTP 429 and 5xx back off exponentially
    /// with jitter, honouring `Retry-After`. Non-2xx statuses become errors that carry
    /// the start of the response body.
    pub fn post<B: Serialize, R: DeserializeOwned>(&self, url: &str, body: &B) -> Result<R> {
        let max_retries = 4;
        let mut attempt = 0;
        loop {
            let mut req = self.agent.post(url);
            if let Some(k) = &self.key {
                req = req.header("Authorization", &format!("Bearer {k}"));
            }
            let mut resp = req
                .send_json(body)
                .map_err(|e| EmbedError::Http(format!("{url}: {e}")))?;
            let status = resp.status();
            if status.is_success() {
                return resp
                    .body_mut()
                    .read_json()
                    .map_err(|e| EmbedError::Response(format!("{url}: {e}")));
            }
            let code = status.as_u16();
            let retryable = code == 429 || status.is_server_error();
            if retryable && attempt < max_retries {
                // A server's `Retry-After` is honoured up to a minute, so one misbehaving
                // gateway cannot stall indexing for hours; HTTP-date values fall back to
                // the backoff.
                let delay = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|h| h.trim().parse::<u64>().ok())
                    .map_or_else(|| backoff_delay(attempt), Duration::from_secs)
                    .min(MAX_RETRY_AFTER);
                attempt += 1;
                std::thread::sleep(delay);
                continue;
            }
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(EmbedError::Http(format!(
                "{url}: {status}: {}",
                body.chars().take(300).collect::<String>()
            )));
        }
    }
}

/// The longest a single retry waits, whatever the server asks.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

fn backoff_delay(attempt: u32) -> Duration {
    let base_ms = 500u64.saturating_mul(1 << attempt.min(5));
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let jitter_pct = 80 + (nanos % 41);
    let ms = (base_ms * jitter_pct as u64) / 100;
    Duration::from_millis(ms)
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
