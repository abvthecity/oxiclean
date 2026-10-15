//! How `tsc` finds the files it loads for an import.

use std::path::{Path, PathBuf};

use oxiclean_core::Project;
use serde_json::Value;

use crate::tsconfig::TsConfigFile;

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

/// The file `tsc` reads for the package in `dir`: its `exports` `.`
/// entry when the program's module resolution reads `exports`, else
/// `types` or `typings`, else `main`, else `index`. `module` is never read.
pub(crate) fn package_entry(
    project: &Project,
    program: &TsConfigFile,
    dir: &Path,
) -> Option<PathBuf> {
    let Some(pkg) = project.package_json(dir) else {
        return resolve_typed(project, &dir.join("index"));
    };
    if let Some(exports) = pkg.get("exports").filter(|_| reads_exports(program)) {
        let root = match exports {
            Value::Object(map) if map.keys().any(|k| k.starts_with('.')) => map.get(".")?,
            other => other,
        };
        let target = condition_target(root, &conditions(program))?;
        return resolve_typed(project, &dir.join(target));
    }
    ["types", "typings", "main"]
        .iter()
        .find_map(|k| pkg.get(k)?.as_str())
        .and_then(|t| resolve_typed(project, &dir.join(t)))
        .or_else(|| resolve_typed(project, &dir.join("index")))
}

/// Whether `program`'s module resolution reads package.json `exports`:
/// `node16`, `nodenext` and `bundler` do, `node10` and `classic` don't.
pub(crate) fn reads_exports(program: &TsConfigFile) -> bool {
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
pub(crate) fn conditions(program: &TsConfigFile) -> Vec<String> {
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

/// How a program resolves packages: whether it reads `exports`, and with
/// which conditions.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Mode {
    pub(crate) exports: bool,
    pub(crate) conditions: Vec<String>,
}

impl Mode {
    pub(crate) fn of(program: &TsConfigFile) -> Self {
        Self { exports: reads_exports(program), conditions: conditions(program) }
    }
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
    if request.starts_with("./")
        || request.starts_with("../")
        || request == "."
        || request == ".."
        || request.starts_with('/')
    {
        return resolve_typed(project, &path_clean::clean(dir.join(request)));
    }
    if oxiclean_core::is_builtin(request) {
        return None;
    }
    let (name, sub) = oxiclean_core::split_package_request(request);
    let types_name = match name.strip_prefix('@') {
        Some(scoped) => format!("@types/{}", scoped.replacen('/', "__", 1)),
        None => format!("@types/{name}"),
    };
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
            pkg.as_ref().and_then(|p| types_versions(p, e))
        });
        return mapped
            .and_then(|m| resolve_typed(project, &dir.join(m)))
            .or_else(|| entry.and_then(|t| resolve_typed(project, &dir.join(t))))
            .or_else(|| resolve_typed(project, &dir.join("index")));
    }
    let mapped = pkg.as_ref().and_then(|p| types_versions(p, sub));
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

/// Where the first `typesVersions` entry maps the subpath `sub`.
fn types_versions(pkg: &Value, sub: &str) -> Option<String> {
    let Value::Object(versions) = pkg.get("typesVersions")? else { return None };
    let Value::Object(map) = versions.values().next()? else { return None };
    map.iter()
        .filter_map(|(k, v)| {
            let star = match k.split_once('*') {
                None => (k == sub).then_some("")?,
                Some((prefix, suffix)) => sub.strip_prefix(prefix)?.strip_suffix(suffix)?,
            };
            let target = v.as_array()?.first()?.as_str()?;
            Some((k.len(), target.replace('*', star)))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, t)| t)
}
