//! The result of a clone check as text, JSON or GitHub annotations.

use std::fmt::Write;

use crate::quality::clones::{CloneGroup, CloneReport};

impl CloneReport {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Each group with where it occurs, and a summary line.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for g in &self.groups {
            let _ = writeln!(
                out,
                "clone #{} ({} tokens) in {} places:",
                g.id,
                g.tokens,
                g.locations.len()
            );
            for l in &g.locations {
                let _ = writeln!(out, "    {}:{}-{}", l.path, l.start_line, l.end_line);
            }
        }
        let t = &self.totals;
        let _ = writeln!(
            out,
            "{} files, {} lines of code, {} duplicated ({:.2}%, allowed {}%) in {} groups",
            t.files, t.lines, t.duplicated_lines, t.percent, self.max_percent, t.groups
        );
        out
    }

    /// One workflow command per place a block occurs: an error when the check fails, a warning otherwise.
    pub fn to_github(&self) -> String {
        let level = if self.passed() { "warning" } else { "error" };
        let mut out = String::new();
        for g in &self.groups {
            for l in &g.locations {
                let _ = writeln!(
                    out,
                    "::{level} file={},line={},endLine={},title=Duplicated code::{}",
                    l.path,
                    l.start_line,
                    l.end_line,
                    others(g, &l.path, l.start_line)
                );
            }
        }
        out
    }
}

/// "also at a.rs:3-9, b.rs:10-16" for the other places of a group.
fn others(g: &CloneGroup, path: &str, start: usize) -> String {
    let rest: Vec<String> = g
        .locations
        .iter()
        .filter(|l| !(l.path == path && l.start_line == start))
        .map(|l| format!("{}:{}-{}", l.path, l.start_line, l.end_line))
        .collect();
    format!("these lines are repeated, also at {}", rest.join(", "))
}
