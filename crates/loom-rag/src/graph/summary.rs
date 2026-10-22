//! One summary chunk per community (`docs/specs/graph.md` § Summary chunks): a
//! deterministic description of the community, indexed and embedded like code, so broad
//! questions ("how does sync work?") have something to match. Summaries never appear in
//! results: they rank separately and can raise the code of the communities they describe.

use super::communities::Community;
use super::query::summarize;
use crate::chunk::Chunk;
use crate::lang::Lang;

/// Ids and paths of summary chunks start with this; no project file can.
pub const SUMMARY_PREFIX: &str = "@community/";
/// Files listed in a summary's text.
const LISTED_FILES: usize = 40;

/// Whether a chunk id or path names a summary chunk.
pub fn is_summary(id_or_path: &str) -> bool {
    id_or_path.starts_with(SUMMARY_PREFIX)
}

/// The community id a summary chunk id or path belongs to.
pub fn community_of(id_or_path: &str) -> Option<u32> {
    id_or_path
        .strip_prefix(SUMMARY_PREFIX)?
        .split('/')
        .next()?
        .parse()
        .ok()
}

/// The summary chunk of a community with at least two files. Its id carries the content
/// hash, so a changed summary is a new chunk and its old vector is dropped.
pub fn summary_chunk(c: &Community) -> Option<Chunk> {
    if c.files.len() < 2 {
        return None;
    }
    let mut content = summarize(c);
    content.push_str("\nFiles: ");
    content.push_str(
        &c.files
            .iter()
            .take(LISTED_FILES)
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
    );
    if c.files.len() > LISTED_FILES {
        content.push_str(", …");
    }
    content.push('\n');
    let content_hash = blake3::hash(content.as_bytes()).to_hex()[..32].to_string();
    let path = format!("{SUMMARY_PREFIX}{}", c.id);
    Some(Chunk {
        id: format!("{path}/{}", &content_hash[..16]),
        content_hash,
        path,
        lang: Lang::Text,
        start_byte: 0,
        end_byte: content.len(),
        start_line: 1,
        end_line: content.lines().count().max(1),
        scope: vec![format!("community {}", c.label)],
        symbols: Vec::new(),
        refs: Vec::new(),
        calls: Vec::new(),
        method_calls: Vec::new(),
        content,
    })
}

/// Appends an LLM-generated community description to a summary chunk.
/// Updates the content, byte/line bounds, content hash and chunk id.
pub fn append_llm_description(chunk: &mut Chunk, description: &str) {
    let desc = description.trim();
    if desc.is_empty() {
        return;
    }
    if !chunk.content.ends_with('\n') {
        chunk.content.push('\n');
    }
    chunk.content.push_str(desc);
    if !chunk.content.ends_with('\n') {
        chunk.content.push('\n');
    }
    chunk.end_byte = chunk.content.len();
    chunk.end_line = chunk.content.lines().count().max(1);
    chunk.content_hash = blake3::hash(chunk.content.as_bytes()).to_hex()[..32].to_string();
    let cid = community_of(&chunk.id).unwrap_or(0);
    chunk.id = format!("{SUMMARY_PREFIX}{cid}/{}", &chunk.content_hash[..16]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::communities::TopSymbol;

    fn community(files: &[&str]) -> Community {
        Community {
            id: 7,
            label: "pkg/sync · Sync".into(),
            files: files.iter().map(|f| f.to_string()).collect(),
            top_symbols: ["c1", "c2", "c1", "c3", "c4"]
                .iter()
                .map(|c| TopSymbol {
                    name: format!("S{c}"),
                    path: "pkg/sync/sync.go".into(),
                    line: 1,
                    in_degree: 3,
                    chunk: c.to_string(),
                })
                .collect(),
            summary_chunk: None,
        }
    }

    #[test]
    fn summary_chunks_describe_communities() {
        assert!(summary_chunk(&community(&["a.go"])).is_none());
        let c = summary_chunk(&community(&["pkg/sync/sync.go", "pkg/sync/heal.go"])).unwrap();
        assert!(is_summary(&c.id) && is_summary(&c.path));
        assert_eq!(community_of(&c.id), Some(7));
        assert!(c.content.contains("pkg/sync · Sync") && c.content.contains("pkg/sync/heal.go"));
        assert!(!is_summary("pkg/sync/sync.go"));
    }

    #[test]
    fn appends_llm_description_and_updates_id() {
        let mut c = summary_chunk(&community(&["pkg/sync/sync.go", "pkg/sync/heal.go"])).unwrap();
        let orig_id = c.id.clone();
        let orig_hash = c.content_hash.clone();
        append_llm_description(&mut c, "This community coordinates synchronization and healing.");
        assert_ne!(c.id, orig_id);
        assert_ne!(c.content_hash, orig_hash);
        assert!(c.content.contains("This community coordinates synchronization"));
        assert_eq!(community_of(&c.id), Some(7));
    }
}
