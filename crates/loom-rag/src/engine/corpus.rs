//! Code and documentation corpora (`docs/specs/retrieval.md` § Corpora).
//!
//! Every chunk belongs to one corpus, derived from its path and language, so the ledger
//! and the stores need no extra column.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::lang::Lang;

/// The corpus a chunk belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Corpus {
    /// Source code in any language, configuration, and community summaries.
    Code,
    /// Prose: Markdown, plain text and reStructuredText.
    Docs,
}

impl Corpus {
    /// Markdown, `.txt` and `.rst` files are documentation. Everything else is code,
    /// including the code and configuration files indexed as [`Lang::Text`].
    pub fn of(path: &str, lang: Lang) -> Self {
        let prose = match lang {
            Lang::Markdown => true,
            Lang::Text => {
                let ext = Path::new(path)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                matches!(ext.as_str(), "txt" | "rst")
            }
            _ => false,
        };
        if prose { Corpus::Docs } else { Corpus::Code }
    }
}

/// Which corpora a query searches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CorpusFilter {
    /// Code and documentation: one ranking for search, both pipelines for context packs.
    #[default]
    All,
    Code,
    Docs,
}

impl CorpusFilter {
    pub fn admits(self, c: Corpus) -> bool {
        match self {
            CorpusFilter::All => true,
            CorpusFilter::Code => c == Corpus::Code,
            CorpusFilter::Docs => c == Corpus::Docs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("README.md", Lang::Markdown, Corpus::Docs)]
    #[case("docs/guide.rst", Lang::Text, Corpus::Docs)]
    #[case("NOTES.TXT", Lang::Text, Corpus::Docs)]
    #[case("src/main.rs", Lang::Rust, Corpus::Code)]
    #[case("app/Main.kt", Lang::Text, Corpus::Code)]
    #[case("scripts/build.sh", Lang::Text, Corpus::Code)]
    #[case("Cargo.toml", Lang::Text, Corpus::Code)]
    #[case("@community/3/summary", Lang::Text, Corpus::Code)]
    fn prose_is_docs_and_everything_else_is_code(
        #[case] path: &str,
        #[case] lang: Lang,
        #[case] expected: Corpus,
    ) {
        assert_eq!(Corpus::of(path, lang), expected);
    }

    #[rstest]
    #[case(CorpusFilter::All, Corpus::Docs, true)]
    #[case(CorpusFilter::Code, Corpus::Code, true)]
    #[case(CorpusFilter::Code, Corpus::Docs, false)]
    #[case(CorpusFilter::Docs, Corpus::Code, false)]
    fn filters_admit_their_corpora(
        #[case] filter: CorpusFilter,
        #[case] corpus: Corpus,
        #[case] expected: bool,
    ) {
        assert_eq!(filter.admits(corpus), expected);
    }

    #[test]
    fn filter_deserializes_from_json() {
        assert_eq!(
            serde_json::from_str::<CorpusFilter>("\"docs\"").unwrap(),
            CorpusFilter::Docs
        );
    }
}
