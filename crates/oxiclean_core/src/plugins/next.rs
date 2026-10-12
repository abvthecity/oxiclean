//! Next.js: pages, App Router files, middleware and instrumentation are
//! entries.

use std::path::Path;

use super::{Plugin, config};

const EXTENSIONS: &[&str] = &["js", "jsx", "ts", "tsx", "mjs", "mts"];

/// App Router files Next.js loads by name.
const APP_FILES: &[&str] = &[
    "page",
    "layout",
    "template",
    "loading",
    "error",
    "global-error",
    "not-found",
    "global-not-found",
    "forbidden",
    "unauthorized",
    "default",
    "route",
    "icon",
    "apple-icon",
    "opengraph-image",
    "twitter-image",
    "manifest",
    "sitemap",
    "robots",
];

/// Files Next.js loads from the project root or `src`.
const ROOT_FILES: &[&str] =
    &["middleware", "proxy", "instrumentation", "instrumentation-client", "mdx-components"];

pub(super) struct Next;

impl Plugin for Next {
    fn name(&self) -> &'static str {
        "next"
    }

    fn enablers(&self) -> &'static [&'static str] {
        &["next"]
    }

    /// In a package that lists `next`: every file under `pages`, the
    /// special files under `app` (`page`, `layout`, `route` and the like),
    /// and `middleware`, `instrumentation` and `mdx-components`, each also
    /// under `src`.
    fn is_entry(&self, file: &Path) -> bool {
        let (Some(stem), Some(ext)) =
            (file.file_stem().and_then(|s| s.to_str()), file.extension().and_then(|e| e.to_str()))
        else {
            return false;
        };
        if !EXTENSIONS.contains(&ext) {
            return false;
        }
        let uses_next = |dir: &Path| config::lists_package(dir, "next");
        if ROOT_FILES.contains(&stem)
            && let Some(parent) = file.parent()
        {
            let owner = if parent.file_name().is_some_and(|n| n == "src") {
                parent.parent()
            } else {
                Some(parent)
            };
            if owner.is_some_and(|o| o.join("package.json").is_file() && uses_next(o)) {
                return true;
            }
        }
        file.ancestors().skip(1).any(|dir| {
            let routes = match dir.file_name().and_then(|n| n.to_str()) {
                Some("pages") => true,
                Some("app") => APP_FILES.contains(&stem),
                _ => return false,
            };
            routes && config::routes_owner(dir).is_some_and(uses_next)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn next_entries() {
        let t = TempDir::new().unwrap();
        let dir = t.path();
        fs::write(dir.join("package.json"), r#"{ "dependencies": { "next": "15" } }"#).unwrap();
        let entry = |p: &str| Next.is_entry(&dir.join(p));
        assert!(entry("app/page.tsx"));
        assert!(entry("src/app/(shop)/cart/layout.tsx"));
        assert!(entry("app/api/users/route.ts"));
        assert!(entry("pages/index.tsx"));
        assert!(entry("src/pages/api/hello.ts"));
        assert!(entry("middleware.ts"));
        assert!(entry("src/instrumentation.ts"));
        assert!(!entry("app/components/button.tsx"));
        assert!(!entry("app/page.css"));
        assert!(!entry("lib/page.ts"));

        let other = TempDir::new().unwrap();
        fs::write(other.path().join("package.json"), "{}").unwrap();
        assert!(!Next.is_entry(&other.path().join("app/page.tsx")));
    }
}
