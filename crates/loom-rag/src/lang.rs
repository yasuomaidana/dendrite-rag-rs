//! Languages, file-extension detection and the tree-sitter grammars and tag queries behind them.

use std::path::Path;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tree_sitter::{Language, Query};

/// A language the chunker understands. `Markdown` and `Text` are split without a grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Go,
    Rust,
    Python,
    TypeScript,
    Tsx,
    JavaScript,
    Java,
    Kotlin,
    Svelte,
    C,
    Cpp,
    Markdown,
    Text,
}

const ALL_CODE: [Lang; 10] = [
    Lang::Go,
    Lang::Rust,
    Lang::Python,
    Lang::TypeScript,
    Lang::Tsx,
    Lang::JavaScript,
    Lang::Java,
    Lang::Kotlin,
    Lang::C,
    Lang::Cpp,
];

/// Everything that differs between languages, in one row each: the id, the extensions, the grammar, the
/// `dendrite-core` language that measures it and the tag query. The methods of [`Lang`] only look a row up, so
/// adding a language is adding a row (no `match` per property).
struct Row {
    lang: Lang,
    id: &'static str,
    exts: &'static [&'static str],
    grammar: fn() -> Option<Language>,
    core: Option<dendrite_core::Lang>,
    /// The tag query (definitions and references); `None` when there is none to run.
    tags: fn() -> Option<String>,
    /// A smaller query to fall back on when the combined one does not compile.
    tags_fallback: Option<&'static str>,
}

const TEXT_EXTS: &[&str] = &[
    "txt",
    "rst",
    "toml",
    "yaml",
    "yml",
    "json",
    "proto",
    "sql",
    "sh",
    "bash",
    "zsh",
    "mod",
    "gradle",
    "xml",
    "html",
    "css",
    "scss",
    "swift",
    "rb",
    "php",
    "cs",
    "scala",
    "lua",
    "zig",
    "dart",
    "ex",
    "exs",
    "erl",
    "hs",
    "ml",
    "vue",
    "tf",
    "hcl",
    "dockerfile",
    "ini",
    "cfg",
];

fn ts_tags() -> Option<String> {
    // The TypeScript queries extend the JavaScript ones.
    Some(format!(
        "{}\n{}",
        tree_sitter_javascript::TAGS_QUERY,
        tree_sitter_typescript::TAGS_QUERY
    ))
}

fn c_tags() -> Option<String> {
    Some(format!("{}\n{C_CALLS}", tree_sitter_c::TAGS_QUERY))
}

fn cpp_tags() -> Option<String> {
    Some(format!(
        "{}\n{}\n{C_CALLS}\n{CPP_CALLS}",
        tree_sitter_c::TAGS_QUERY,
        tree_sitter_cpp::TAGS_QUERY
    ))
}

const fn code(
    lang: Lang,
    id: &'static str,
    exts: &'static [&'static str],
    grammar: fn() -> Option<Language>,
    core: dendrite_core::Lang,
    tags: fn() -> Option<String>,
) -> Row {
    Row {
        lang,
        id,
        exts,
        grammar,
        core: Some(core),
        tags,
        tags_fallback: None,
    }
}

const fn plain(lang: Lang, id: &'static str, exts: &'static [&'static str]) -> Row {
    Row {
        lang,
        id,
        exts,
        grammar: || None,
        core: None,
        tags: || None,
        tags_fallback: None,
    }
}

static ROWS: [Row; 13] = [
    code(
        Lang::Go,
        "go",
        &["go"],
        || Some(tree_sitter_go::LANGUAGE.into()),
        dendrite_core::Lang::Go,
        || Some(tree_sitter_go::TAGS_QUERY.to_string()),
    ),
    code(
        Lang::Rust,
        "rust",
        &["rs"],
        || Some(tree_sitter_rust::LANGUAGE.into()),
        dendrite_core::Lang::Rust,
        || Some(tree_sitter_rust::TAGS_QUERY.to_string()),
    ),
    code(
        Lang::Python,
        "python",
        &["py", "pyi"],
        || Some(tree_sitter_python::LANGUAGE.into()),
        dendrite_core::Lang::Python,
        || Some(tree_sitter_python::TAGS_QUERY.to_string()),
    ),
    Row {
        tags_fallback: Some(tree_sitter_typescript::TAGS_QUERY),
        ..code(
            Lang::TypeScript,
            "typescript",
            &["ts", "mts", "cts"],
            || Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
            dendrite_core::Lang::TypeScript,
            ts_tags,
        )
    },
    Row {
        tags_fallback: Some(tree_sitter_typescript::TAGS_QUERY),
        ..code(
            Lang::Tsx,
            "tsx",
            &["tsx"],
            || Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
            dendrite_core::Lang::Tsx,
            ts_tags,
        )
    },
    code(
        Lang::JavaScript,
        "javascript",
        &["js", "mjs", "cjs", "jsx"],
        || Some(tree_sitter_javascript::LANGUAGE.into()),
        dendrite_core::Lang::JavaScript,
        || Some(tree_sitter_javascript::TAGS_QUERY.to_string()),
    ),
    code(
        Lang::Java,
        "java",
        &["java"],
        || Some(tree_sitter_java::LANGUAGE.into()),
        dendrite_core::Lang::Java,
        || Some(tree_sitter_java::TAGS_QUERY.to_string()),
    ),
    code(
        Lang::Kotlin,
        "kotlin",
        &["kt", "kts"],
        || Some(tree_sitter_kotlin_ng::LANGUAGE.into()),
        dendrite_core::Lang::Kotlin,
        || Some(KOTLIN_TAGS.to_string()),
    ),
    // A Svelte component is chunked through the scripts `dendrite-core` finds in it, so it has no tag query.
    code(
        Lang::Svelte,
        "svelte",
        &["svelte"],
        || Some(tree_sitter_svelte_ng::LANGUAGE.into()),
        dendrite_core::Lang::Svelte,
        || None,
    ),
    code(
        Lang::C,
        "c",
        &["c", "h"],
        || Some(tree_sitter_c::LANGUAGE.into()),
        dendrite_core::Lang::C,
        c_tags,
    ),
    Row {
        tags_fallback: Some(tree_sitter_cpp::TAGS_QUERY),
        ..code(
            Lang::Cpp,
            "cpp",
            &["cc", "cpp", "cxx", "hpp", "hh", "hxx"],
            || Some(tree_sitter_cpp::LANGUAGE.into()),
            dendrite_core::Lang::Cpp,
            cpp_tags,
        )
    },
    plain(Lang::Markdown, "markdown", &["md", "markdown", "mdx"]),
    plain(Lang::Text, "text", TEXT_EXTS),
];

impl Lang {
    fn row(self) -> &'static Row {
        ROWS.iter()
            .find(|r| r.lang == self)
            .expect("every language has a row")
    }

    /// Detects the language from the file extension; `None` means the file is not indexed.
    pub fn from_path(path: &Path) -> Option<Lang> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        ROWS.iter()
            .find(|r| r.exts.contains(&ext.as_str()))
            .map(|r| r.lang)
    }

    pub fn as_str(self) -> &'static str {
        self.row().id
    }

    /// The tree-sitter grammar, for code languages.
    pub fn grammar(self) -> Option<Language> {
        (self.row().grammar)()
    }

    /// The `dendrite-core` language that measures and parses files of this language, if there is one.
    pub fn core(self) -> Option<dendrite_core::Lang> {
        self.row().core
    }

    /// The language that `dendrite-core` names, for the code it finds inside a file of another language.
    pub fn from_core(core: dendrite_core::Lang) -> Option<Lang> {
        ALL_CODE.iter().copied().find(|l| l.core() == Some(core))
    }

    /// The grammar's tag query (definitions and references), compiled once per language.
    /// `None` when the language has no grammar or its query does not compile.
    pub fn tags(self) -> Option<&'static Query> {
        static TAGS: OnceLock<Vec<(Lang, Option<Query>)>> = OnceLock::new();
        let all = TAGS.get_or_init(|| ALL_CODE.iter().map(|&l| (l, compile_tags(l))).collect());
        all.iter().find(|(l, _)| *l == self).and_then(|(_, q)| q.as_ref())
    }

    /// The import query (`@import` captures the module specifier, `@import.mod` a Rust
    /// `mod name;` declaration), compiled once per language.
    pub fn imports(self) -> Option<&'static Query> {
        static IMPORTS: OnceLock<Vec<(Lang, Option<Query>)>> = OnceLock::new();
        let all = IMPORTS.get_or_init(|| {
            ALL_CODE
                .iter()
                .map(|&l| (l, l.grammar().and_then(|g| Query::new(&g, import_query(l)).ok())))
                .collect()
        });
        all.iter().find(|(l, _)| *l == self).and_then(|(_, q)| q.as_ref())
    }
}

/// Import statements per language. Specifiers keep their quotes; the chunker strips them.
fn import_query(lang: Lang) -> &'static str {
    match lang {
        Lang::Go => "(import_spec path: (_) @import)",
        Lang::Rust => {
            "(use_declaration argument: (_) @import)
             (mod_item !body name: (identifier) @import.mod)"
        }
        Lang::Python => {
            "(import_statement name: (dotted_name) @import)
             (import_statement name: (aliased_import name: (dotted_name) @import))
             (import_from_statement module_name: (_) @import)"
        }
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => {
            "(import_statement source: (string) @import)
             (export_statement source: (string) @import)
             ((call_expression function: (identifier) @_f arguments: (arguments (string) @import))
              (#eq? @_f \"require\"))"
        }
        Lang::Java => "(import_declaration (_) @import)",
        Lang::Kotlin => "(import (qualified_identifier) @import)",
        Lang::C | Lang::Cpp => "(preproc_include path: (_) @import)",
        Lang::Svelte | Lang::Markdown | Lang::Text => "",
    }
}

/// Calls in C and C++, whose stock tag queries capture definitions only.
const C_CALLS: &str = "
(call_expression function: (identifier) @name) @reference.call
(call_expression function: (field_expression field: (field_identifier) @name)) @reference.call
";

/// Kotlin has no stock tag query: classes, interfaces and objects, functions, and calls (plain, or through a
/// receiver, whose name is the last identifier of the navigation).
const KOTLIN_TAGS: &str = "
(class_declaration name: (identifier) @name) @definition.class
(object_declaration name: (identifier) @name) @definition.class
(function_declaration name: (identifier) @name) @definition.function
(call_expression . (identifier) @name) @reference.call
(call_expression . (navigation_expression (identifier) @name .)) @reference.call
";

const CPP_CALLS: &str = "
(call_expression function: (qualified_identifier name: (identifier) @name)) @reference.call
";

fn compile_tags(lang: Lang) -> Option<Query> {
    let row = lang.row();
    let grammar = (row.grammar)()?;
    let source = (row.tags)()?;
    Query::new(&grammar, &source)
        .ok()
        // Fall back to the language's own query when the combined one does not compile.
        .or_else(|| Query::new(&grammar, row.tags_fallback?).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("a/b.go", Some(Lang::Go))]
    #[case("x.TSX", Some(Lang::Tsx))]
    #[case("README.md", Some(Lang::Markdown))]
    #[case("Cargo.toml", Some(Lang::Text))]
    #[case("A.kt", Some(Lang::Kotlin))]
    #[case("build.gradle.kts", Some(Lang::Kotlin))]
    #[case("App.svelte", Some(Lang::Svelte))]
    #[case("logo.png", None)]
    #[case("Makefile", None)]
    fn detects_languages_by_extension(#[case] path: &str, #[case] expected: Option<Lang>) {
        assert_eq!(Lang::from_path(Path::new(path)), expected);
    }

    #[rstest]
    fn every_code_language_has_a_tag_query(
        #[values(
            Lang::Go,
            Lang::Rust,
            Lang::Python,
            Lang::TypeScript,
            Lang::Tsx,
            Lang::JavaScript,
            Lang::Java,
            Lang::Kotlin,
            Lang::C,
            Lang::Cpp
        )]
        lang: Lang,
    ) {
        assert!(lang.tags().is_some(), "{lang:?} tag query does not compile");
        assert!(lang.imports().is_some(), "{lang:?} import query does not compile");
    }
}
