//! `duplicate-packages`: packages a trace shows `tsc` loading from more
//! than one directory, as analyze-trace's duplicate-package report finds
//! them, with the version each directory's package.json gives.
//!
//! Every copy is parsed, bound and checked on its own, and types from
//! two copies of one package don't relate to each other without a full
//! structural comparison. Directories are compared by where they really
//! are, so the symlinks of pnpm, Bun or `preserveSymlinks` that lead to
//! one directory are one copy. One finding per package, on the first
//! copy's package.json, with the other copies as related locations; the
//! programs of a `tsc --build` trace that load several copies are merged
//! into it. pnpm's copies of one version linked against different peer
//! dependencies (`zod@3.22.4_typescript@5.4.0`) say so.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    path::{Path, PathBuf},
};

use oxiclean_core::{
    CheckResult, Finding, FindingKind, Issue, Related, Result, Severity, Workspace,
};
use serde_json::Value;
use tracing::{debug, info};

use crate::{
    config::{DUPLICATE_PACKAGES_DEFAULT_THRESHOLD, DUPLICATE_PACKAGES_RULE, HOT_SPOTS_RULE},
    trace::Trace,
};

/// Runs `duplicate-packages` on `trace`.
///
/// # Errors
/// Fails if the config file has an invalid glob.
pub(crate) fn run_on(workspace: &Workspace, trace: &Trace) -> Result<CheckResult> {
    info!("Starting {DUPLICATE_PACKAGES_RULE} check");
    let policy = workspace.policy(
        DUPLICATE_PACKAGES_RULE,
        DUPLICATE_PACKAGES_DEFAULT_THRESHOLD,
        Severity::Warn,
    )?;
    // Each package loaded from more than one directory by some program:
    // its copies, by where each really is, and how many programs did.
    let mut found: BTreeMap<&str, (BTreeMap<String, PackageCopy>, usize)> = BTreeMap::new();
    for project in &trace.projects {
        for (name, dirs) in &project.packages {
            // `node_modules/.pnpm` and `node_modules/.deno` hold packages;
            // analyze-trace's pattern reads them as packages too.
            if dirs.len() < 2 || name.starts_with('.') {
                continue;
            }
            let mut copies: BTreeMap<String, PackageCopy> = BTreeMap::new();
            for raw in dirs {
                let copy = PackageCopy::new(trace, name, raw);
                copies.entry(copy.key.clone()).or_insert(copy);
            }
            if copies.len() < 2 {
                continue;
            }
            let (all, programs) = found.entry(name).or_default();
            *programs += 1;
            for (key, copy) in copies {
                all.entry(key).or_insert(copy);
            }
        }
    }
    let findings: Vec<Finding> = found
        .into_iter()
        .filter_map(|(name, (copies, programs))| {
            let mut copies: Vec<PackageCopy> = copies.into_values().collect();
            copies.sort_by(|a, b| a.shown.cmp(&b.shown));
            finding(trace, name, &copies, programs)
        })
        .collect();
    info!(findings = findings.len(), "{DUPLICATE_PACKAGES_RULE} check complete");
    let mut result = workspace.finish(&policy, findings, trace.checked_files());
    // `hot-spots` says which traces were skipped, when it runs.
    if !workspace.enabled(HOT_SPOTS_RULE) {
        result.notes.extend(trace.notes.iter().cloned());
    }
    Ok(result)
}

/// One directory a package was loaded from.
#[derive(Debug, Clone)]
struct PackageCopy {
    /// Where it really is, or the path the trace gives when it isn't here.
    key: String,
    /// As shown in messages.
    shown: String,
    /// Its package.json, when that's under the root.
    manifest: Option<PathBuf>,
    version: Option<String>,
    /// For a copy in pnpm's store, the version and the peer dependencies
    /// it was linked against, from its directory's name.
    pnpm: Option<(String, String)>,
}

impl PackageCopy {
    fn new(trace: &Trace, name: &str, raw: &str) -> Self {
        let local = trace.paths.local(raw);
        let real = local.as_ref().and_then(|d| d.canonicalize().ok());
        let here = |p: &&PathBuf| p.starts_with(&trace.root);
        let shown_path = real.as_ref().filter(here).or(local.as_ref().filter(here));
        let shown = shown_path.map_or_else(|| raw.replace('\\', "/"), |p| trace.show(p));
        let key = real.as_ref().map_or_else(|| raw.replace('\\', "/"), |p| p.display().to_string());
        let manifest = shown_path.map(|d| d.join("package.json"));
        let version = manifest.as_deref().and_then(|m| version(trace, m));
        let manifest = manifest.filter(|_| version.is_some());
        let pnpm = pnpm_version(name, &shown);
        Self { key, shown, manifest, version, pnpm }
    }
}

/// The finding for package `name` at `copies`, which `programs` programs
/// load more than one of; `None` when nothing is under the root to report
/// it on.
fn finding(trace: &Trace, name: &str, copies: &[PackageCopy], programs: usize) -> Option<Finding> {
    let mut message = format!("tsc loads `{name}` from {} directories: ", copies.len());
    for (i, c) in copies.iter().enumerate() {
        if i > 0 {
            message.push_str(", ");
        }
        match &c.version {
            Some(v) => {
                let _ = write!(message, "{v} at {}", c.shown);
            }
            None => {
                let _ = write!(message, "an unknown version at {}", c.shown);
            }
        }
    }
    message.push_str("; ");
    message.push_str(&advice(copies));
    if programs > 1 {
        let _ = write!(message, " (in {programs} programs)");
    }
    let file = copies
        .iter()
        .find_map(|c| c.manifest.clone())
        .or_else(|| Some(trace.root.join("package.json")).filter(|m| m.is_file()));
    let Some(file) = file else {
        debug!("Leaving out the duplicate package {name}: no copy is under the root");
        return None;
    };
    let related = copies
        .iter()
        .filter_map(|c| {
            let manifest = c.manifest.as_ref().filter(|m| **m != file)?;
            let what = c.version.as_deref().map_or_else(
                || "another copy".to_string(),
                |v| format!("another copy, version {v}"),
            );
            Some(Related::new(manifest.clone(), 0, what))
        })
        .collect();
    let issue = Issue::new("duplicate-package", name, message, 0).with_related(related);
    Some(Finding::new(file, FindingKind::Issue(issue), copies.len()))
}

/// What would leave one copy of the package, from what tells its copies
/// apart.
fn advice(copies: &[PackageCopy]) -> String {
    let versions =
        |c: &PackageCopy| c.version.clone().or_else(|| c.pnpm.as_ref().map(|p| p.0.clone()));
    let mut all: Vec<Option<String>> = copies.iter().map(versions).collect();
    all.sort();
    all.dedup();
    let same_version = matches!(all.as_slice(), [Some(_)]);
    if same_version
        && copies.iter().all(|c| c.pnpm.is_some())
        && copies.iter().any(|c| c.pnpm.as_ref().is_some_and(|p| !p.1.is_empty()))
    {
        let peers: Vec<&str> = copies
            .iter()
            .filter_map(|c| c.pnpm.as_ref().map(|p| p.1.as_str()))
            .map(|p| if p.is_empty() { "no peers" } else { p })
            .collect();
        return format!(
            "the same version, linked by pnpm against different peer dependencies ({}); align the versions of those peers so one copy is linked",
            peers.join(" and ")
        );
    }
    if same_version {
        "the same version installed twice; dedupe the lockfile, or hoist the package".into()
    } else {
        "align the versions that depend on it so one copy is loaded".into()
    }
}

/// For a directory in pnpm's store,
/// `node_modules/.pnpm/zod@3.22.4_typescript@5.4.0/node_modules/zod`, the
/// version and the peers it was linked against: `3.22.4` and
/// `typescript@5.4.0`. pnpm 8 and later write the peers in parentheses,
/// `zod@3.22.4(typescript@5.4.0)`.
fn pnpm_version(name: &str, dir: &str) -> Option<(String, String)> {
    let (_, rest) =
        dir.rsplit_once("/.pnpm/").or_else(|| dir.strip_prefix(".pnpm/").map(|r| ("", r)))?;
    let (entry, _) = rest.split_once('/')?;
    let prefix = format!("{}@", name.replace('/', "+"));
    let spec = entry.strip_prefix(&prefix)?;
    let cut = spec.find(['_', '(']).unwrap_or(spec.len());
    let (version, peers) = spec.split_at(cut);
    let peers = peers.trim_start_matches('_').replace(")(", ", ").replace(['(', ')'], "");
    Some((version.to_string(), peers.replace('_', ", ").replace('+', "/")))
}

/// The `version` of a package.json under the root.
fn version(trace: &Trace, manifest: &Path) -> Option<String> {
    let text = trace.paths.read_source(manifest)?;
    let json: Value = serde_json::from_str(&text).ok()?;
    json.get("version")?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pnpm_store_directories() {
        let dir = "node_modules/.pnpm/zod@3.22.4_typescript@5.4.0/node_modules/zod";
        assert_eq!(pnpm_version("zod", dir), Some(("3.22.4".into(), "typescript@5.4.0".into())));
        let dir =
            "node_modules/.pnpm/@s+cc@1.1.0(react@18.2.0)(typescript@5.5.4)/node_modules/@s/cc";
        assert_eq!(
            pnpm_version("@s/cc", dir),
            Some(("1.1.0".into(), "react@18.2.0, typescript@5.5.4".into()))
        );
        let dir = "node_modules/.pnpm/csstype@3.1.2/node_modules/csstype";
        assert_eq!(pnpm_version("csstype", dir), Some(("3.1.2".into(), String::new())));
        assert_eq!(pnpm_version("zod", "node_modules/zod"), None);
    }

    #[test]
    fn peer_variants_get_their_own_advice() {
        let copy = |shown: &str, v: &str| PackageCopy {
            key: shown.into(),
            shown: shown.into(),
            manifest: None,
            version: Some(v.into()),
            pnpm: pnpm_version("zod", shown),
        };
        let peers = [
            copy("node_modules/.pnpm/zod@3.22.4_typescript@5.4.0/node_modules/zod", "3.22.4"),
            copy("node_modules/.pnpm/zod@3.22.4_typescript@5.5.0/node_modules/zod", "3.22.4"),
        ];
        assert!(
            advice(&peers)
                .contains("different peer dependencies (typescript@5.4.0 and typescript@5.5.0)")
        );
        let twice = [copy("node_modules/zod", "3.22.4"), copy("a/node_modules/zod", "3.22.4")];
        assert!(advice(&twice).starts_with("the same version installed twice"));
        let versions = [copy("node_modules/zod", "3.22.4"), copy("a/node_modules/zod", "3.23.8")];
        assert!(advice(&versions).starts_with("align the versions"));
    }
}
