//! Constants for file extensions and resolution strategies.
//!
//! This module centralizes all file extension handling to ensure consistency
//! across parsing, resolution, and collection of JavaScript/TypeScript files.
//!
//! ## Supported Extensions
//!
//! - **TypeScript**: `.ts`, `.tsx`, `.mts` (ES module), `.cts` (CommonJS)
//! - **JavaScript**: `.js`, `.jsx`, `.mjs` (ES module), `.cjs` (CommonJS)
//!
//! ## Module System Extensions
//!
//! - `.mts` and `.mjs`: ES Module files (use `import`/`export`)
//! - `.cts` and `.cjs`: CommonJS files (use `require`/`module.exports`)

/// File extensions for JavaScript/TypeScript files that should be analyzed
pub const JS_TS_EXTENSIONS: &[&str] = &[
    "ts",  // TypeScript
    "tsx", // TypeScript with JSX
    "mts", // TypeScript module
    "cts", // TypeScript CommonJS
    "js",  // JavaScript
    "jsx", // JavaScript with JSX
    "mjs", // JavaScript module
    "cjs", // JavaScript CommonJS
];

/// Extensions to try when resolving module imports (in priority order)
pub const RESOLVE_EXTENSIONS: &[&str] = &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

/// Suffixes of TypeScript declaration files, tried after every runtime
/// file when resolving an import.
pub const DECLARATION_SUFFIXES: &[&str] = &[".d.ts", ".d.mts", ".d.cts"];

/// Whether `path` is a TypeScript declaration file such as `index.d.ts`.
#[must_use]
pub fn is_declaration(path: &std::path::Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| DECLARATION_SUFFIXES.iter().any(|s| n.ends_with(s)))
}

/// The declaration file next to a JavaScript file that describes it:
/// `a.d.ts` for `a.js` or `a.jsx`, `a.d.mts` for `a.mjs`, `a.d.cts` for
/// `a.cjs`. `None` for other files, or when it doesn't exist.
#[must_use]
pub fn declaration_for(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let suffix = match path.extension()?.to_str()? {
        "js" | "jsx" => "d.ts",
        "mjs" => "d.mts",
        "cjs" => "d.cts",
        _ => return None,
    };
    let candidate = path.with_extension(suffix);
    // Canonical, like the resolver's paths, so it is one module however it
    // is reached.
    candidate.is_file().then(|| candidate.canonicalize().unwrap_or(candidate))
}

/// Index file names to try when resolving directory imports
pub const INDEX_FILES: &[&str] = &[
    "index.ts",
    "index.tsx",
    "index.mts",
    "index.cts",
    "index.js",
    "index.jsx",
    "index.mjs",
    "index.cjs",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_js_ts_extensions_includes_all_variants() {
        // Ensure all common JavaScript/TypeScript extensions are supported
        assert!(JS_TS_EXTENSIONS.contains(&"ts"));
        assert!(JS_TS_EXTENSIONS.contains(&"tsx"));
        assert!(JS_TS_EXTENSIONS.contains(&"mts"));
        assert!(JS_TS_EXTENSIONS.contains(&"cts"));
        assert!(JS_TS_EXTENSIONS.contains(&"js"));
        assert!(JS_TS_EXTENSIONS.contains(&"jsx"));
        assert!(JS_TS_EXTENSIONS.contains(&"mjs"));
        assert!(JS_TS_EXTENSIONS.contains(&"cjs"));
        assert_eq!(JS_TS_EXTENSIONS.len(), 8);
    }

    #[test]
    fn test_resolve_extensions_matches_js_ts_extensions() {
        // RESOLVE_EXTENSIONS should contain the same extensions as JS_TS_EXTENSIONS
        assert_eq!(RESOLVE_EXTENSIONS.len(), JS_TS_EXTENSIONS.len());
        for ext in RESOLVE_EXTENSIONS {
            assert!(
                JS_TS_EXTENSIONS.contains(ext),
                "RESOLVE_EXTENSIONS contains '{}' which is not in JS_TS_EXTENSIONS",
                ext
            );
        }
    }

    #[test]
    fn test_index_files_uses_all_extensions() {
        // INDEX_FILES should have an index file for each extension
        assert_eq!(INDEX_FILES.len(), JS_TS_EXTENSIONS.len());
        for ext in JS_TS_EXTENSIONS {
            let expected = format!("index.{}", ext);
            assert!(INDEX_FILES.contains(&expected.as_str()), "INDEX_FILES missing '{}'", expected);
        }
    }

    #[test]
    fn test_typescript_module_extensions_included() {
        // Specifically verify mts and cts are included (the additions requested)
        assert!(JS_TS_EXTENSIONS.contains(&"mts"));
        assert!(JS_TS_EXTENSIONS.contains(&"cts"));
        assert!(RESOLVE_EXTENSIONS.contains(&"mts"));
        assert!(RESOLVE_EXTENSIONS.contains(&"cts"));
        assert!(INDEX_FILES.contains(&"index.mts"));
        assert!(INDEX_FILES.contains(&"index.cts"));
    }
}
