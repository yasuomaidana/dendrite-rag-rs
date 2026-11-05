//! Production, test, comment and blank lines of one file.

pub mod regions;

use crate::lang::Lang;
use regions::{Marked, Region};

/// The lines of one file by kind. A line holding code and a comment is a code line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LocCount {
    pub production: usize,
    pub test: usize,
    pub comment: usize,
    pub blank: usize,
    /// The grammar met a syntax error; the counts still stand but may be off around it.
    pub parse_errors: bool,
}

/// Extensions of code files that have no grammar here: every non-blank line counts as production.
const PLAIN_CODE: [&str; 11] = [
    "sh", "bash", "kt", "kts", "swift", "rb", "php", "cs", "scala", "lua", "zig",
];

/// Whether `lang` and `path` name a file whose production lines are worth counting.
pub fn counts_as_code(lang: Lang, path: &str) -> bool {
    lang.grammar().is_some() || (lang == Lang::Text && PLAIN_CODE.contains(&extension(path).as_str()))
}

fn extension(path: &str) -> String {
    path.rsplit('.').next().unwrap_or("").to_ascii_lowercase()
}

/// Name endings, before the extension, that mark a whole file as test code in any language.
const TEST_STEM_SUFFIXES: [&str; 4] = ["_test", "_tests", ".test", ".spec"];

/// Whether the whole file is test code, by where it lives and what it is called.
pub fn is_test_path(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    let (name, dirs) = parts.split_last().map_or(("", &[][..]), |(n, d)| (*n, d));
    if dirs.iter().any(|d| matches!(*d, "tests" | "test" | "__tests__")) {
        return true;
    }
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    TEST_STEM_SUFFIXES.iter().any(|s| stem.ends_with(s)) || is_test_stem(stem, ext)
}

/// The test-file naming conventions of one language.
fn is_test_stem(stem: &str, ext: &str) -> bool {
    match ext {
        "py" => stem.starts_with("test_") || stem == "conftest",
        "c" | "cc" | "cpp" | "cxx" => stem.starts_with("test_"),
        "java" => stem.ends_with("Test") || stem.ends_with("Tests"),
        "rs" => stem == "tests",
        _ => false,
    }
}

/// Counts the lines of `src`, a file at `path` (`/`-separated, relative to the project).
pub fn count_lines(path: &str, lang: Lang, src: &str) -> LocCount {
    if is_test_path(path) {
        return classify(
            src,
            &[Marked {
                range: 0..src.len(),
                kind: Region::Test,
            }],
            false,
        );
    }
    match regions::find(lang, src) {
        Some(r) => classify(src, &r.marks, r.parse_errors),
        None => classify(src, &[], false),
    }
}

fn classify(src: &str, marks: &[Marked], parse_errors: bool) -> LocCount {
    let mut count = LocCount {
        parse_errors,
        ..LocCount::default()
    };
    let bytes = src.as_bytes();
    let mut m = 0;
    let mut start = 0;
    while start < bytes.len() {
        let end = bytes[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |p| start + p);
        let (mut code, mut test, mut comment) = (false, false, false);
        for (offset, &b) in bytes[start..end].iter().enumerate() {
            if b.is_ascii_whitespace() {
                continue;
            }
            let at = start + offset;
            while m < marks.len() && marks[m].range.end <= at {
                m += 1;
            }
            match marks
                .get(m)
                .filter(|mark| mark.range.start <= at)
                .map(|mark| mark.kind)
            {
                Some(Region::Test) => test = true,
                Some(Region::Comment) => comment = true,
                None => code = true,
            }
        }
        match (code, test, comment) {
            (true, _, _) => count.production += 1,
            (_, true, _) => count.test += 1,
            (_, _, true) => count.comment += 1,
            _ => count.blank += 1,
        }
        start = end + 1;
    }
    count
}
