//! Single-file components: the `<script>` blocks of `.vue`, `.svelte` and
//! `.astro` files, and Astro's `---` frontmatter.

use std::path::Path;

use oxc_span::SourceType;

use super::{
    Extracted, Plugin,
    markup::{keep_only, script_bodies},
};

/// The JS/TS of a component: its `<script>` blocks and, when `frontmatter`
/// is set, the `---` block at its top.
fn component_scripts(src: &str, frontmatter: bool) -> Extracted {
    let mut keep = vec![false; src.len()];
    let mut mark = |from: usize, to: usize| keep[from..to].iter_mut().for_each(|k| *k = true);
    if frontmatter
        && let Some(rest) = src.strip_prefix("---")
        && let Some(end) = rest.find("\n---")
    {
        mark(3, 3 + end);
    }
    for (from, to) in script_bodies(src) {
        mark(from, to);
    }
    Extracted { js: keep_only(src, &keep), source_type: SourceType::ts(), imports: Vec::new() }
}

/// Vue single-file components.
pub(super) struct Vue;

impl Plugin for Vue {
    fn name(&self) -> &'static str {
        "vue"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["vue"]
    }
    fn extract(&self, _file: &Path, src: &str) -> Option<Extracted> {
        Some(component_scripts(src, false))
    }
}

/// Svelte components.
pub(super) struct Svelte;

impl Plugin for Svelte {
    fn name(&self) -> &'static str {
        "svelte"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["svelte"]
    }
    fn extract(&self, _file: &Path, src: &str) -> Option<Extracted> {
        Some(component_scripts(src, false))
    }
}

/// Astro components, with their frontmatter.
pub(super) struct Astro;

impl Plugin for Astro {
    fn name(&self) -> &'static str {
        "astro"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["astro"]
    }
    fn extract(&self, _file: &Path, src: &str) -> Option<Extracted> {
        Some(component_scripts(src, true))
    }
}
