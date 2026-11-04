//! Files that hold code of another language, such as the scripts of a Svelte component.
//!
//! The regions come from `dendrite-core` (the same definition its complexity and symbol operations use). Each
//! region is chunked as the language it names: everything outside it is blanked with spaces, newlines kept, so
//! the code chunker sees only that region while byte offsets and line numbers stay those of the file. What is
//! outside every region (markup, style) is chunked as text.

use super::*;

/// `src` with the bytes inside `ranges` kept (or, if `keep_inside` is false, the bytes outside them) and the
/// rest turned into spaces. Newlines stay, so every offset and line number is unchanged.
fn blank(src: &str, ranges: &[Range<usize>], keep_inside: bool) -> String {
    let mut bytes = src.as_bytes().to_vec();
    for (i, b) in bytes.iter_mut().enumerate() {
        let inside = ranges.iter().any(|r| r.contains(&i));
        if inside != keep_inside && *b != b'\n' {
            *b = b' ';
        }
    }
    String::from_utf8(bytes).expect("blanking bytes with spaces keeps UTF-8 valid")
}

pub(super) fn chunk_embedded(
    path: &str,
    lang: Lang,
    src: &str,
    lines: &LineIndex,
    opts: ChunkOptions,
) -> (Vec<Chunk>, Vec<String>) {
    let regions = lang
        .core()
        .map(|core| dendrite_core::embedded::regions(core, src))
        .unwrap_or_default();
    let ranges: Vec<Range<usize>> = regions.iter().map(|r| r.start..r.end).collect();
    let mut chunks = Vec::new();
    let mut imports: Vec<String> = Vec::new();
    for (region, range) in regions.iter().zip(&ranges) {
        let Some(inner) = Lang::from_core(region.lang) else {
            continue;
        };
        let only_region = blank(src, std::slice::from_ref(range), true);
        if let Some((found, specs)) = chunk_code(path, inner, &only_region, lines, opts) {
            chunks.extend(found.into_iter().map(|c| relang(c, lang)));
            for spec in specs {
                if !imports.contains(&spec) {
                    imports.push(spec);
                }
            }
        }
    }
    let markup = blank(src, &ranges, false);
    chunks.extend(
        pack_text(&markup, 0..markup.len(), opts.max_chars)
            .into_iter()
            .filter_map(|r| build(path, lang, &markup, lines, r, Vec::new(), &[], &[])),
    );
    chunks.sort_by_key(|c| c.start_byte);
    (chunks, imports)
}
