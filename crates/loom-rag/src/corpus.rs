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
    /// Code and documentation in one ranking (the behaviour before corpora existed).
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

    #[test]
    fn prose_is_docs_and_everything_else_is_code() {
        assert_eq!(Corpus::of("README.md", Lang::Markdown), Corpus::Docs);
        assert_eq!(Corpus::of("docs/guide.rst", Lang::Text), Corpus::Docs);
        assert_eq!(Corpus::of("NOTES.TXT", Lang::Text), Corpus::Docs);
        assert_eq!(Corpus::of("src/main.rs", Lang::Rust), Corpus::Code);
        assert_eq!(Corpus::of("app/Main.kt", Lang::Text), Corpus::Code);
        assert_eq!(Corpus::of("scripts/build.sh", Lang::Text), Corpus::Code);
        assert_eq!(Corpus::of("Cargo.toml", Lang::Text), Corpus::Code);
        assert_eq!(Corpus::of("@community/3/summary", Lang::Text), Corpus::Code);
    }

    #[test]
    fn filters_admit_their_corpora() {
        assert!(CorpusFilter::All.admits(Corpus::Docs));
        assert!(CorpusFilter::Code.admits(Corpus::Code));
        assert!(!CorpusFilter::Code.admits(Corpus::Docs));
        assert!(!CorpusFilter::Docs.admits(Corpus::Code));
        assert_eq!(
            serde_json::from_str::<CorpusFilter>("\"docs\"").unwrap(),
            CorpusFilter::Docs
        );
    }
}
