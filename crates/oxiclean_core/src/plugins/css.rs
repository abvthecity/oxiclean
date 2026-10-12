//! Stylesheets: `@import`, Sass's `@use` and `@forward`, and Tailwind's
//! `@plugin`, `@config` and `@reference` in `.css`, `.scss`, `.sass` and
//! `.less` files, including CSS modules.

use std::path::Path;

use oxc_span::SourceType;

use super::{Extracted, Plugin};

/// At-rules whose argument is a stylesheet, module or plugin to load.
const AT_RULES: &[&str] = &["@import", "@use", "@forward", "@plugin", "@config", "@reference"];

/// Extensions, in Sass's order, tried for an import without one.
const STYLE_EXTENSIONS: &[&str] = &["scss", "sass", "css", "less"];

pub(super) struct Css;

impl Plugin for Css {
    fn name(&self) -> &'static str {
        "css"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["css", "scss", "sass", "less", "pcss", "postcss"]
    }

    /// No JS/TS; each at-rule that loads something is an import. A local
    /// file is imported by its path, Sass partials (`_x.scss`) and index
    /// files included; anything else, or a `~` request, is a package.
    fn extract(&self, file: &Path, src: &str) -> Option<Extracted> {
        let dir = file.parent().unwrap_or(Path::new("."));
        let text = without_comments(src);
        let mut imports = Vec::new();
        for rule in AT_RULES {
            for (offset, _) in text.match_indices(rule) {
                let rest = &text[offset + rule.len()..];
                // `@import` and not `@imports`, at the start of a statement.
                let starts = text[..offset].trim_end().is_empty()
                    || text[..offset].trim_end().ends_with([';', '{', '}']);
                if !starts || !rest.starts_with(char::is_whitespace) {
                    continue;
                }
                for value in arguments(rest) {
                    if let Some(request) = request(dir, value) {
                        imports.push((request, offset));
                    }
                }
            }
        }
        imports.sort_by_key(|(_, offset)| *offset);
        // No JS, but the same lines, so each import's offset finds its line.
        let js = src.bytes().map(|b| if b == b'\n' { '\n' } else { ' ' }).collect();
        Some(Extracted { js, source_type: SourceType::mjs(), imports })
    }
}

/// The quoted or `url()` arguments of an at-rule, up to its `;` or `{`:
/// `"a", "b"` in `@import "a", "b";`.
fn arguments(rest: &str) -> Vec<&str> {
    let end = rest.find([';', '{', '\n']).unwrap_or(rest.len());
    let mut out = Vec::new();
    // Less's options, as in `@import (reference) "x";`.
    let mut s = rest[..end].trim_start();
    if let Some(options) = s.strip_prefix('(') {
        s = options.find(')').map_or("", |close| &options[close + 1..]);
    }
    loop {
        s = s.trim_start_matches(|c: char| c.is_whitespace() || c == ',');
        let (value, after) = if let Some(inner) = s.strip_prefix("url(") {
            let Some(close) = inner.find(')') else { break };
            (inner[..close].trim().trim_matches(['"', '\'']), &inner[close + 1..])
        } else if let Some(q) = s.chars().next().filter(|c| matches!(c, '"' | '\'')) {
            let Some(close) = s[1..].find(q) else { break };
            (&s[1..=close], &s[close + 2..])
        } else {
            break;
        };
        out.push(value);
        s = after;
    }
    out
}

/// The request to resolve for an at-rule's `value` in a stylesheet in
/// `dir`, or `None` for URLs and Sass's built-in `sass:` modules.
fn request(dir: &Path, value: &str) -> Option<String> {
    if value.is_empty() || value.contains("://") || value.starts_with("//") || value.contains(':') {
        return None;
    }
    if let Some(package) = value.strip_prefix('~') {
        return Some(package.to_string());
    }
    Some(local_file(dir, value).unwrap_or_else(|| value.to_string()))
}

/// `value` as a file next to the stylesheet, as Sass and CSS look first:
/// the file itself, with a style extension, as a `_` partial, or as an
/// `index` or `_index` file in a directory.
fn local_file(dir: &Path, value: &str) -> Option<String> {
    let path = Path::new(value.trim_start_matches('/'));
    let base = dir.join(path);
    let name = path.file_name()?.to_str()?;
    let parent = base.parent()?;
    let mut candidates = vec![base.clone()];
    for ext in STYLE_EXTENSIONS {
        candidates.push(parent.join(format!("{name}.{ext}")));
        candidates.push(parent.join(format!("_{name}.{ext}")));
        candidates.push(base.join(format!("index.{ext}")));
        candidates.push(base.join(format!("_index.{ext}")));
    }
    let found = candidates.into_iter().find(|c| c.is_file())?;
    let rel = found.strip_prefix(dir).ok()?;
    Some(format!("./{}", rel.to_string_lossy().replace('\\', "/")))
}

/// `src` with `/* */` and `//` comments blanked, strings kept.
fn without_comments(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = src.as_bytes().to_vec();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'/' && bytes.get(i + 1) == Some(&b'*') => {
                let end = src[i + 2..].find("*/").map_or(bytes.len(), |e| i + 2 + e + 2);
                blank(&mut out, i, end);
                i = end;
                continue;
            }
            // `//` starts a comment in Sass and Less, but not inside `url()`.
            None if b == b'/'
                && bytes.get(i + 1) == Some(&b'/')
                && (i == 0 || bytes[i - 1] != b':') =>
            {
                let end = src[i..].find('\n').map_or(bytes.len(), |e| i + e);
                blank(&mut out, i, end);
                i = end;
                continue;
            }
            None => {}
        }
        i += 1;
    }
    String::from_utf8(out).unwrap_or_default()
}

/// Blanks `out[from..to]` to spaces, keeping newlines and UTF-8 intact
/// (multi-byte characters become as many spaces as they have bytes).
fn blank(out: &mut [u8], from: usize, to: usize) {
    for b in &mut out[from..to] {
        if *b != b'\n' {
            *b = b' ';
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn stylesheet_imports() {
        let t = TempDir::new().unwrap();
        let dir = t.path();
        fs::create_dir_all(dir.join("theme")).unwrap();
        fs::write(dir.join("_variables.scss"), "").unwrap();
        fs::write(dir.join("theme/_index.scss"), "").unwrap();
        fs::write(dir.join("reset.css"), "").unwrap();
        let src = r#"@use "sass:math";
@use 'variables' as v;
@forward "theme";
@import url("reset.css") screen;
@import "~bootstrap/scss/bootstrap", "normalize.css";
/* @import "commented-out"; é */
// @import "line-comment";
@import url(https://fonts.googleapis.com/css?family=Inter);
@plugin "@tailwindcss/typography";
.a { background: url(//cdn.example.com/x.png); }
@imports "not-a-rule";
@import (reference) "~less-mixins";
"#;
        let file = dir.join("app.scss");
        let extracted = Css.extract(&file, src).unwrap();
        assert_eq!(extracted.js.len(), src.len());
        assert_eq!(extracted.js.lines().count(), src.lines().count());
        let line = |offset: usize| src[..offset].matches('\n').count() + 1;
        let got: Vec<(String, usize)> =
            extracted.imports.into_iter().map(|(r, offset)| (r, line(offset))).collect();
        assert_eq!(
            got,
            [
                ("./_variables.scss", 2),
                ("./theme/_index.scss", 3),
                ("./reset.css", 4),
                ("bootstrap/scss/bootstrap", 5),
                ("normalize.css", 5),
                ("@tailwindcss/typography", 9),
                ("less-mixins", 12),
            ]
            .map(|(r, l)| (r.to_string(), l))
        );
    }
}
