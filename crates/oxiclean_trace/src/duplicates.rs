//! `duplicate-packages`: packages a trace shows `tsc` loading from more
//! than one directory, as analyze-trace's duplicate-package report finds
//! them, with the version each directory's package.json gives.
//!
//! Every copy is parsed, bound and checked on its own, and types from
//! two copies of one package don't relate to each other without a full
//! structural comparison. One finding per package, on the first copy's
//! package.json, with the other copies as related locations. A package
//! found the same way by several programs of a `tsc --build` trace is
//! reported once.

use std::{collections::BTreeMap, fs, path::PathBuf};

use oxiclean_core::{
    CheckResult, Finding, FindingKind, Issue, Related, Result, Severity, Workspace,
};
use serde_json::Value;
use tracing::info;

use crate::{
    config::{DUPLICATE_PACKAGES_DEFAULT_THRESHOLD, DUPLICATE_PACKAGES_RULE},
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
    // Each package's directories, sorted, to the programs that load them.
    let mut found: BTreeMap<(String, Vec<String>), usize> = BTreeMap::new();
    for project in &trace.projects {
        for (name, dirs) in &project.packages {
            // `node_modules/.pnpm` and `node_modules/.deno` hold packages;
            // analyze-trace's pattern reads them as packages too.
            if dirs.len() < 2 || name.starts_with('.') {
                continue;
            }
            let mut dirs = dirs.clone();
            dirs.sort();
            *found.entry((name.clone(), dirs)).or_default() += 1;
        }
    }
    let findings: Vec<Finding> = found
        .into_iter()
        .map(|((name, dirs), programs)| finding(trace, &name, &dirs, programs))
        .collect();
    info!(findings = findings.len(), "{DUPLICATE_PACKAGES_RULE} check complete");
    Ok(workspace.finish(&policy, findings, trace.checked_files()))
}

/// The finding for package `name` at `dirs`, which `programs` programs
/// load.
fn finding(trace: &Trace, name: &str, dirs: &[String], programs: usize) -> Finding {
    let copies: Vec<(String, Option<PathBuf>, Option<String>)> = dirs
        .iter()
        .map(|raw| {
            let local = trace.paths.local(raw);
            let manifest = local.as_ref().map(|d| d.join("package.json")).filter(|m| m.is_file());
            let version = manifest.as_ref().and_then(|m| version(m));
            let shown = local.as_ref().map_or_else(|| raw.clone(), |d| trace.show(d));
            (shown, manifest.or(local), version)
        })
        .collect();
    let listed: Vec<String> = copies
        .iter()
        .map(|(dir, _, v)| match v {
            Some(v) => format!("{v} at {dir}"),
            None => format!("an unknown version at {dir}"),
        })
        .collect();
    let mut versions: Vec<&str> = copies.iter().filter_map(|(_, _, v)| v.as_deref()).collect();
    versions.sort_unstable();
    versions.dedup();
    let advice = match versions.as_slice() {
        [_] if copies.iter().all(|(_, _, v)| v.is_some()) => {
            "the same version installed twice; dedupe the lockfile, or hoist the package"
        }
        _ => "align the versions that depend on it so one copy is loaded",
    };
    let mut message = format!(
        "tsc loads `{name}` from {} directories: {}; {advice}",
        dirs.len(),
        listed.join(", ")
    );
    if programs > 1 {
        message.push_str(&format!(" (in {programs} programs)"));
    }
    let related = copies
        .iter()
        .skip(1)
        .filter_map(|(_, file, v)| {
            let file = file.as_ref().filter(|f| f.starts_with(&trace.root) && f.is_file())?;
            let what = v.as_deref().map_or_else(
                || "another copy".to_string(),
                |v| format!("another copy, version {v}"),
            );
            Some(Related::new(file.clone(), 0, what))
        })
        .collect();
    let file = copies[0].1.clone().unwrap_or_else(|| PathBuf::from(&dirs[0]));
    let issue = Issue::new("duplicate-package", name, message, 0).with_related(related);
    Finding::new(file, FindingKind::Issue(issue), dirs.len())
}

/// The `version` of a package.json.
fn version(manifest: &std::path::Path) -> Option<String> {
    let text = fs::read_to_string(manifest).ok()?;
    let json: Value = serde_json::from_str(&text).ok()?;
    json.get("version")?.as_str().map(str::to_string)
}
