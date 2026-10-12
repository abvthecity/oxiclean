//! Vite's `import.meta.glob`, expanded to the files it matches.

use std::path::{Component, Path, PathBuf};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use tracing::warn;

/// How many files one glob walks at most, so that `/**/*` over a large
/// tree stays cheap.
const MAX_FILES: usize = 20_000;

/// The files `patterns` match, as requests relative to `file`, sorted.
/// A pattern is relative to the file, or with a leading `/` to its package
/// directory, the root Vite serves; `!` excludes. Aliases such as `@/x/*`
/// aren't expanded.
pub(crate) fn expand(file: &Path, patterns: &[String]) -> Vec<String> {
    let Some(dir) = file.parent() else { return Vec::new() };
    let root = dir.ancestors().find(|d| d.join("package.json").is_file()).unwrap_or(dir);
    let mut include = GlobSetBuilder::new();
    let mut exclude = GlobSetBuilder::new();
    let mut bases = Vec::new();
    for pattern in patterns {
        let (negated, pattern) = match pattern.strip_prefix('!') {
            Some(p) => (true, p),
            None => (false, pattern.as_str()),
        };
        let absolute = if let Some(rest) = pattern.strip_prefix('/') {
            root.join(rest)
        } else if pattern.starts_with("./") || pattern.starts_with("../") {
            dir.join(pattern)
        } else {
            continue;
        };
        let absolute = path_clean::clean(&absolute);
        let text = slashes(&absolute);
        let Ok(glob) = GlobBuilder::new(&text).literal_separator(true).build() else {
            warn!("Invalid glob '{pattern}' in {}", file.display());
            continue;
        };
        if negated {
            exclude.add(glob);
        } else {
            include.add(glob);
            bases.push(literal_base(&absolute));
        }
    }
    let (Ok(include), Ok(exclude)) = (include.build(), exclude.build()) else { return Vec::new() };
    let mut out: Vec<String> = Vec::new();
    for base in bases {
        out.extend(walk(&base, &include, &exclude, file).iter().map(|f| relative(dir, f)));
    }
    out.sort();
    out.dedup();
    out
}

/// The files under `base`, but `file`, that `include` and not `exclude`
/// matches.
fn walk(base: &Path, include: &GlobSet, exclude: &GlobSet, file: &Path) -> Vec<PathBuf> {
    let walker = WalkBuilder::new(base)
        .standard_filters(false)
        .filter_entry(|e| e.file_name() != "node_modules" && e.file_name() != ".git")
        .build();
    walker
        .filter_map(Result::ok)
        .take(MAX_FILES)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(ignore::DirEntry::into_path)
        .filter(|p| p != file)
        .filter(|p| {
            let text = slashes(p);
            include.is_match(&text) && !exclude.is_match(&text)
        })
        .collect()
}

/// The directories of `pattern` before its first glob character.
fn literal_base(pattern: &Path) -> PathBuf {
    let mut base = PathBuf::new();
    for c in pattern.components() {
        if c.as_os_str().to_string_lossy().contains(['*', '?', '[', '{']) {
            break;
        }
        base.push(c);
    }
    base
}

/// `to` as a request from directory `from`: `./a/b.ts` or `../c.ts`.
fn relative(from: &Path, to: &Path) -> String {
    let from: Vec<Component> = from.components().collect();
    let to: Vec<Component> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let ups = from.len() - common;
    let rest: Vec<String> =
        to[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    let up = if ups == 0 { "./".to_string() } else { "../".repeat(ups) };
    format!("{up}{}", rest.join("/"))
}

fn slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn expands_relative_root_and_negated_globs() {
        let t = TempDir::new().unwrap();
        let root = t.path();
        fs::write(root.join("package.json"), "{}").unwrap();
        for f in [
            "src/main.ts",
            "src/pages/a.ts",
            "src/pages/b.vue",
            "src/pages/nested/c.ts",
            "src/pages/skip.ts",
            "lib/x.ts",
        ] {
            let p = root.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "").unwrap();
        }
        let file = root.join("src/main.ts");
        let p = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert_eq!(expand(&file, &p(&["./pages/*.ts"])), ["./pages/a.ts", "./pages/skip.ts"]);
        assert_eq!(
            expand(&file, &p(&["./pages/**/*.{ts,vue}", "!./pages/skip.ts"])),
            ["./pages/a.ts", "./pages/b.vue", "./pages/nested/c.ts"]
        );
        assert_eq!(expand(&file, &p(&["/lib/*.ts", "@/x/*.ts"])), ["../lib/x.ts"]);
        assert_eq!(expand(&file, &p(&["./*.ts"])), Vec::<String>::new());
    }
}
