//! Reading JS/TS out of markup: `<script>` tags and their attributes.

/// `src` with every byte outside the ranges `keep` marks blanked to a
/// space, newlines kept, so that offsets and lines stay those of `src`.
pub(super) fn keep_only(src: &str, keep: &[bool]) -> String {
    let mut out = String::with_capacity(src.len());
    for (i, c) in src.char_indices() {
        if keep[i] || c == '\n' {
            out.push(c);
        } else {
            // As many spaces as the character had bytes, to keep offsets.
            out.extend(std::iter::repeat_n(' ', c.len_utf8()));
        }
    }
    out
}

/// The bodies of the JS/TS `<script>` blocks in `src`, as byte ranges.
pub(super) fn script_bodies(src: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(open) = src[at..].find("<script") {
        let open = at + open;
        let Some(body) = src[open..].find('>').map(|i| open + i + 1) else { break };
        let Some(close) = src[body..].find("</script>").map(|i| body + i) else { break };
        if is_js_script(&src[open..body]) {
            out.push((body, close));
        }
        at = close + "</script>".len();
    }
    out
}

/// Each `<script ...>` opening tag in `src` with its byte offset.
pub(super) fn script_tags(src: &str) -> impl Iterator<Item = (usize, &str)> {
    src.match_indices("<script").filter_map(|(open, _)| {
        let end = src[open..].find('>')?;
        Some((open, &src[open..open + end + 1]))
    })
}

/// Whether a `<script>` tag holds JS/TS: no `type`, or a JavaScript or
/// module one, rather than JSON, a template or an import map.
pub(super) fn is_js_script(tag: &str) -> bool {
    attribute(tag, "type").is_none_or(|t| {
        matches!(
            t.to_ascii_lowercase().as_str(),
            "" | "module" | "text/javascript" | "application/javascript" | "text/typescript" | "ts"
        )
    })
}

/// The value of attribute `name` in an HTML opening tag, quoted or not.
pub(super) fn attribute<'t>(tag: &'t str, name: &str) -> Option<&'t str> {
    let bytes = tag.as_bytes();
    let mut from = 0;
    while let Some(i) = tag[from..].find(name) {
        let at = from + i;
        from = at + name.len();
        let before_ok = at > 0 && bytes[at - 1].is_ascii_whitespace();
        let rest = tag[from..].trim_start();
        let (true, Some(value)) = (before_ok, rest.strip_prefix('=')) else { continue };
        let value = value.trim_start();
        return Some(match value.chars().next() {
            Some(q @ ('"' | '\'')) => value[1..].split(q).next().unwrap_or_default(),
            _ => value.split(|c: char| c.is_whitespace() || c == '>' || c == '/').next()?,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes() {
        let tag = r#"<script type="module" data-src="x" src='/a.ts' defer>"#;
        assert_eq!(attribute(tag, "type"), Some("module"));
        assert_eq!(attribute(tag, "src"), Some("/a.ts"));
        assert_eq!(attribute("<script src=b.js>", "src"), Some("b.js"));
        assert_eq!(attribute("<script>", "src"), None);
        assert!(!is_js_script(r#"<script type="importmap">"#));
        assert!(is_js_script(r#"<script lang="ts">"#));
    }
}
