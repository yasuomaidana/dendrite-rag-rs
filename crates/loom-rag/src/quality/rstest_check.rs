//! Rust tests that should use rstest: tests that differ only in constants, and setup helpers that should be
//! fixtures (`docs/specs/quality.md` § rstest).

use std::collections::HashMap;
use std::fmt::Write;
use std::path::Path;

use rayon::prelude::*;
use serde::Serialize;
use tree_sitter::{Node, Parser};

use crate::lang::Lang;
use crate::quality::rstest_dep::check_manifest;
use crate::quality::syntax::{Limits, Normalize, walk};
use crate::walk::{WalkOptions, walk as walk_tree};

/// A test that should be restructured.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Violation {
    /// `repeated_test`, `helper_should_be_fixture`, `missing_rstest` or `outdated_rstest`.
    pub kind: String,
    pub path: String,
    pub line: usize,
    pub message: String,
}

/// The result of an rstest check.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RstestReport {
    pub schema: u32,
    pub files: usize,
    pub violations: Vec<Violation>,
}

impl RstestReport {
    pub fn passed(&self) -> bool {
        self.violations.is_empty()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for v in &self.violations {
            let _ = writeln!(out, "{}:{}  [{}] {}", v.path, v.line, v.kind, v.message);
        }
        let _ = writeln!(
            out,
            "{} Rust files checked, {} violations",
            self.files,
            self.violations.len()
        );
        out
    }

    pub fn to_github(&self) -> String {
        let mut out = String::new();
        for v in &self.violations {
            let _ = writeln!(
                out,
                "::error file={},line={},title=Use rstest::{}",
                v.path, v.line, v.message
            );
        }
        out
    }
}

/// A test body this small is not worth parameterising.
const MIN_BODY_LINES: usize = 3;
const MIN_BODY_TOKENS: usize = 12;

/// Checks the Rust files under `root`.
pub fn check_rstest(root: &Path, exclude: &[String]) -> anyhow::Result<RstestReport> {
    let files = walk_tree(
        root,
        &WalkOptions {
            max_file_bytes: u64::MAX,
            exclude: exclude.to_vec(),
        },
    )?;
    let rust: Vec<_> = files.iter().filter(|f| f.lang == Lang::Rust).collect();
    let checked: Vec<(Vec<Violation>, Option<Violation>)> = rust
        .par_iter()
        .filter_map(|f| {
            let bytes = std::fs::read(&f.abs).ok()?;
            let text = String::from_utf8_lossy(&bytes);
            let dependency = has_plain_test(&text)
                .then(|| check_manifest(root, &f.abs))
                .flatten();
            Some((check_source(&f.path, &text), dependency))
        })
        .collect();
    let mut violations: Vec<Violation> = Vec::new();
    for (found, dependency) in checked {
        violations.extend(found);
        if let Some(d) = dependency.filter(|d| !violations.contains(d)) {
            violations.push(d);
        }
    }
    violations.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    Ok(RstestReport {
        schema: 1,
        files: rust.len(),
        violations,
    })
}

/// What the attributes before an item say it is.
#[derive(Default, Clone, Copy)]
struct Role {
    plain_test: bool,
    parameterised: bool,
}

fn role(children: &[Node], i: usize, src: &[u8]) -> Role {
    let mut r = Role::default();
    for n in children[..i]
        .iter()
        .rev()
        .take_while(|n| matches!(n.kind(), "attribute_item" | "line_comment" | "block_comment"))
    {
        let text: String = std::str::from_utf8(&src[n.byte_range()])
            .unwrap_or("")
            .split_whitespace()
            .collect();
        let name = text
            .trim_start_matches("#[")
            .split(['(', ']'])
            .next()
            .unwrap_or("");
        match name {
            "rstest" | "fixture" | "case" | "test_case" => r.parameterised = true,
            "test" => r.plain_test = true,
            n if n.ends_with("::test") => r.plain_test = true,
            _ => {}
        }
    }
    r
}

struct TestFn<'a> {
    name: String,
    line: usize,
    body: Node<'a>,
}

fn function_name(node: Node, src: &[u8]) -> String {
    node.child_by_field_name("name")
        .map(|n| String::from_utf8_lossy(&src[n.byte_range()]).into_owned())
        .unwrap_or_default()
}

/// Collects, for every container (the file and each module), its plain tests and its undecorated functions.
fn scan<'a>(container: Node<'a>, src: &[u8], out: &mut Vec<(Vec<TestFn<'a>>, Vec<TestFn<'a>>)>) {
    let mut cursor = container.walk();
    let children: Vec<Node> = container.children(&mut cursor).collect();
    let (mut tests, mut helpers) = (Vec::new(), Vec::new());
    for (i, child) in children.iter().enumerate() {
        match child.kind() {
            "function_item" => {
                let r = role(&children, i, src);
                let Some(body) = child.child_by_field_name("body") else {
                    continue;
                };
                let f = TestFn {
                    name: function_name(*child, src),
                    line: child.start_position().row + 1,
                    body,
                };
                if r.plain_test {
                    tests.push(f);
                } else if !r.parameterised {
                    helpers.push(f);
                }
            }
            "mod_item" => {
                if let Some(body) = child.child_by_field_name("body") {
                    scan(body, src, out);
                }
            }
            _ => {}
        }
    }
    out.push((tests, helpers));
}

/// Does the function `body` bind the result of calling `helper` (`let x = helper(...)`)?
fn binds_result_of(body: Node, helper: &str, src: &[u8]) -> bool {
    let mut stack = vec![body];
    while let Some(n) = stack.pop() {
        if n.kind() == "let_declaration"
            && n.child_by_field_name("value")
                .is_some_and(|v| calls(v, helper, src))
        {
            return true;
        }
        let mut cursor = n.walk();
        stack.extend(n.children(&mut cursor));
    }
    false
}

fn calls(value: Node, helper: &str, src: &[u8]) -> bool {
    let mut stack = vec![value];
    while let Some(n) = stack.pop() {
        if n.kind() == "call_expression"
            && n.child_by_field_name("function")
                .is_some_and(|f| &src[f.byte_range()] == helper.as_bytes())
        {
            return true;
        }
        let mut cursor = n.walk();
        stack.extend(n.children(&mut cursor));
    }
    false
}

/// Does the file hold a plain `#[test]` function? Those are the tests the rstest crate should replace.
fn has_plain_test(text: &str) -> bool {
    let mut parser = Parser::new();
    if parser
        .set_language(&Lang::Rust.grammar().expect("Rust has a grammar"))
        .is_err()
    {
        return false;
    }
    let Some(tree) = parser.parse(text, None) else {
        return false;
    };
    let mut containers = Vec::new();
    scan(tree.root_node(), text.as_bytes(), &mut containers);
    containers.iter().any(|(tests, _)| !tests.is_empty())
}

/// Checks one Rust source file.
pub fn check_source(path: &str, text: &str) -> Vec<Violation> {
    let mut parser = Parser::new();
    if parser
        .set_language(&Lang::Rust.grammar().expect("Rust has a grammar"))
        .is_err()
    {
        return Vec::new();
    }
    let Some(tree) = parser.parse(text, None) else {
        return Vec::new();
    };
    let src = text.as_bytes();
    let mut containers = Vec::new();
    scan(tree.root_node(), src, &mut containers);
    let mut found = Vec::new();
    for (tests, helpers) in &containers {
        found.extend(repeated_tests(path, tests, src));
        found.extend(helpers_to_fixtures(path, tests, helpers, src));
    }
    found
}

/// Tests whose bodies are the same but for constants.
fn repeated_tests(path: &str, tests: &[TestFn], src: &[u8]) -> Vec<Violation> {
    let mut groups: HashMap<u64, Vec<&TestFn>> = HashMap::new();
    let none: [std::ops::Range<usize>; 0] = [];
    for t in tests {
        let lines = t.body.end_position().row + 1 - t.body.start_position().row;
        let mut sink = Vec::new();
        let lim = Limits {
            min_lines: MIN_BODY_LINES,
            min_tokens: MIN_BODY_TOKENS,
            skip: &none,
        };
        let (hash, tokens) = walk(t.body, src, Normalize::Literals, &lim, &mut sink, 0);
        if lines >= MIN_BODY_LINES && tokens >= MIN_BODY_TOKENS {
            groups.entry(hash).or_default().push(t);
        }
    }
    groups
        .into_values()
        .filter(|g| g.len() >= 2)
        .map(|g| {
            let names: Vec<&str> = g.iter().map(|t| t.name.as_str()).collect();
            Violation {
                kind: "repeated_test".into(),
                path: path.to_string(),
                line: g.iter().map(|t| t.line).min().unwrap_or(1),
                message: format!(
                    "{} differ only in constants; make one #[rstest] with a #[case] each (#[case::name] to name them, #[values] when they cross several lists)",
                    names.join(", ")
                ),
            }
        })
        .collect()
}

/// Undecorated functions whose result two or more tests bind with `let`: setup that should be a `#[fixture]`.
fn helpers_to_fixtures(path: &str, tests: &[TestFn], helpers: &[TestFn], src: &[u8]) -> Vec<Violation> {
    helpers
        .iter()
        .filter_map(|h| {
            let users: Vec<&str> = tests
                .iter()
                .filter(|t| binds_result_of(t.body, &h.name, src))
                .map(|t| t.name.as_str())
                .collect();
            (users.len() >= 2).then(|| Violation {
                kind: "helper_should_be_fixture".into(),
                path: path.to_string(),
                line: h.line,
                message: format!(
                    "{}() sets up {}; make it a #[fixture] and take it as an argument of #[rstest] (#[once] to share one value, #[with(..)] to vary it)",
                    h.name,
                    users.join(", ")
                ),
            })
        })
        .collect()
}
