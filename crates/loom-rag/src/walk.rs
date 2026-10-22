//! gitignore-aware project walk (`docs/specs/chunking.md` § Walking).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::{WalkBuilder, WalkState};

use crate::lang::Lang;

/// Directories never indexed, whatever the ignore files say.
const ALWAYS_SKIPPED: [&str; 8] = [
    ".git",
    "target",
    "node_modules",
    "vendor",
    "dist",
    ".loom",
    "__pycache__",
    ".venv",
];

/// Walk settings.
#[derive(Debug, Clone)]
pub struct WalkOptions {
    /// Files larger than this are skipped.
    pub max_file_bytes: u64,
    /// Extra glob patterns (relative to the root) to skip.
    pub exclude: Vec<String>,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            max_file_bytes: 1 << 20,
            exclude: Vec::new(),
        }
    }
}

/// A file selected for indexing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    /// Path relative to the root, `/`-separated.
    pub path: String,
    pub abs: PathBuf,
    pub lang: Lang,
    pub size: u64,
    /// Modification time in nanoseconds since the Unix epoch (0 when unavailable).
    pub mtime_ns: u128,
}

/// Lists indexable files under `root`, sorted by path.
pub fn walk(root: &Path, opts: &WalkOptions) -> anyhow::Result<Vec<SourceFile>> {
    let exclude = build_globs(&opts.exclude)?;
    let found = Mutex::new(Vec::new());
    WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .filter_entry(|e| !(e.depth() > 0 && ALWAYS_SKIPPED.iter().any(|s| e.file_name() == *s)))
        .build_parallel()
        .run(|| {
            let found = &found;
            let exclude = &exclude;
            Box::new(move |entry| {
                let Ok(entry) = entry else {
                    return WalkState::Continue;
                };
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    return WalkState::Continue;
                }
                if let Some(file) = select(root, entry.path(), opts, exclude) {
                    found.lock().expect("walk mutex").push(file);
                }
                WalkState::Continue
            })
        });
    let mut files = found.into_inner().expect("walk mutex");
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn select(root: &Path, abs: &Path, opts: &WalkOptions, exclude: &GlobSet) -> Option<SourceFile> {
    let lang = Lang::from_path(abs)?;
    let rel = abs.strip_prefix(root).ok()?;
    let path = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    if exclude.is_match(&path) {
        return None;
    }
    let meta = abs.metadata().ok()?;
    if meta.len() > opts.max_file_bytes || meta.len() == 0 {
        return None;
    }
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    Some(SourceFile {
        path,
        abs: abs.to_path_buf(),
        lang,
        size: meta.len(),
        mtime_ns,
    })
}

fn build_globs(patterns: &[String]) -> anyhow::Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        b.add(Glob::new(p)?);
    }
    Ok(b.build()?)
}

/// Reads a file as UTF-8 text; `None` for binary content (a NUL byte in the first 8 KiB)
/// or undecodable bytes.
pub fn read_text(abs: &Path) -> Option<String> {
    let bytes = std::fs::read(abs).ok()?;
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn walks_respecting_ignores_and_filters() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::create_dir_all(root.join("gen")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("src/skip.go"), "package x\n").unwrap();
        fs::write(root.join("target/debug/out.rs"), "fn x() {}\n").unwrap();
        fs::write(root.join("gen/big.py"), "x = 1\n".repeat(10)).unwrap();
        fs::write(root.join("logo.png"), [0u8, 1, 2]).unwrap();
        fs::write(root.join(".gitignore"), "skip.go\n").unwrap();

        let opts = WalkOptions {
            max_file_bytes: 50,
            exclude: vec![],
        };
        let files: Vec<_> = walk(root, &opts).unwrap().into_iter().map(|f| f.path).collect();
        assert_eq!(files, vec!["src/main.rs"]);

        let opts = WalkOptions {
            max_file_bytes: 1 << 20,
            exclude: vec!["src/**".into()],
        };
        let files: Vec<_> = walk(root, &opts).unwrap().into_iter().map(|f| f.path).collect();
        assert_eq!(files, vec!["gen/big.py"]);
    }

    #[test]
    fn binary_files_are_not_text() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.txt");
        fs::write(&p, b"abc\0def").unwrap();
        assert!(read_text(&p).is_none());
        fs::write(&p, "héllo").unwrap();
        assert_eq!(read_text(&p).as_deref(), Some("héllo"));
    }
}
