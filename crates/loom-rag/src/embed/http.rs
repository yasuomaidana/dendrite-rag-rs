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

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use rstest::rstest;

    use super::*;

    /// Serves one canned raw HTTP response per connection and returns the base URL; each
    /// request's text is sent back on the channel.
    fn serve(responses: Vec<String>) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for resp in responses {
                let Ok((mut s, _)) = listener.accept() else { return };
                let mut buf = [0u8; 8192];
                let n = s.read(&mut buf).unwrap_or(0);
                let _ = tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
                let _ = s.write_all(resp.as_bytes());
            }
        });
        (url, rx)
    }

    fn reply(status: &str, extra: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\n{extra}Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn post_decodes_json_and_sends_the_bearer_key() {
        let (url, rx) = serve(vec![reply("200 OK", "", r#"{"n":3}"#)]);
        let got: serde_json::Value = Client::new(Some("sekret".into()))
            .post(&url, &serde_json::json!({"q": 1}))
            .unwrap();
        assert_eq!(got["n"], 3);
        let req = rx.recv().unwrap().to_ascii_lowercase();
        assert!(req.contains("authorization: bearer sekret"), "{req}");
    }

    #[test]
    fn post_retries_a_throttled_answer_then_succeeds() {
        let (url, _rx) = serve(vec![
            reply("429 Too Many Requests", "Retry-After: 0\r\n", "{}"),
            reply("200 OK", "", r#"{"ok":true}"#),
        ]);
        let got: serde_json::Value = Client::new(None).post(&url, &serde_json::json!({})).unwrap();
        assert_eq!(got["ok"], true);
    }

    #[test]
    fn post_reports_a_client_error_with_the_body_start() {
        let (url, _rx) = serve(vec![reply("400 Bad Request", "", "no such model")]);
        let err = Client::new(None)
            .post::<_, serde_json::Value>(&url, &serde_json::json!({}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("400") && err.contains("no such model"), "{err}");
    }

    #[test]
    fn post_reports_an_undecodable_success_body() {
        let (url, _rx) = serve(vec![reply("200 OK", "", "not json")]);
        let err = Client::new(None)
            .post::<_, serde_json::Value>(&url, &serde_json::json!({}))
            .unwrap_err();
        assert!(matches!(err, EmbedError::Response(_)), "{err}");
    }

    #[test]
    fn post_reports_a_refused_connection() {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let err = Client::new(None)
            .post::<_, serde_json::Value>(&format!("http://127.0.0.1:{port}"), &serde_json::json!({}))
            .unwrap_err();
        assert!(matches!(err, EmbedError::Http(_)), "{err}");
    }

    #[rstest]
    #[case(0, 400, 600)]
    #[case(2, 1600, 2400)]
    #[case(9, 12_800, 19_200)]
    fn backoff_grows_with_jitter_and_caps(#[case] attempt: u32, #[case] lo: u64, #[case] hi: u64) {
        let ms = backoff_delay(attempt).as_millis() as u64;
        assert!((lo..=hi).contains(&ms), "{ms}");
    }

    #[rstest]
    #[case(3, 3, true)]
    #[case(2, 3, false)]
    fn expect_len_checks_the_count(#[case] got: usize, #[case] want: usize, #[case] ok: bool) {
        assert_eq!(expect_len(got, want).is_ok(), ok);
    }
}
