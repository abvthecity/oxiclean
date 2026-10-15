//! How `tsc` finds the files it loads for an import.

use std::path::{Path, PathBuf};

use oxiclean_core::Project;
use serde_json::Value;

use crate::{tsconfig::TsConfigFile, typescript::typescript_version};

/// The file `program`'s `paths` map `name` to: the targets of the exact
/// key, else of the wildcard key with the longest prefix, in order. None
/// when no key matches or no target exists, and `tsc` looks the package up
/// as usual.
pub(crate) fn paths_target(
    project: &Project,
    program: &TsConfigFile,
    name: &str,
) -> Option<PathBuf> {
    let Some(Value::Object(paths)) = program.options.get("paths") else { return None };
    let (star, targets) = paths
        .iter()
        .filter_map(|(key, targets)| match key.split_once('*') {
            None => (key == name).then_some((usize::MAX, "", targets)),
            Some((prefix, suffix)) => name
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix(suffix))
                .map(|star| (prefix.len(), star, targets)),
        })
        .max_by_key(|(rank, _, _)| *rank)
        .map(|(_, star, targets)| (star, targets))?;
    targets
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .find_map(|t| resolve_typed(project, Path::new(&t.replacen('*', star, 1))))
}

/// Whether `program`'s module resolution reads package.json `exports`:
/// `node16`, `nodenext` and `bundler` do, `node10` and `classic` don't.
fn reads_exports(program: &TsConfigFile) -> bool {
    let option = |k: &str| program.options.get(k).and_then(Value::as_str).map(str::to_lowercase);
    match option("moduleResolution").as_deref() {
        Some("node16" | "nodenext" | "bundler") => true,
        Some(_) => false,
        None => matches!(
            option("module").as_deref(),
            Some("node16" | "node18" | "node20" | "nodenext" | "preserve")
        ),
    }
}

/// The `exports` conditions `tsc` matches for `program`.
fn conditions(program: &TsConfigFile) -> Vec<String> {
    let mut out: Vec<String> =
        ["types", "import", "require", "node", "default"].iter().map(ToString::to_string).collect();
    if let Some(Value::Array(custom)) = program.options.get("customConditions") {
        out.extend(custom.iter().filter_map(Value::as_str).map(str::to_string));
    }
    out
}

/// The target of an `exports` entry: the first condition in key order
/// that `conditions` has (`types@<version>` counting as `types`).
pub(crate) fn condition_target<'a>(entry: &'a Value, conditions: &[String]) -> Option<&'a str> {
    match entry {
        Value::String(s) => Some(s),
        Value::Array(items) => items.iter().find_map(|i| condition_target(i, conditions)),
        Value::Object(map) => map
            .iter()
            .filter(|(k, _)| {
                let k = k.split('@').next().unwrap_or(k);
                conditions.iter().any(|c| c == k)
            })
            .find_map(|(_, v)| condition_target(v, conditions)),
        _ => None,
    }
}

/// The file `tsc` loads for `path`: `.js` swapped for `.ts`, `.tsx` or
/// `.d.ts`; the file itself; `path` plus one of those extensions; or, for a
/// directory, its package.json entry or `index`.
pub(crate) fn resolve_typed(project: &Project, path: &Path) -> Option<PathBuf> {
    let text = path.to_string_lossy();
    let swapped = [(".mjs", [".mts", ".d.mts"]), (".cjs", [".cts", ".d.cts"])]
        .into_iter()
        .chain([".js", ".jsx"].map(|e| (e, [".ts", ".d.ts"])))
        .find_map(|(js, ts)| {
            let stem = text.strip_suffix(js)?;
            let tsx = (js == ".jsx" || js == ".js").then(|| format!("{stem}.tsx"));
            ts.iter()
                .map(|e| format!("{stem}{e}"))
                .chain(tsx)
                .map(PathBuf::from)
                .find(|c| c.is_file())
        });
    if swapped.is_some() {
        return swapped;
    }
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    if let Some(found) = [".ts", ".tsx", ".d.ts"]
        .iter()
        .map(|e| PathBuf::from(format!("{text}{e}")))
        .find(|c| c.is_file())
    {
        return Some(found);
    }
    if !path.is_dir() {
        return None;
    }
    let pkg = project.package_json(path);
    pkg.as_ref()
        .and_then(|p| ["types", "typings", "main"].iter().find_map(|k| p.get(k)?.as_str()))
        .map(|t| path.join(t))
        .filter(|t| t.as_path() != path)
        .and_then(|t| resolve_file_only(&t))
        .or_else(|| resolve_file_only(&path.join("index")))
}

/// [`resolve_typed`] for a file only, so directories don't recurse.
pub(crate) fn resolve_file_only(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    let text = path.to_string_lossy();
    let stem =
        [".js", ".mjs", ".cjs", ".jsx"].iter().find_map(|e| text.strip_suffix(e)).unwrap_or(&text);
    [".ts", ".tsx", ".d.ts"]
        .iter()
        .map(|e| PathBuf::from(format!("{stem}{e}")))
        .find(|c| c.is_file())
}

/// How a program resolves packages: whether it reads `exports`, with
/// which conditions, and the TypeScript version `typesVersions` is matched
/// against.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Mode {
    pub(crate) exports: bool,
    pub(crate) conditions: Vec<String>,
    pub(crate) typescript: (u64, u64),
}

impl Mode {
    /// The mode of `program`. When the installed TypeScript can't be found
    /// it's taken to be the newest, which matches the ranges packages
    /// usually list first.
    pub(crate) fn of(project: &Project, program: &TsConfigFile) -> Self {
        Self {
            exports: reads_exports(program),
            conditions: conditions(program),
            typescript: typescript_version(project, &program.dir).unwrap_or((u64::MAX, 0)),
        }
    }

    /// Resolution without `exports`, for following a program's local
    /// imports.
    pub(crate) fn local() -> Self {
        Self { exports: false, conditions: Vec::new(), typescript: (u64::MAX, 0) }
    }
}

/// Whether `request` names a file rather than a package: `.`, `..`, or a
/// path starting with `./`, `../` or `/`.
pub(crate) fn is_relative(request: &str) -> bool {
    request.starts_with("./")
        || request.starts_with("../")
        || request == "."
        || request == ".."
        || request.starts_with('/')
}

/// The `@types` package that types `name`: `@types/name`, or
/// `@types/scope__pkg` for `@scope/pkg`.
pub(crate) fn types_package_name(name: &str) -> String {
    match name.strip_prefix('@') {
        Some(scoped) => format!("@types/{}", scoped.replacen('/', "__", 1)),
        None => format!("@types/{name}"),
    }
}

/// Whether `file` is a declaration file, as `tsc` decides: `.d.ts`,
/// `.d.mts`, `.d.cts`, or any `.ts` file with `.d.` in its name, such as
/// `a.d.json.ts`.
pub(crate) fn is_declaration(file: &Path) -> bool {
    let name = file.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    [".d.mts", ".d.cts"].iter().any(|e| name.ends_with(e))
        || (name.ends_with(".ts") && name.contains(".d."))
}

/// Whether `file` is TypeScript source rather than a declaration file.
pub(crate) fn is_typescript(file: &Path) -> bool {
    let name = file.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    [".ts", ".tsx", ".mts", ".cts"].iter().any(|e| name.ends_with(e)) && !is_declaration(file)
}

/// Whether `file` is TypeScript or JavaScript source, which `tsc` infers
/// types in.
pub(crate) fn is_source(file: &Path) -> bool {
    is_typescript(file)
        || file.extension().is_some_and(|e| ["js", "jsx", "mjs", "cjs"].iter().any(|j| e == *j))
}

pub(crate) fn in_node_modules(file: &Path) -> bool {
    file.components().any(|c| c.as_os_str() == "node_modules")
}

/// The file `tsc` loads for `request` imported from `from`, or `None`
/// for built-in modules and imports it can't find. Packages are looked up
/// in each `node_modules` from `from` up, the package itself before its
/// `@types` package.
pub(crate) fn resolve_types(
    project: &Project,
    mode: &Mode,
    from: &Path,
    request: &str,
) -> Option<PathBuf> {
    let dir = from.parent()?;
    if is_relative(request) {
        return resolve_typed(project, &path_clean::clean(dir.join(request)));
    }
    if oxiclean_core::is_builtin(request) {
        return None;
    }
    let (name, sub) = oxiclean_core::split_package_request(request);
    let types_name = types_package_name(name);
    // A package without types of its own, such as `react`, is typed by its
    // `@types` package.
    let typed = |f: &PathBuf| {
        let name = f.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
        [".ts", ".tsx", ".mts", ".cts"].iter().any(|e| name.ends_with(e))
    };
    dir.ancestors().find_map(|a| {
        let modules = a.join("node_modules");
        package_file(project, mode, &modules.join(name), sub)
            .filter(typed)
            .or_else(|| package_file(project, mode, &modules.join(&types_name), sub))
    })
}

/// The file `tsc` loads for the subpath `sub` (empty for the package
/// itself) of the package in `dir`.
pub(crate) fn package_file(
    project: &Project,
    mode: &Mode,
    dir: &Path,
    sub: &str,
) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    let pkg = project.package_json(dir);
    if mode.exports
        && let Some(exports) = pkg.as_ref().and_then(|p| p.get("exports"))
    {
        let key = if sub.is_empty() { ".".to_string() } else { format!("./{sub}") };
        let (entry, star) = export_entry(exports, &key)?;
        let target = condition_target(entry, &mode.conditions)?.replace('*', star);
        return resolve_typed(project, &dir.join(target));
    }
    if sub.is_empty() {
        // `typesVersions` maps the `types` entry too.
        let entry = pkg
            .as_ref()
            .and_then(|p| ["types", "typings", "main"].iter().find_map(|k| p.get(k)?.as_str()));
        let mapped = entry.and_then(|e| {
            let e = e.strip_prefix("./").unwrap_or(e);
            pkg.as_ref().and_then(|p| types_versions(p, mode.typescript, e))
        });
        return mapped
            .and_then(|m| resolve_typed(project, &dir.join(m)))
            .or_else(|| entry.and_then(|t| resolve_typed(project, &dir.join(t))))
            .or_else(|| resolve_typed(project, &dir.join("index")));
    }
    let mapped = pkg.as_ref().and_then(|p| types_versions(p, mode.typescript, sub));
    mapped
        .and_then(|m| resolve_typed(project, &dir.join(m)))
        .or_else(|| resolve_typed(project, &dir.join(sub)))
}

/// The `exports` entry for `key` (`.` or `./sub`): the exact key, else the
/// `*` pattern with the longest prefix, with what `*` matched.
fn export_entry<'a>(exports: &'a Value, key: &'a str) -> Option<(&'a Value, &'a str)> {
    let Value::Object(map) = exports else {
        return (key == ".").then_some((exports, ""));
    };
    if !map.keys().any(|k| k.starts_with('.')) {
        // Conditions only: the entry for `.`.
        return (key == ".").then_some((exports, ""));
    }
    if let Some(entry) = map.get(key) {
        return Some((entry, ""));
    }
    map.iter()
        .filter_map(|(k, v)| {
            let (prefix, suffix) = k.split_once('*')?;
            let star = key.strip_prefix(prefix)?.strip_suffix(suffix)?;
            Some((prefix.len(), v, star))
        })
        .max_by_key(|(len, _, _)| *len)
        .map(|(_, v, star)| (v, star))
}

/// Where `typesVersions` maps the subpath `sub` for TypeScript
/// `version`: in the first entry whose range matches, the exact key, else
/// the `*` pattern with the longest prefix.
fn types_versions(pkg: &Value, version: (u64, u64), sub: &str) -> Option<String> {
    let Value::Object(versions) = pkg.get("typesVersions")? else { return None };
    let Value::Object(map) = versions.iter().find(|(range, _)| version_matches(range, version))?.1
    else {
        return None;
    };
    let target = |v: &Value| v.as_array()?.first()?.as_str().map(str::to_string);
    if let Some(exact) = map.get(sub) {
        return target(exact);
    }
    map.iter()
        .filter_map(|(k, v)| {
            let (prefix, suffix) = k.split_once('*')?;
            let star = sub.strip_prefix(prefix)?.strip_suffix(suffix)?;
            Some((prefix.len(), target(v)?.replace('*', star)))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, t)| t)
}

/// Whether the npm range `range` (such as `>=4.2`, `<5 || >=5.2`, `*`)
/// includes TypeScript `version`, compared on major and minor.
fn version_matches(range: &str, version: (u64, u64)) -> bool {
    range.split("||").any(|alt| {
        alt.split_whitespace().all(|cmp| {
            let (op, v) = match cmp.find(|c: char| c.is_ascii_digit() || c == '*') {
                Some(at) => cmp.split_at(at),
                None => return false,
            };
            let mut parts = v.split('.').map(|p| p.parse::<u64>().ok());
            let Some(Some(major)) = parts.next() else { return v == "*" };
            let minor = parts.next().flatten();
            let bound = (major, minor.unwrap_or(0));
            match op {
                ">=" => version >= bound,
                ">" => match minor {
                    Some(_) => version > bound,
                    None => version.0 > major,
                },
                "<=" => match minor {
                    Some(_) => version <= bound,
                    None => version.0 <= major,
                },
                "<" => version < bound,
                "" | "=" | "^" | "~" => {
                    version.0 == major && minor.is_none_or(|m| version.1 == m)
                        || (op == "^" && version.0 == major && version >= bound)
                }
                _ => false,
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn export_entry_prefers_exact_then_longest_prefix() {
        let exports =
            json!({ ".": "./i.js", "./a/*": "./a/*.js", "./a/b/*": "./b/*.js", "./x": "./x.js" });
        assert_eq!(export_entry(&exports, "."), Some((&json!("./i.js"), "")));
        assert_eq!(export_entry(&exports, "./x"), Some((&json!("./x.js"), "")));
        assert_eq!(export_entry(&exports, "./a/b/c"), Some((&json!("./b/*.js"), "c")));
        assert_eq!(export_entry(&exports, "./a/c"), Some((&json!("./a/*.js"), "c")));
        assert_eq!(export_entry(&exports, "./nope"), None);
        let conditions_only = json!({ "types": "./i.d.ts" });
        assert_eq!(export_entry(&conditions_only, "."), Some((&conditions_only, "")));
        assert_eq!(export_entry(&conditions_only, "./x"), None);
    }

    #[test]
    fn types_versions_matches_range_then_exact_key() {
        let pkg = json!({ "typesVersions": {
            "<4.0": { "*": ["ts3/*"] },
            ">=4.0": { "*": ["ts4/*"], "a": ["exact.d.ts"], "a/*": ["a/*"] }
        }});
        assert_eq!(types_versions(&pkg, (5, 9), "a").as_deref(), Some("exact.d.ts"));
        assert_eq!(types_versions(&pkg, (5, 9), "a/b").as_deref(), Some("a/b"));
        assert_eq!(types_versions(&pkg, (5, 9), "c").as_deref(), Some("ts4/c"));
        assert_eq!(types_versions(&pkg, (3, 9), "c").as_deref(), Some("ts3/c"));
        let none = json!({ "typesVersions": { "<3.0": { "*": ["old/*"] } } });
        assert_eq!(types_versions(&none, (5, 0), "c"), None);
    }

    #[test]
    fn matches_npm_ranges() {
        for (range, version, expected) in [
            ("*", (5, 0), true),
            (">=4.2", (4, 2), true),
            (">=4.2", (4, 1), false),
            ("<5", (4, 9), true),
            ("<5", (5, 0), false),
            ("<=4", (4, 9), true),
            (">4", (4, 9), false),
            (">=3.1 <4", (3, 5), true),
            (">=3.1 <4", (4, 0), false),
            ("<3 || >=5.2", (5, 3), true),
            ("<3 || >=5.2", (4, 0), false),
            ("4.1", (4, 1), true),
            ("4", (4, 7), true),
        ] {
            assert_eq!(version_matches(range, version), expected, "{range} {version:?}");
        }
    }

    #[test]
    fn condition_target_takes_first_known_condition() {
        let conditions: Vec<String> =
            ["types", "import", "default"].iter().map(ToString::to_string).collect();
        let entry = json!({ "browser": "./b.js", "types@>=5": "./t.d.ts", "default": "./d.js" });
        assert_eq!(condition_target(&entry, &conditions), Some("./t.d.ts"));
        let nested = json!([{ "worker": "./w.js" }, { "import": { "types": "./i.d.ts" } }]);
        assert_eq!(condition_target(&nested, &conditions), Some("./i.d.ts"));
    }

    #[test]
    fn names_types_packages_and_kinds_of_files() {
        assert_eq!(types_package_name("react"), "@types/react");
        assert_eq!(types_package_name("@scope/pkg"), "@types/scope__pkg");
        for (name, declaration) in [
            ("a.d.ts", true),
            ("a.d.mts", true),
            ("a.d.json.ts", true),
            ("a.ts", false),
            ("d.ts", false),
        ] {
            assert_eq!(is_declaration(Path::new(name)), declaration, "{name}");
        }
        assert!(is_source(Path::new("a.mjs")) && !is_source(Path::new("a.d.ts")));
        assert!(!is_typescript(Path::new("a.js")));
        assert!(is_relative("./a") && is_relative("..") && !is_relative(".pkg"));
    }
}
