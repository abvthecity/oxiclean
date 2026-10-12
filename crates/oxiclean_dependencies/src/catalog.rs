//! Unused pnpm catalog entries, as knip's `catalog` issue type: entries of
//! `catalog` and `catalogs` in pnpm-workspace.yaml that no package.json of
//! the workspace refers to with `catalog:`.

use std::{collections::HashSet, fs, path::Path};

use oxiclean_core::{
    CheckResult, Finding, FindingKind, Project, Result, UnusedCatalogEntry, parse_jsonc,
};
use serde_json::Value;
use tracing::{debug, info};

use crate::config::{CATALOG_RULE, Config, DEFAULT_THRESHOLD};

/// The catalog `catalog:` and `catalog:default` refer to.
const DEFAULT_CATALOG: &str = "default";

/// Runs the check with `cfg`.
///
/// # Errors
/// Fails if the root can't be found or read, or the config file is invalid.
pub fn run_catalog_check(cfg: &Config) -> Result<CheckResult> {
    info!("Starting {CATALOG_RULE} check");
    let workspace = cfg.common.workspace()?;
    let policy = workspace.policy(CATALOG_RULE, DEFAULT_THRESHOLD)?;
    let file = workspace.root.join("pnpm-workspace.yaml");
    let unused = fs::read_to_string(&file)
        .map(|yaml| unused_catalog_entries(&yaml, &workspace.project))
        .unwrap_or_default();
    let findings = unused
        .into_iter()
        .map(|e| Finding::new(file.clone(), FindingKind::UnusedCatalogEntry(e), 1))
        .collect();
    let manifests = manifest_dirs(&workspace.project).len();
    info!("{CATALOG_RULE} check complete");
    Ok(workspace.finish(&policy, findings, manifests + 1))
}

/// The entries of the catalogs in `yaml`, the text of pnpm-workspace.yaml,
/// that no package.json of `project` and no override refers to.
#[must_use]
pub fn unused_catalog_entries(yaml: &str, project: &Project) -> Vec<UnusedCatalogEntry> {
    let lines = mapping_lines(yaml);
    let mut used: HashSet<(String, String)> = HashSet::new();
    let mut refer = |name: &str, version: &str| {
        if let Some(catalog) = version.trim().strip_prefix("catalog:") {
            let catalog = catalog.trim();
            let catalog = if catalog.is_empty() { DEFAULT_CATALOG } else { catalog };
            used.insert((catalog.to_string(), name.to_string()));
        }
    };
    for dir in manifest_dirs(project) {
        let Some(pkg) = fs::read_to_string(dir.join("package.json"))
            .ok()
            .and_then(|s| parse_jsonc(s.trim_start_matches('\u{feff}')))
        else {
            continue;
        };
        for field in ["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"]
        {
            if let Some(Value::Object(deps)) = pkg.get(field) {
                for (name, version) in deps {
                    refer(name, version.as_str().unwrap_or_default());
                }
            }
        }
        for overrides in [pkg.get("pnpm").and_then(|p| p.get("overrides")), pkg.get("overrides")] {
            if let Some(Value::Object(o)) = overrides {
                for (selector, version) in o {
                    refer(&override_target(selector), version.as_str().unwrap_or_default());
                }
            }
        }
    }
    for l in &lines {
        if let [top, selector] = l.path.as_slice()
            && top == "overrides"
        {
            refer(&override_target(selector), l.value.as_deref().unwrap_or_default());
        }
    }
    let mut out = Vec::new();
    for l in lines {
        let (catalog, name) = match l.path.as_slice() {
            [top, name] if top == "catalog" => (DEFAULT_CATALOG.to_string(), name.clone()),
            [top, catalog, name] if top == "catalogs" => (catalog.clone(), name.clone()),
            _ => continue,
        };
        if l.value.is_some() && !used.contains(&(catalog.clone(), name.clone())) {
            out.push(UnusedCatalogEntry::new(name, catalog, l.line));
        }
    }
    debug!("{} unused catalog entries", out.len());
    out
}

/// The root and every workspace package directory.
fn manifest_dirs(project: &Project) -> Vec<&Path> {
    let mut dirs = vec![project.root.as_path()];
    dirs.extend(project.workspace_packages().into_iter().map(|(_, d)| d));
    dirs
}

/// The package an override selector replaces: `bar` in `foo@1>bar@^2`.
fn override_target(selector: &str) -> String {
    let last = selector.rsplit('>').next().unwrap_or(selector).trim();
    // A version follows the name's own `@`: `bar@^2`, `@scope/a@1`.
    let start = usize::from(last.starts_with('@'));
    last[start..].find('@').map_or_else(|| last.to_string(), |i| last[..start + i].to_string())
}

/// A `key: value` line of a YAML block mapping, with the keys of the
/// mappings it is nested in.
#[derive(Debug, PartialEq)]
struct MappingLine {
    path: Vec<String>,
    /// The scalar value, or `None` for a key that opens a nested mapping.
    value: Option<String>,
    /// 1-based line.
    line: u32,
}

/// The mapping lines of `yaml`, read as far as pnpm-workspace.yaml needs:
/// block mappings of scalars, nested by indentation, with comments and
/// quotes. Sequence items and flow collections are skipped.
fn mapping_lines(yaml: &str) -> Vec<MappingLine> {
    let mut out = Vec::new();
    let mut parents: Vec<(usize, String)> = Vec::new();
    for (i, raw) in yaml.lines().enumerate() {
        let text = raw.trim_start();
        let indent = raw.len() - text.len();
        if text.is_empty() || text.starts_with('#') || text.starts_with("---") {
            continue;
        }
        while parents.last().is_some_and(|(at, _)| *at >= indent) {
            parents.pop();
        }
        if text.starts_with('-') {
            continue;
        }
        let Some((key, rest)) = split_key(text) else { continue };
        let value = scalar(rest);
        let mut path: Vec<String> = parents.iter().map(|(_, k)| k.clone()).collect();
        path.push(key.clone());
        if value.is_none() {
            parents.push((indent, key));
        }
        out.push(MappingLine { path, value, line: u32::try_from(i + 1).unwrap_or(u32::MAX) });
    }
    out
}

/// `key` and the rest of a `key: value` line, the key unquoted.
fn split_key(text: &str) -> Option<(String, &str)> {
    if let Some(q) = text.chars().next().filter(|c| matches!(c, '\'' | '"')) {
        let close = text[1..].find(q)? + 1;
        let rest = text[close + 1..].trim_start().strip_prefix(':')?;
        return Some((text[1..close].to_string(), rest));
    }
    let colon = text
        .match_indices(':')
        .find(|(i, _)| text[i + 1..].chars().next().is_none_or(char::is_whitespace))?;
    Some((text[..colon.0].trim_end().to_string(), &text[colon.0 + 1..]))
}

/// The scalar after a key, unquoted and without a trailing comment, or
/// `None` when the key opens a nested block.
fn scalar(rest: &str) -> Option<String> {
    let rest = rest.trim();
    if rest.is_empty() || rest.starts_with('#') {
        return None;
    }
    if let Some(q) = rest.chars().next().filter(|c| matches!(c, '\'' | '"')) {
        let close = rest[1..].find(q).map_or(rest.len(), |c| c + 1);
        return Some(rest[1..close].to_string());
    }
    Some(rest.split(" #").next().unwrap_or(rest).trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nested_mappings() {
        let yaml = r#"packages:
  - packages/*

catalog:
  react: ^19.0.0 # pinned
  '@types/node': "22"

catalogs:
  # legacy apps
  react18:
    react: ^18.0.0
overrides:
  "foo@1>bar": catalog:
"#;
        let got: Vec<(String, Option<String>, u32)> =
            mapping_lines(yaml).into_iter().map(|l| (l.path.join("."), l.value, l.line)).collect();
        let s = |v: &str| Some(v.to_string());
        assert_eq!(
            got,
            [
                ("packages".into(), None, 1),
                ("catalog".into(), None, 4),
                ("catalog.react".into(), s("^19.0.0"), 5),
                ("catalog.@types/node".into(), s("22"), 6),
                ("catalogs".into(), None, 8),
                ("catalogs.react18".into(), None, 10),
                ("catalogs.react18.react".into(), s("^18.0.0"), 11),
                ("overrides".into(), None, 12),
                ("overrides.foo@1>bar".into(), s("catalog:"), 13),
            ]
        );
    }
    #[test]
    fn override_targets() {
        assert_eq!(override_target("foo@1>bar@^2"), "bar");
        assert_eq!(override_target("@scope/a@1"), "@scope/a");
        assert_eq!(override_target("lodash"), "lodash");
    }
}
