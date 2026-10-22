//! Client for an OpenAI-compatible `/v1/chat/completions` endpoint (LiteLLM gateway).

use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("llm request failed: {0}")]
    Http(String),
    #[error("llm response invalid: {0}")]
    Response(String),
    #[error("llm configuration error: {0}")]
    Config(String),
}

pub type Result<T> = std::result::Result<T, LlmError>;

/// A concept extracted from a document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Concept {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
}

#[derive(Debug, Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct ChatChoiceMessage {
    content: Option<String>,
}

/// Resolves the chat completions endpoint URL.
pub fn resolve_url(url: Option<&str>) -> String {
    let base = match url {
        Some(u) if !u.trim().is_empty() => u.trim().to_string(),
        _ => std::env::var("LITELLM_PROXY_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "http://localhost:4000".into()),
    };
    let base = base.trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}/chat/completions")
    } else {
        format!("{base}/v1/chat/completions")
    }
}

/// Resolves the bearer API key: explicit key, then `LITELLM_MASTER_KEY`, then `LITELLM_API_KEY`.
pub fn resolve_api_key(key: Option<&str>) -> Option<String> {
    if let Some(k) = key.filter(|k| !k.trim().is_empty()) {
        return Some(k.trim().to_string());
    }
    std::env::var("LITELLM_MASTER_KEY")
        .or_else(|_| std::env::var("LITELLM_API_KEY"))
        .ok()
        .filter(|v| !v.trim().is_empty())
}

/// Small blocking client for LiteLLM gateway's chat completions API.
#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    url: String,
    key: Option<String>,
    model: String,
}

impl Client {
    pub fn new(url: Option<String>, key: Option<String>, model: String) -> Self {
        let url = resolve_url(url.as_deref());
        let key = resolve_api_key(key.as_deref());
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            agent,
            url,
            key,
            model,
        }
    }

    pub fn from_env(model: String) -> Self {
        Self::new(None, None, model)
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Sends a prompt and returns the assistant's text response.
    pub fn complete(&self, prompt: &str) -> Result<String> {
        self.chat(prompt, false)
    }

    /// Sends a prompt requesting JSON output (`response_format: {"type": "json_object"}`).
    pub fn complete_json(&self, prompt: &str) -> Result<String> {
        self.chat(prompt, true)
    }

    fn chat(&self, prompt: &str, json_mode: bool) -> Result<String> {
        let req_body = ChatRequest {
            model: &self.model,
            messages: vec![ChatMessage {
                role: "user",
                content: prompt,
            }],
            temperature: 0.0,
            response_format: if json_mode {
                Some(ResponseFormat { kind: "json_object" })
            } else {
                None
            },
        };

        let mut req = self.agent.post(&self.url);
        if let Some(k) = &self.key {
            req = req.header("Authorization", &format!("Bearer {k}"));
        }
        let mut resp = req
            .send_json(&req_body)
            .map_err(|e| LlmError::Http(format!("{}: {e}", self.url)))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(LlmError::Http(format!(
                "{}: {status}: {}",
                self.url,
                body.chars().take(300).collect::<String>()
            )));
        }
        let body: ChatResponse = resp
            .body_mut()
            .read_json()
            .map_err(|e| LlmError::Response(format!("{}: {e}", self.url)))?;

        let choice = body
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| LlmError::Response(format!("{}: response had no choices", self.url)))?;

        choice
            .message
            .content
            .ok_or_else(|| LlmError::Response(format!("{}: response choice had no content", self.url)))
    }
}

/// Constructs the prompt for a 3-5 sentence community summary description.
pub fn build_community_summary_prompt(deterministic_text: &str, top_symbol_lines: &[String]) -> String {
    let mut prompt = format!(
        "Describe this code community in 3-5 sentences based on its summary and top symbols.\n\nSummary:\n{}",
        deterministic_text.trim()
    );
    if !top_symbol_lines.is_empty() {
        prompt.push_str("\n\nTop symbols:\n");
        for line in top_symbol_lines {
            prompt.push_str(line);
            prompt.push('\n');
        }
    }
    prompt
}

/// Constructs the prompt for extracting up to 8 concepts from a text/Markdown file.
pub fn build_concepts_prompt(content: &str) -> String {
    format!(
        "Extract up to 8 key concepts from the following text as a JSON array of objects with \"name\" and \"description\" fields:\n\n{}",
        content.trim()
    )
}

/// Parses the model's concepts JSON response, handling direct arrays, wrapper objects, or markdown code blocks.
pub fn parse_concepts(text: &str) -> Option<Vec<Concept>> {
    let text = text.trim();
    let text = if let Some(stripped) = text.strip_prefix("```json") {
        stripped.strip_suffix("```").unwrap_or(stripped).trim()
    } else if let Some(stripped) = text.strip_prefix("```") {
        stripped.strip_suffix("```").unwrap_or(stripped).trim()
    } else {
        text
    };

    if let Ok(mut list) = serde_json::from_str::<Vec<Concept>>(text) {
        list.truncate(8);
        return Some(list);
    }
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(text) {
        if let Some(arr) = val.as_array() {
            let mut list: Vec<Concept> = arr
                .iter()
                .filter_map(|item| serde_json::from_value(item.clone()).ok())
                .collect();
            list.truncate(8);
            return Some(list);
        }
        if let Some(obj) = val.as_object() {
            for (_, v) in obj {
                if let Some(arr) = v.as_array() {
                    let mut list: Vec<Concept> = arr
                        .iter()
                        .filter_map(|item| serde_json::from_value(item.clone()).ok())
                        .collect();
                    if !list.is_empty() {
                        list.truncate(8);
                        return Some(list);
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::HttpStub;

    #[test]
    fn resolves_urls_correctly() {
        assert_eq!(
            resolve_url(Some("http://my-host:8080")),
            "http://my-host:8080/v1/chat/completions"
        );
        assert_eq!(
            resolve_url(Some("http://my-host:8080/v1")),
            "http://my-host:8080/v1/chat/completions"
        );
        assert_eq!(
            resolve_url(Some("http://my-host:8080/v1/chat/completions")),
            "http://my-host:8080/v1/chat/completions"
        );
        assert_eq!(
            resolve_url(Some("http://my-host:8080/")),
            "http://my-host:8080/v1/chat/completions"
        );
    }

    #[test]
    fn parses_concepts_from_various_formats() {
        let json_arr = r#"[{"name": "Auth", "description": "Authentication module"}, {"name": "DB", "description": "Database pool"}]"#;
        let parsed = parse_concepts(json_arr).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "Auth");
        assert_eq!(parsed[1].name, "DB");

        let wrapped = r#"{"concepts": [{"name": "Auth", "description": "Authentication module"}]}"#;
        let parsed = parse_concepts(wrapped).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "Auth");

        let fenced = format!("```json\n{json_arr}\n```");
        let parsed = parse_concepts(&fenced).unwrap();
        assert_eq!(parsed.len(), 2);

        assert!(parse_concepts("not json at all").is_none());
    }

    #[test]
    fn parses_concepts_truncates_at_eight() {
        let items: Vec<String> = (0..12)
            .map(|i| format!(r#"{{"name": "C{i}", "description": "D{i}"}}"#))
            .collect();
        let json_arr = format!("[{}]", items.join(","));
        let parsed = parse_concepts(&json_arr).unwrap();
        assert_eq!(parsed.len(), 8);
        assert_eq!(parsed[7].name, "C7");
    }

    #[test]
    fn completes_chat_with_http_stub() {
        let stub = HttpStub::serve(r#"{"choices":[{"message":{"content":"A helpful community summary."}}]}"#);
        let client = Client::new(
            Some(stub.url.clone()),
            Some("secret".into()),
            "gpt-4o-mini".into(),
        );
        let res = client.complete("Hello world").unwrap();
        assert_eq!(res, "A helpful community summary.");

        let req = stub.request();
        assert!(
            req.head
                .to_ascii_lowercase()
                .contains("authorization: bearer secret")
        );
        assert_eq!(req.body["model"], "gpt-4o-mini");
        assert_eq!(req.body["temperature"], 0.0);
        assert_eq!(req.body["messages"][0]["content"], "Hello world");
        assert!(req.body.get("response_format").is_none());
    }

    #[test]
    fn completes_json_sets_response_format() {
        let stub = HttpStub::serve(
            r#"{"choices":[{"message":{"content":"[{\"name\":\"A\",\"description\":\"B\"}]"}}]}"#,
        );
        let client = Client::new(Some(stub.url.clone()), None, "gpt-4o-mini".into());
        let res = client.complete_json("Extract concepts").unwrap();
        assert_eq!(res, r#"[{"name":"A","description":"B"}]"#);

        let req = stub.request();
        assert_eq!(req.body["response_format"]["type"], "json_object");
    }
}
