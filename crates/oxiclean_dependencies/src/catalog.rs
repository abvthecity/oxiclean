//! Unused pnpm catalog entries, as knip's `catalog` issue type: entries of
//! `catalog` and `catalogs` in pnpm-workspace.yaml that no package.json of
//! the workspace refers to with `catalog:`.

use std::{collections::HashSet, fs, path::Path};

use oxiclean_core::{
    CheckResult, Finding, FindingKind, Project, Result, UnusedCatalogEntry, Workspace, parse_jsonc,
};
use serde_json::Value;
use tracing::{debug, info};

use crate::{
    config::{CATALOG_RULE, Config, DEFAULT_THRESHOLD},
    yaml::mapping_lines,
};

/// The catalog `catalog:` and `catalog:default` refer to.
const DEFAULT_CATALOG: &str = "default";

/// Runs the check with `cfg`.
///
/// # Errors
/// Fails if the root can't be found or read, or the config file is invalid.
pub fn run_catalog_check(cfg: &Config) -> Result<CheckResult> {
    run_on(&cfg.common.workspace()?)
}

/// Runs the unused catalog entries check on `workspace`.
pub(crate) fn run_on(workspace: &Workspace) -> Result<CheckResult> {
    info!("Starting {CATALOG_RULE} check");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_targets() {
        assert_eq!(override_target("foo@1>bar@^2"), "bar");
        assert_eq!(override_target("@scope/a@1"), "@scope/a");
        assert_eq!(override_target("lodash"), "lodash");
    }
}
