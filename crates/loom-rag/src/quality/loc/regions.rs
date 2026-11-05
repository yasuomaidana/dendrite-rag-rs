//! The parts of a source file that are not production code: comments, docstrings and test code.

use std::ops::Range;

use tree_sitter::{Node, Parser};

use crate::lang::Lang;

/// Why a region is not production code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    Comment,
    Test,
}

/// A byte range of the source and why it is not production code. Ranges come in source order and never
/// overlap: a test region is not searched for comments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marked {
    pub range: Range<usize>,
    pub kind: Region,
}

/// The non-production regions of one file.
#[derive(Debug, Clone, Default)]
pub struct Regions {
    pub marks: Vec<Marked>,
    /// The grammar could not parse some of the file; the regions found are still used.
    pub parse_errors: bool,
}

/// Finds the comments and test code of `src`. `None` when the language has no grammar.
pub fn find(lang: Lang, src: &str) -> Option<Regions> {
    let mut parser = Parser::new();
    parser.set_language(&lang.grammar()?).ok()?;
    let tree = parser.parse(src, None)?;
    let root = tree.root_node();
    let mut marks = Vec::new();
    if lang == Lang::Rust && has_inner_test_attribute(root, src.as_bytes()) {
        marks.push(Marked {
            range: 0..src.len(),
            kind: Region::Test,
        });
    } else {
        collect(lang, root, src.as_bytes(), &mut marks);
    }
    Some(Regions {
        marks,
        parse_errors: root.has_error(),
    })
}

fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    std::str::from_utf8(&src[node.byte_range()]).unwrap_or("")
}

/// Whitespace removed, so `# [ cfg ( test ) ]` and `#[cfg(test)]` compare equal.
fn compact(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

fn collect(lang: Lang, node: Node, src: &[u8], out: &mut Vec<Marked>) {
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    let mut i = 0;
    while i < children.len() {
        let child = children[i];
        if lang == Lang::Rust
            && child.kind() == "attribute_item"
            && let Some(end) = rust_test_item_end(&children, i, src)
        {
            out.push(Marked {
                range: child.start_byte()..children[end].end_byte(),
                kind: Region::Test,
            });
            i = end + 1;
            continue;
        }
        if is_comment(lang, child, node, src) {
            out.push(Marked {
                range: child.byte_range(),
                kind: Region::Comment,
            });
        } else if is_test_item(lang, child, src) {
            out.push(Marked {
                range: child.byte_range(),
                kind: Region::Test,
            });
        } else if child.child_count() > 0 {
            collect(lang, child, src, out);
        }
        i += 1;
    }
}

fn is_comment(lang: Lang, node: Node, parent: Node, src: &[u8]) -> bool {
    node.kind().ends_with("comment") || (lang == Lang::Python && is_docstring(node, parent, src))
}

/// A Python docstring: a lone string statement that opens a module, class or function body.
fn is_docstring(node: Node, parent: Node, src: &[u8]) -> bool {
    if node.kind() != "expression_statement" || node.named_child_count() != 1 {
        return false;
    }
    let is_string = node.named_child(0).is_some_and(|c| c.kind() == "string");
    let first = parent.named_child(0).is_some_and(|c| c.id() == node.id());
    is_string
        && first
        && matches!(parent.kind(), "module" | "block")
        && text(node, src).starts_with(['"', '\'', 'r', 'b', 'u'])
}

/// For a Rust attribute at `children[i]`, the index of the item it decorates when the run of attributes
/// starting there marks that item as test code (`#[test]`, `#[rstest]`, `#[cfg(test)]`, ...).
fn rust_test_item_end(children: &[Node], i: usize, src: &[u8]) -> Option<usize> {
    let mut j = i;
    let mut test = false;
    while let Some(n) = children.get(j) {
        match n.kind() {
            "attribute_item" => test |= rust_attribute_marks_test(text(*n, src)),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        j += 1;
    }
    (test && j < children.len()).then_some(j)
}

/// Whether a Rust attribute (`#[...]`) says its item is test code.
fn rust_attribute_marks_test(attribute: &str) -> bool {
    let a = compact(attribute);
    let Some(inner) = a.strip_prefix("#[").and_then(|s| s.strip_suffix(']')) else {
        return false;
    };
    let name = inner.split('(').next().unwrap_or(inner);
    if name == "cfg" {
        return !inner.contains("not(test)") && contains_word(inner, "test");
    }
    matches!(name, "test" | "rstest" | "test_case" | "wasm_bindgen_test") || name.ends_with("::test")
}

fn contains_word(haystack: &str, word: &str) -> bool {
    haystack
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|w| w == word)
}

fn has_inner_test_attribute(root: Node, src: &[u8]) -> bool {
    let mut cursor = root.walk();
    root.children(&mut cursor)
        .any(|n| n.kind() == "inner_attribute_item" && compact(text(n, src)) == "#![cfg(test)]")
}

fn is_test_item(lang: Lang, node: Node, src: &[u8]) -> bool {
    match lang {
        Lang::Python => python_test(node, src),
        Lang::Java => java_test(node, src),
        Lang::JavaScript | Lang::TypeScript | Lang::Tsx => js_test_call(node, src),
        _ => false,
    }
}

fn field_text<'a>(node: Node, field: &str, src: &'a [u8]) -> &'a str {
    node.child_by_field_name(field).map_or("", |n| text(n, src))
}

fn python_test(node: Node, src: &[u8]) -> bool {
    match node.kind() {
        "function_definition" => field_text(node, "name", src).starts_with("test_"),
        "class_definition" => field_text(node, "name", src).starts_with("Test"),
        "decorated_definition" => node
            .child_by_field_name("definition")
            .is_some_and(|d| python_test(d, src)),
        "if_statement" => {
            let cond = compact(field_text(node, "condition", src));
            cond == "__name__==\"__main__\"" || cond == "__name__=='__main__'"
        }
        _ => false,
    }
}

fn java_test(node: Node, src: &[u8]) -> bool {
    match node.kind() {
        "class_declaration" => {
            let name = field_text(node, "name", src);
            name.ends_with("Test") || name.ends_with("Tests")
        }
        "method_declaration" => {
            let mut cursor = node.walk();
            let modifiers = node.children(&mut cursor).find(|c| c.kind() == "modifiers");
            modifiers.is_some_and(|m| {
                let mut c = m.walk();
                m.children(&mut c).any(|a| {
                    matches!(a.kind(), "marker_annotation" | "annotation")
                        && matches!(
                            field_text(a, "name", src),
                            "Test" | "ParameterizedTest" | "RepeatedTest" | "TestFactory"
                        )
                })
            })
        }
        _ => false,
    }
}

/// A top-level `describe(...)`, `it(...)` or `test(...)` call of a JavaScript or TypeScript test file.
fn js_test_call(node: Node, src: &[u8]) -> bool {
    if node.kind() != "expression_statement" {
        return false;
    }
    let Some(call) = node.named_child(0).filter(|c| c.kind() == "call_expression") else {
        return false;
    };
    let Some(func) = call.child_by_field_name("function") else {
        return false;
    };
    let head = if func.kind() == "member_expression" {
        field_text(func, "object", src)
    } else {
        text(func, src)
    };
    matches!(
        head,
        "describe" | "it" | "test" | "beforeEach" | "afterEach" | "beforeAll" | "afterAll"
    )
}
