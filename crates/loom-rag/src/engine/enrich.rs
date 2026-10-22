//! Index-time enrichment: graph communities with their summary chunks, and optional LLM
//! concepts for prose files.

use super::*;

/// Characters of a document sent for concept extraction.
const CONCEPT_TEXT_CHARS: usize = 12_000;

/// Documents concepts are extracted from: the documentation corpus, not the configuration
/// and data files that are also indexed as text.
fn is_prose(f: &SourceFile) -> bool {
    Corpus::of(&f.path, f.lang) == Corpus::Docs
}

impl Engine {
    /// Recomputes the communities of the code graph and stores them; returns their count.
    /// Each community of two or more files gets a summary chunk, replacing the last run's:
    /// lexical index first, then the ledger, like an index run; vectors follow in
    /// `sync_vectors`.
    pub(super) fn update_communities(&self, report: &mut IndexReport) -> Result<usize> {
        use crate::graph::summary;
        let graph = self.code_graph()?;
        let previous = self.ledger.community_files()?;
        let old_summaries: Vec<String> = self
            .ledger
            .communities()?
            .into_iter()
            .filter_map(|c| c.summary_chunk)
            .collect();
        let mut communities = crate::graph::communities::detect(&graph, &previous, self.cfg.graph_resolution);

        let mut summaries: Vec<Chunk> = Vec::new();
        let mut new_cache: Vec<(String, String, String)> = Vec::new();

        let use_llm_summaries =
            self.cfg.graph_llm.enabled && self.cfg.graph_llm.summaries && self.llm.is_some();

        for c in &mut communities {
            let Some(mut chunk) = summary::summary_chunk(c) else {
                continue;
            };

            if use_llm_summaries {
                let llm = self.llm.as_ref().unwrap();
                let model = llm.model();
                let mut top_symbol_lines = Vec::new();
                for sym in &c.top_symbols {
                    if let Ok(Some(sc)) = self.ledger.chunk(&sym.chunk)
                        && let Some(first_line) = sc.content.lines().find(|l| !l.trim().is_empty())
                    {
                        top_symbol_lines.push(format!("{}: {}", sym.name, first_line.trim()));
                    }
                }
                let prompt = crate::llm::build_community_summary_prompt(&chunk.content, &top_symbol_lines);
                let prompt_hash = blake3::hash(prompt.as_bytes()).to_hex().to_string();

                // After one failure the gateway is not asked again in this run: a down
                // gateway must not cost one timeout per community.
                let desc_res = match self.ledger.llm_cache(model, &prompt_hash)? {
                    Some(cached) => Ok(cached),
                    None if report.llm_error.is_some() => Err(()),
                    None => match llm.complete(&prompt) {
                        Ok(desc) => {
                            new_cache.push((model.to_string(), prompt_hash, desc.clone()));
                            Ok(desc)
                        }
                        Err(e) => {
                            report.llm_error = Some(e.to_string());
                            Err(())
                        }
                    },
                };

                if let Ok(desc) = desc_res {
                    summary::append_llm_description(&mut chunk, &desc);
                }
            }

            c.summary_chunk = Some(chunk.id.clone());
            summaries.push(chunk);
        }

        let mut lw = self.lexical.writer()?;
        for id in &old_summaries {
            if let Some(cid) = summary::community_of(id) {
                lw.remove_file(&format!("{}{cid}", summary::SUMMARY_PREFIX));
            }
        }
        for c in &summaries {
            lw.remove_file(&c.path);
            lw.add(c)?;
        }
        lw.commit()?;

        let mut batch = self.ledger.batch()?;
        for (model, prompt_hash, desc) in new_cache {
            batch.put_llm_cache(&model, &prompt_hash, &desc)?;
        }
        for id in &old_summaries {
            batch.remove_chunk(id)?;
        }
        for c in &summaries {
            batch.put_chunk(c)?;
        }
        batch.put_communities(&communities, &self.ledger.files()?)?;
        batch.commit()?;
        Ok(communities.len())
    }

    /// Extracts up to 8 concepts per Markdown/text file when `concepts` is enabled.
    pub(super) fn enrich_concepts(
        &self,
        scanned: &[(SourceFile, String)],
        added: &BTreeSet<String>,
        report: &mut IndexReport,
    ) -> Result<()> {
        if !self.cfg.graph_llm.enabled || !self.cfg.graph_llm.concepts {
            return Ok(());
        }
        let Some(llm) = &self.llm else {
            return Ok(());
        };
        let model = llm.model();

        let mut batch = self.ledger.batch()?;
        for (f, _) in scanned {
            if !is_prose(f) {
                continue;
            }
            let is_new_or_modified = added.contains(&f.path);
            let has_concepts = self.ledger.concepts(&f.path)?.is_some();
            if !is_new_or_modified && has_concepts {
                continue;
            }
            let Some(text) = read_text(&f.abs) else {
                continue;
            };
            if text.trim().is_empty() {
                continue;
            }

            let text: String = text.chars().take(CONCEPT_TEXT_CHARS).collect();
            let prompt = crate::llm::build_concepts_prompt(&text);
            let prompt_hash = blake3::hash(prompt.as_bytes()).to_hex().to_string();

            let raw_res = match self.ledger.llm_cache(model, &prompt_hash)? {
                Some(cached) => Ok(cached),
                None if report.llm_error.is_some() => Err(()),
                None => match llm.complete_json(&prompt) {
                    Ok(resp) => {
                        batch.put_llm_cache(model, &prompt_hash, &resp)?;
                        Ok(resp)
                    }
                    Err(e) => {
                        report.llm_error = Some(e.to_string());
                        Err(())
                    }
                },
            };

            if let Ok(resp) = raw_res
                && let Some(concepts) = crate::llm::parse_concepts(&resp)
            {
                let json = serde_json::to_string(&concepts).map_err(crate::ledger::LedgerError::from)?;
                batch.put_concepts(&f.path, &json)?;
            }
        }
        batch.commit()?;
        Ok(())
    }
}
