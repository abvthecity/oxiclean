//! HTML pages: their `<script>` blocks and `<script src>`. A page that
//! loads the project's scripts is an entry, as knip's Vite plugin takes
//! `index.html`.

use std::{fs, path::Path};

use oxc_span::SourceType;

use super::{
    Extracted, Plugin,
    markup::{attribute, is_js_script, keep_only, script_bodies, script_tags},
};

pub(super) struct Html;

impl Plugin for Html {
    fn name(&self) -> &'static str {
        "html"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["html"]
    }

    /// Inline scripts are parsed; each `<script src>` is an import. A `src`
    /// starting with `/` is relative to the page's package directory, the
    /// root a dev server such as Vite serves; URLs are left out.
    fn extract(&self, file: &Path, src: &str) -> Option<Extracted> {
        let mut keep = vec![false; src.len()];
        for (from, to) in script_bodies(src) {
            keep[from..to].iter_mut().for_each(|k| *k = true);
        }
        let dir = file.parent().unwrap_or(Path::new("."));
        let root = dir.ancestors().find(|d| d.join("package.json").is_file()).unwrap_or(dir);
        let mut imports = Vec::new();
        for (offset, tag) in script_tags(src) {
            let Some(value) = attribute(tag, "src").filter(|v| is_local(v)) else { continue };
            let request = match value.strip_prefix('/') {
                Some(rest) => match dir.strip_prefix(root).map_or(0, |d| d.components().count()) {
                    0 => format!("./{rest}"),
                    depth => format!("{}{rest}", "../".repeat(depth)),
                },
                None if value.starts_with('.') => value.to_string(),
                None => format!("./{value}"),
            };
            imports.push((request, offset));
        }
        Some(Extracted { js: keep_only(src, &keep), source_type: SourceType::ts(), imports })
    }

    /// A page with a `<script>` that has a local `src`, or an inline module
    /// script.
    fn is_entry(&self, file: &Path) -> bool {
        if file.extension().and_then(|e| e.to_str()) != Some("html") {
            return false;
        }
        let Ok(src) = fs::read_to_string(file) else { return false };
        script_tags(&src).any(|(_, tag)| {
            let local_src = attribute(tag, "src").is_some_and(is_local);
            is_js_script(tag) && (local_src || attribute(tag, "type") == Some("module"))
        })
    }
}

/// A `src` in the project rather than a URL.
fn is_local(src: &str) -> bool {
    !src.is_empty() && !src.contains("://") && !src.starts_with("//")
}
