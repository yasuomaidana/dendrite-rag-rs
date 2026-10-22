//! What `graph.explain` reports: callers, callees, imports and community of a symbol or
//! file.

use super::*;

/// Community lookup by file.
pub(super) struct Communities {
    by_file: BTreeMap<String, u32>,
    labels: HashMap<u32, String>,
}

impl Communities {
    pub(super) fn load(engine: &Engine) -> Result<Self> {
        Ok(Self {
            by_file: engine.ledger.community_files()?,
            labels: engine
                .ledger
                .communities()?
                .into_iter()
                .map(|c| (c.id, c.label))
                .collect(),
        })
    }

    pub(super) fn of(&self, path: &str) -> Option<CommunityRef> {
        let id = *self.by_file.get(path)?;
        Some(CommunityRef {
            id,
            label: self.labels.get(&id).cloned().unwrap_or_default(),
        })
    }
}

fn neighbours(g: &CodeGraph, it: impl Iterator<Item = (u32, EdgeKind)>) -> Vec<Neighbour> {
    let mut v: Vec<(u32, EdgeKind)> = it.filter(|(s, _)| !g.sites(*s).is_empty()).collect();
    v.sort_by_key(|&(s, k)| (std::cmp::Reverse(g.in_degree(s)), g.symbol_name(s).to_string(), k));
    v.into_iter()
        .take(EXPLAIN_NEIGHBOURS)
        .map(|(s, kind)| {
            let site = &g.sites(s)[0];
            Neighbour {
                symbol: g.symbol_name(s).to_string(),
                path: site.path.clone(),
                line: site.line,
                kind,
            }
        })
        .collect()
}

pub(super) fn symbol_summary(g: &CodeGraph, s: u32, community: Option<&CommunityRef>) -> String {
    let site = &g.sites(s)[0];
    let files: HashSet<u32> = g.users(s).map(|(u, _)| g.symbol_file(u)).collect();
    let mut text = format!(
        "{} is defined at {}:{} and used by {} symbols in {} files; it uses {} symbols.",
        g.symbol_name(s),
        site.path,
        site.line,
        g.in_degree(s),
        files.len(),
        g.out_degree(s)
    );
    if let Some(c) = community {
        text.push_str(&format!(" It belongs to community {} ({}).", c.id, c.label));
    }
    if g.in_degree(s) >= GOD_NODE_IN_DEGREE {
        text.push_str(" It is a god node: changes to it reach much of the codebase.");
    }
    text
}

pub(super) fn explain_symbol(g: &CodeGraph, s: u32, comms: &Communities) -> Explanation {
    let community = comms.of(g.file_name(g.symbol_file(s)));
    Explanation::Symbol {
        summary: symbol_summary(g, s, community.as_ref()),
        symbol: g.symbol_name(s).to_string(),
        sites: g.sites(s).to_vec(),
        god_node: g.in_degree(s) >= GOD_NODE_IN_DEGREE,
        in_degree: g.in_degree(s),
        out_degree: g.out_degree(s),
        users: neighbours(g, g.users(s)),
        uses: neighbours(g, g.uses(s)),
        community,
    }
}

pub(super) fn explain_file(
    g: &CodeGraph,
    f: u32,
    comms: &Communities,
    concepts: Vec<crate::llm::Concept>,
) -> Explanation {
    let path = g.file_name(f).to_string();
    let mut defines: Vec<u32> = (0..g.symbol_count() as u32)
        .filter(|&s| g.symbol_file(s) == f && !g.sites(s).is_empty())
        .collect();
    defines.sort_by_key(|&s| (std::cmp::Reverse(g.in_degree(s)), g.symbol_name(s).to_string()));
    let defines: Vec<TopSymbol> = defines
        .into_iter()
        .take(EXPLAIN_NEIGHBOURS)
        .map(|s| TopSymbol {
            name: g.symbol_name(s).to_string(),
            path: path.clone(),
            line: g.sites(s)[0].line,
            in_degree: g.in_degree(s),
            chunk: g.sites(s)[0].chunk.clone(),
        })
        .collect();
    let mut imports = Vec::new();
    let mut out_weight: HashMap<u32, f32> = HashMap::new();
    for (t, k, w) in g.file_edges(f) {
        if k == EdgeKind::Import {
            imports.push(g.file_name(t).to_string());
        }
        *out_weight.entry(t).or_default() += w;
    }
    let mut imported_by = Vec::new();
    let mut in_weight: HashMap<u32, f32> = HashMap::new();
    for src in 0..g.file_count() as u32 {
        for (_, k, w) in g.file_edges(src).filter(|(t, _, _)| *t == f) {
            if k == EdgeKind::Import {
                imported_by.push(g.file_name(src).to_string());
            }
            *in_weight.entry(src).or_default() += w;
        }
    }
    imported_by.sort();
    imported_by.dedup();
    let heaviest = |m: HashMap<u32, f32>| -> Vec<String> {
        let mut v: Vec<(u32, f32)> = m.into_iter().collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        v.into_iter()
            .take(EXPLAIN_NEIGHBOURS)
            .map(|(t, _)| g.file_name(t).to_string())
            .collect()
    };
    let community = comms.of(&path);
    let depends_on = heaviest(out_weight);
    let used_by = heaviest(in_weight);
    let mut summary = format!(
        "{path} defines {} symbols, depends on {} files and is used by {} files.",
        defines.len(),
        depends_on.len(),
        used_by.len()
    );
    if let Some(c) = &community {
        summary.push_str(&format!(" It belongs to community {} ({}).", c.id, c.label));
    }
    Explanation::File {
        external_imports: g.external_imports(f).to_vec(),
        path,
        community,
        concepts,
        defines,
        imports,
        imported_by,
        depends_on,
        used_by,
        summary,
    }
}
