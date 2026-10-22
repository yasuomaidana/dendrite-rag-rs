use super::*;
use crate::rpc::handle_line;

fn call(s: &mut Server, method: &str, params: Value) -> Value {
    let line = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    serde_json::from_str(&handle_line(s, &line).unwrap()).unwrap()
}

#[test]
fn embedders_and_project_settings() {
    let mut s = Server::per_project();
    let r = call(&mut s, "embedders", json!({}));
    let names: Vec<_> = r["result"]["backends"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["backend"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"ollama".to_string()) && names.contains(&"hf".to_string()));

    let dir = loom_rag::testutil::project();
    std::fs::create_dir_all(dir.path().join(".dendrite")).unwrap();
    std::fs::write(
        dir.path().join(crate::settings::FILE),
        "[embed]\nbackend = \"none\"\n",
    )
    .unwrap();
    let root = dir.path().to_str().unwrap();
    let r = call(&mut s, "settings", json!({"root": root}));
    assert_eq!(r["result"]["file_exists"], true);
    // The environment may override the backend on developer machines.
    if std::env::var("DENDRITE_RAG_EMBED_BACKEND").is_err() {
        assert_eq!(r["result"]["settings"]["embed"]["backend"], "none");
        // The project's own settings open the engine: no embedder, so indexing works offline.
        let r = call(&mut s, "index", json!({"root": root}));
        assert_eq!(r["result"]["embedded"], 0, "{r}");
    }

    let other = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(other.path().join(".dendrite")).unwrap();
    std::fs::write(
        other.path().join(crate::settings::FILE),
        "[embed]\nbackend = \"word2vec\"\n",
    )
    .unwrap();
    let r = call(&mut s, "index", json!({"root": other.path().to_str().unwrap()}));
    assert_eq!(r["error"]["data"]["kind"], "config", "{r}");
}
