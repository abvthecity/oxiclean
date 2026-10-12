//! The files a package offers to others, which the checks of exports start
//! from when the config sets no `entries`. They are what knip uses with its
//! plugins off: the files package.json points to or runs in a script, and
//! `index`, `main` and `cli` files at the package root or in `src`.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tracing::trace;

use crate::{
    Project,
    constants::RESOLVE_EXTENSIONS,
    resolver::{package_target_files, strip_compiled_extension},
};

/// package.json fields that name one file each.
const FILE_FIELDS: &[&str] = &["main", "module", "source", "browser", "types", "typings"];

/// File names, without extension, that are entries in every package.
const ENTRY_NAMES: &[&str] = &["index", "main", "cli"];

/// The entry points of the root package and of each workspace package,
/// sorted and without duplicates:
///
/// - the files package.json's `main`, `module`, `source`, `browser`,
///   `types`, `typings`, `bin` and `exports` (every condition) point to,
///   with unbuilt build output mapped to `src/` as imports are;
/// - the JS/TS files its `scripts` name, such as `node scripts/build.js`;
/// - `index`, `main` and `cli` files in the package directory or its `src`.
#[must_use]
pub(crate) fn package_entry_points(project: &Project) -> Vec<PathBuf> {
    let mut dirs: Vec<&Path> = vec![project.root.as_path()];
    dirs.extend(project.workspace_packages().into_iter().map(|(_, dir)| dir));
    let mut files: Vec<PathBuf> =
        dirs.into_iter().flat_map(|dir| entries_of(project, dir)).collect();
    files.sort();
    files.dedup();
    files
}

fn entries_of(project: &Project, dir: &Path) -> Vec<PathBuf> {
    let mut targets = Vec::new();
    let pkg = project.package_json(dir);
    if let Some(pkg) = &pkg {
        for field in FILE_FIELDS {
            if let Some(Value::String(s)) = pkg.get(*field) {
                targets.push(s.clone());
            }
        }
        match pkg.get("bin") {
            Some(Value::String(s)) => targets.push(s.clone()),
            Some(Value::Object(bins)) => {
                targets.extend(bins.values().filter_map(Value::as_str).map(str::to_owned));
            }
            _ => {}
        }
        if let Some(exports) = pkg.get("exports") {
            string_leaves(exports, &mut targets);
        }
    }
    // Conditions often name one file several ways (`types` and `default`);
    // look each up once, as a `*` pattern walks the directory.
    targets.sort_by(|a, b| strip_compiled_extension(a).cmp(strip_compiled_extension(b)));
    targets.dedup_by(|a, b| {
        a.contains('*') && strip_compiled_extension(a) == strip_compiled_extension(b)
    });
    let mut files: Vec<PathBuf> =
        targets.iter().flat_map(|t| package_target_files(project, dir, t)).collect();
    if let Some(Value::Object(scripts)) = pkg.as_deref().and_then(|p| p.get("scripts")) {
        let scripts = scripts.values().filter_map(Value::as_str);
        files.extend(scripts.flat_map(|s| script_files(dir, s)));
    }
    for base in [dir.to_path_buf(), dir.join("src")] {
        for name in ENTRY_NAMES {
            for ext in RESOLVE_EXTENSIONS {
                let file = base.join(format!("{name}.{ext}"));
                if file.is_file() {
                    files.push(file.canonicalize().unwrap_or(file));
                }
            }
        }
    }
    trace!("{} entry points in {}", files.len(), dir.display());
    files
}

/// The source files a package.json script names, such as `utils/build.js`
/// in `node utils/build.js && tsc`: each word with a JS/TS extension that is
/// a file in the package.
fn script_files(dir: &Path, script: &str) -> Vec<PathBuf> {
    script
        .split(|c: char| c.is_whitespace() || matches!(c, '&' | '|' | ';' | '(' | ')' | '='))
        .map(|w| w.trim_matches(|c| c == '"' || c == '\''))
        .filter(|w| {
            Path::new(w)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| RESOLVE_EXTENSIONS.contains(&e))
        })
        .map(|w| dir.join(w))
        .filter(|p| p.is_file())
        .map(|p| p.canonicalize().unwrap_or(p))
        .collect()
}

/// Every string in an `exports` value, whatever its conditions.
fn string_leaves(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => items.iter().for_each(|i| string_leaves(i, out)),
        Value::Object(o) => o.values().for_each(|i| string_leaves(i, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(root: &Path, path: &str, content: &str) {
        let p = root.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn rel(root: &Path, files: Vec<PathBuf>) -> Vec<String> {
        let root = root.canonicalize().unwrap();
        files
            .iter()
            .map(|f| f.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/"))
            .collect()
    }

    #[test]
    fn package_json_fields_and_index_files() {
        let t = TempDir::new().unwrap();
        let root = t.path();
        write(root, "pnpm-workspace.yaml", "packages:\n  - 'packages/*'\n");
        write(
            root,
            "package.json",
            r#"{"name":"root","bin":{"tool":"./bin/tool.js"},
               "scripts":{"gen":"node utils/gen.mjs --out=dist/x.js && tsc","lint":"eslint 'src/**/*.ts'"}}"#,
        );
        write(root, "bin/tool.js", "");
        write(root, "utils/gen.mjs", "");
        // dist/ is mapped to src/. `types` finds the same file.
        write(
            root,
            "packages/ui/package.json",
            r#"{"name":"ui","main":"./dist/index.js","types":"./dist/index.d.ts",
               "exports":{".":{"import":"./dist/index.js"},"./icons/*":{"types":"./dist/icons/*.d.ts","default":"./dist/icons/*.js"}}}"#,
        );
        write(root, "packages/ui/src/index.ts", "");
        // Built output doesn't change the entries: the source still wins.
        write(root, "packages/ui/dist/index.js", "");
        write(root, "packages/ui/src/icons/star.tsx", "");
        write(root, "packages/ui/src/icons/nested/moon.ts", "");
        write(root, "packages/ui/src/internal.ts", "");
        // No package.json fields: index and cli files.
        write(root, "packages/app/package.json", r#"{"name":"app"}"#);
        write(root, "packages/app/src/cli.ts", "");
        write(root, "packages/app/index.js", "");
        write(root, "packages/app/src/other.ts", "");

        let project = Project::load(root);
        assert_eq!(
            rel(root, package_entry_points(&project)),
            [
                "bin/tool.js",
                "packages/app/index.js",
                "packages/app/src/cli.ts",
                "packages/ui/src/icons/nested/moon.ts",
                "packages/ui/src/icons/star.tsx",
                "packages/ui/src/index.ts",
                "utils/gen.mjs",
            ]
        );
    }
}
