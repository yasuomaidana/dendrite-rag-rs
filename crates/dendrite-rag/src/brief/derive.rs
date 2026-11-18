//! Search patterns taken from a question: the quoted strings and the identifiers in it. Plain
//! words are left to the retrieval map; an identifier such as `LocateBinary`, `fold_signatures`,
//! `tokens.count`, `DEN-903` or `DENDRITE_HOME` is exactly what a grep finds.

/// Most patterns derived from one question.
const MAX_PATTERNS: usize = 6;
const MIN_LEN: usize = 3;
const MAX_LEN: usize = 80;

/// The literal strings worth grepping for in `question`, in order of appearance, escaped for a
/// regular expression.
pub(crate) fn patterns(question: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut rest = String::new();
    let mut quote: Option<(char, String)> = None;
    for c in question.chars() {
        match (&mut quote, c) {
            (Some((q, buf)), c) if c == *q => {
                push(&mut found, buf);
                quote = None;
            }
            (Some((_, buf)), c) => buf.push(c),
            (None, '"' | '`') => quote = Some((c, String::new())),
            (None, c) => rest.push(c),
        }
    }
    for token in rest.split(|c: char| !is_token_char(c)) {
        let token = token.trim_matches(|c| matches!(c, '.' | ':' | '-'));
        if is_identifier(token) {
            push(&mut found, token);
        }
    }
    found.truncate(MAX_PATTERNS);
    found.iter().map(|s| escape(s)).collect()
}

fn push(found: &mut Vec<String>, s: &str) {
    let s = s.trim();
    let len = s.chars().count();
    if (MIN_LEN..=MAX_LEN).contains(&len) && !found.iter().any(|f| f == s) {
        found.push(s.to_string());
    }
}

fn is_token_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '.' | ':' | '-')
}

/// Whether `token` looks like a name from code: snake_case, camelCase or CamelCase, a path of
/// names (`a.b`, `a::b`), an upper-case constant, or a ticket id (`DEN-903`).
fn is_identifier(token: &str) -> bool {
    if token.chars().count() < MIN_LEN || token.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return false;
    }
    let letters = |f: fn(&char) -> bool| token.chars().filter(f).count();
    let upper = letters(|c| c.is_uppercase());
    let lower = letters(|c| c.is_lowercase());
    let first_upper = token.chars().next().is_some_and(char::is_uppercase);
    token.contains('_')
        || token.contains("::")
        || (token.contains('.') && lower > 0 && token.chars().count() > MIN_LEN)
        || (upper > 0 && lower > 0 && (upper > 1 || !first_upper))
        || ticket_id(token)
}

/// `DEN-903`: capital letters, a dash, digits.
fn ticket_id(token: &str) -> bool {
    token.split_once('-').is_some_and(|(prefix, number)| {
        !prefix.is_empty()
            && prefix.chars().all(|c| c.is_ascii_uppercase())
            && !number.is_empty()
            && number.chars().all(|c| c.is_ascii_digit())
    })
}

/// `s` as a regular expression matching it literally.
pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// One regular expression matching any of `patterns`.
pub(crate) fn alternation(patterns: &[String]) -> String {
    format!("(?:{})", patterns.join("|"))
}
