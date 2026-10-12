//! MDX: the `import` and `export` lines of `.mdx` files.

use std::path::Path;

use oxc_span::SourceType;

use super::{Extracted, Plugin};

pub(super) struct Mdx;

impl Plugin for Mdx {
    fn name(&self) -> &'static str {
        "mdx"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["mdx"]
    }

    /// Each block of lines that starts with `import` or `export` at the
    /// start of a line, up to a blank line. Prose and code fences are
    /// blanked.
    fn extract(&self, _file: &Path, src: &str) -> Option<Extracted> {
        let mut js = String::with_capacity(src.len());
        let (mut in_esm, mut in_fence) = (false, false);
        for line in src.split_inclusive('\n') {
            let text = line.trim_end();
            if text.trim_start().starts_with("```") || text.trim_start().starts_with("~~~") {
                in_fence = !in_fence;
                in_esm = false;
            } else if !in_fence && (text.starts_with("import ") || text.starts_with("export ")) {
                in_esm = true;
            } else if text.trim().is_empty() {
                in_esm = false;
            }
            if in_esm && !in_fence {
                js.push_str(line);
            } else {
                for c in line.chars() {
                    if c == '\n' {
                        js.push(c);
                    } else {
                        js.extend(std::iter::repeat_n(' ', c.len_utf8()));
                    }
                }
            }
        }
        Some(Extracted { js, source_type: SourceType::tsx(), imports: Vec::new() })
    }
}
