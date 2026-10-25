//! JSON-RPC 2.0 over stdio, one message per line (`docs/specs/protocol.md`).

use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Standard and engine error codes.
pub mod code {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const ENGINE_ERROR: i64 = -32000;
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("rpc error {code}: {message}")]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(code::INVALID_PARAMS, message)
    }

    /// An engine failure tagged with a machine-readable `data.kind`.
    pub fn engine(kind: &str, message: impl Into<String>) -> Self {
        Self {
            code: code::ENGINE_ERROR,
            message: message.into(),
            data: Some(json!({ "kind": kind })),
        }
    }
}

#[derive(Debug, Deserialize)]
struct Request {
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct Response {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

/// Dispatches one method call. `params` is `Value::Null` when the request had none.
pub trait Handler {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcError>;
}

/// Decodes params into `T`, treating absent params as an empty object.
pub fn params<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    let params = if params.is_null() { json!({}) } else { params };
    serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))
}

/// Handles one line and returns the serialized response, or `None` for a notification.
pub fn handle_line<H: Handler>(handler: &mut H, line: &str) -> Option<String> {
    let req: Request = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            return Some(encode(
                Value::Null,
                Err(RpcError::new(code::PARSE_ERROR, e.to_string())),
            ));
        }
    };
    let id = req.id.clone();
    let result = match (req.jsonrpc.as_deref(), req.method.as_deref()) {
        (Some("2.0"), Some(method)) => handler.call(method, req.params.unwrap_or(Value::Null)),
        _ => Err(RpcError::new(
            code::INVALID_REQUEST,
            "expected jsonrpc \"2.0\" and a method",
        )),
    };
    id.map(|id| encode(id, result))
}

fn encode(id: Value, result: Result<Value, RpcError>) -> String {
    let (result, error) = match result {
        Ok(v) => (Some(v), None),
        Err(e) => (None, Some(e)),
    };
    serde_json::to_string(&Response {
        jsonrpc: "2.0",
        id,
        result,
        error,
    })
    .expect("responses always serialize")
}

/// Serves requests from `input` until EOF, writing one response line per request.
pub fn serve<H: Handler>(
    handler: &mut H,
    input: impl BufRead,
    mut output: impl Write,
) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(resp) = handle_line(handler, &line) {
            writeln!(output, "{resp}")?;
            output.flush()?;
        }
    }
    Ok(())
}

/// The `health` result.
pub fn health() -> Value {
    json!({
        "name": crate::NAME,
        "version": crate::VERSION,
        "protocol": crate::PROTOCOL,
        "features": crate::features(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    struct Echo;

    impl Handler for Echo {
        fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
            match method {
                "health" => Ok(health()),
                "echo" => Ok(params),
                _ => Err(RpcError::new(
                    code::METHOD_NOT_FOUND,
                    format!("unknown method {method}"),
                )),
            }
        }
    }

    fn call(line: &str) -> Value {
        serde_json::from_str(&handle_line(&mut Echo, line).unwrap()).unwrap()
    }

    #[test]
    fn health_reports_name_version_protocol() {
        let r = call(r#"{"jsonrpc":"2.0","id":1,"method":"health"}"#);
        assert_eq!(r["id"], 1);
        assert_eq!(r["result"]["name"], "dendrite-rag");
        assert_eq!(r["result"]["protocol"], crate::PROTOCOL);
        let features: Vec<&str> = r["result"]["features"]
            .as_array()
            .expect("features is a list")
            .iter()
            .map(|f| f.as_str().unwrap())
            .collect();
        assert_eq!(features, crate::features());
        assert_eq!(features.contains(&"sqlite"), cfg!(feature = "sqlite"));
    }

    #[rstest]
    #[case("{not json", code::PARSE_ERROR)]
    #[case(r#"{"id":2,"method":"health"}"#, code::INVALID_REQUEST)]
    #[case(r#"{"jsonrpc":"2.0","id":3,"method":"nope"}"#, code::METHOD_NOT_FOUND)]
    fn errors_use_standard_codes(#[case] req: &str, #[case] expected: i64) {
        assert_eq!(call(req)["error"]["code"], expected);
    }

    #[test]
    fn notifications_get_no_response() {
        assert!(handle_line(&mut Echo, r#"{"jsonrpc":"2.0","method":"health"}"#).is_none());
    }

    #[test]
    fn serve_answers_each_line() {
        let input = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"echo\",\"params\":[1]}\n\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"health\"}\n";
        let mut out = Vec::new();
        serve(&mut Echo, input.as_bytes(), &mut out).unwrap();
        let lines: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["result"], json!([1]));
        assert_eq!(lines[1]["id"], 2);
    }
}
