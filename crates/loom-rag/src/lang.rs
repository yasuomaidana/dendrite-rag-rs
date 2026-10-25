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
    C,
    Cpp,
    Markdown,
    Text,
}

const ALL_CODE: [Lang; 9] = [
    Lang::Go,
    Lang::Rust,
    Lang::Python,
    Lang::TypeScript,
    Lang::Tsx,
    Lang::JavaScript,
    Lang::Java,
    Lang::C,
    Lang::Cpp,
];

impl Lang {
    /// Detects the language from the file extension; `None` means the file is not indexed.
    pub fn from_path(path: &Path) -> Option<Lang> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "go" => Lang::Go,
            "rs" => Lang::Rust,
            "py" | "pyi" => Lang::Python,
            "ts" | "mts" | "cts" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            "js" | "mjs" | "cjs" | "jsx" => Lang::JavaScript,
            "java" => Lang::Java,
            "c" | "h" => Lang::C,
            "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => Lang::Cpp,
            "md" | "markdown" | "mdx" => Lang::Markdown,
            "txt" | "rst" | "toml" | "yaml" | "yml" | "json" | "proto" | "sql" | "sh" | "bash" | "zsh"
            | "mod" | "gradle" | "xml" | "html" | "css" | "scss" | "kt" | "kts" | "swift" | "rb" | "php"
            | "cs" | "scala" | "lua" | "zig" | "dart" | "ex" | "exs" | "erl" | "hs" | "ml" | "vue"
            | "svelte" | "tf" | "hcl" | "dockerfile" | "ini" | "cfg" => Lang::Text,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Lang::Go => "go",
            Lang::Rust => "rust",
            Lang::Python => "python",
            Lang::TypeScript => "typescript",
            Lang::Tsx => "tsx",
            Lang::JavaScript => "javascript",
            Lang::Java => "java",
            Lang::C => "c",
            Lang::Cpp => "cpp",
            Lang::Markdown => "markdown",
            Lang::Text => "text",
        }
    }

    /// The tree-sitter grammar, for code languages.
    pub fn grammar(self) -> Option<Language> {
        Some(match self {
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::Java => tree_sitter_java::LANGUAGE.into(),
            Lang::C => tree_sitter_c::LANGUAGE.into(),
            Lang::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Lang::Markdown | Lang::Text => return None,
        })
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
        Lang::C | Lang::Cpp => "(preproc_include path: (_) @import)",
        Lang::Markdown | Lang::Text => "",
    }
}

/// Calls in C and C++, whose stock tag queries capture definitions only.
const C_CALLS: &str = "
(call_expression function: (identifier) @name) @reference.call
(call_expression function: (field_expression field: (field_identifier) @name)) @reference.call
";

const CPP_CALLS: &str = "
(call_expression function: (qualified_identifier name: (identifier) @name)) @reference.call
";

fn compile_tags(lang: Lang) -> Option<Query> {
    let grammar = lang.grammar()?;
    let source = match lang {
        Lang::Go => tree_sitter_go::TAGS_QUERY.to_string(),
        Lang::Rust => tree_sitter_rust::TAGS_QUERY.to_string(),
        Lang::Python => tree_sitter_python::TAGS_QUERY.to_string(),
        // The TypeScript queries extend the JavaScript ones.
        Lang::TypeScript | Lang::Tsx => {
            format!(
                "{}\n{}",
                tree_sitter_javascript::TAGS_QUERY,
                tree_sitter_typescript::TAGS_QUERY
            )
        }
        Lang::JavaScript => tree_sitter_javascript::TAGS_QUERY.to_string(),
        Lang::Java => tree_sitter_java::TAGS_QUERY.to_string(),
        Lang::C => format!("{}\n{C_CALLS}", tree_sitter_c::TAGS_QUERY),
        Lang::Cpp => format!(
            "{}\n{}\n{C_CALLS}\n{CPP_CALLS}",
            tree_sitter_c::TAGS_QUERY,
            tree_sitter_cpp::TAGS_QUERY
        ),
        Lang::Markdown | Lang::Text => return None,
    };
    Query::new(&grammar, &source)
        .or_else(|_| {
            // Fall back to the language's own query when the combined one does not compile.
            let own = match lang {
                Lang::TypeScript | Lang::Tsx => tree_sitter_typescript::TAGS_QUERY,
                Lang::Cpp => tree_sitter_cpp::TAGS_QUERY,
                _ => return Err(()),
            };
            Query::new(&grammar, own).map_err(|_| ())
        })
        .ok()
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
            Lang::C,
            Lang::Cpp
        )]
        lang: Lang,
    ) {
        assert!(lang.tags().is_some(), "{lang:?} tag query does not compile");
        assert!(lang.imports().is_some(), "{lang:?} import query does not compile");
    }
}
