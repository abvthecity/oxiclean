//! Heavy dependencies: third-party packages scored by how many modules
//! importing them pulls in, through `node_modules`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use oxiclean_core::{
    Analysis, BuildOptions, CheckResult, Finding, FindingKind, HeavyDependency, ModuleGraph,
    ModuleId, Result, RulePolicy,
};
use tracing::info;

use crate::{
    checker::{FIELDS, Manifests, dependency_line, package_name},
    config::{Config, HEAVY_DEFAULT_THRESHOLD, HEAVY_RULE},
};

/// Runs the heavy dependencies check described by `cfg`. Files in
/// `node_modules` are parsed, so the packages must be installed.
///
/// # Errors
/// Fails if the project root can't be found, an entry glob is invalid, or no
/// entry files are found.
pub fn run_heavy_dependencies_check(cfg: &Config) -> Result<CheckResult> {
    info!("Starting {HEAVY_RULE} check");
    let analysis =
        cfg.common.analyze_package_entries(BuildOptions::default().follow_node_modules(true))?;
    let policy = analysis.policy(HEAVY_RULE, HEAVY_DEFAULT_THRESHOLD)?;
    Ok(check_heavy_dependencies(&analysis, &policy))
}

/// Flags each package a package.json lists whose import, from the files the
/// entries reach, pulls in at least its threshold of unique modules: the
/// modules of every entry point of the package the project imports, and
/// what they import in turn, runtime imports only.
#[must_use]
pub fn check_heavy_dependencies(analysis: &Analysis, policy: &RulePolicy) -> CheckResult {
    if policy.min_threshold().is_none() {
        info!("{HEAVY_RULE} is disabled");
        return analysis.finish(policy, Vec::new());
    }
    let graph = &analysis.graph;
    let manifests = Manifests::load(&analysis.project);

    let mut packages: BTreeMap<(usize, &str), PackageUse> = BTreeMap::new();
    for (id, module) in graph.iter() {
        if module.in_node_modules() {
            continue;
        }
        let Some(owner) = module.path.parent().and_then(|d| manifests.owner(d)) else { continue };
        for edge in &module.edges {
            if !graph.module(edge.target).in_node_modules() {
                continue;
            }
            let Some(name) = package_name(&edge.request) else { continue };
            let Some(listing) =
                manifests.chain(owner).find(|&i| manifests.all[i].all.contains(name))
            else {
                continue;
            };
            let used = packages.entry((listing, name)).or_default();
            used.entries.insert(edge.target);
            used.importers.insert(id);
        }
    }

    let mut sizes: HashMap<&BTreeSet<ModuleId>, usize> = HashMap::new();
    let mut findings = Vec::new();
    for ((i, name), PackageUse { entries, importers }) in &packages {
        let size = *sizes.entry(entries).or_insert_with(|| reach(graph, entries));
        let manifest = &manifests.all[*i];
        let line = FIELDS
            .iter()
            .find(|f| manifest.listed.get(f).is_some_and(|l| l.iter().any(|n| n == name)))
            .map_or(0, |f| dependency_line(&manifest.text, f.key(), name));
        let dep = HeavyDependency::new((*name).to_string(), line, importers.len());
        findings.push(Finding::new(
            manifest.dir.join("package.json"),
            FindingKind::HeavyDependency(dep),
            size,
        ));
    }
    info!("{HEAVY_RULE} check complete. Scored {} packages", findings.len());
    analysis.finish(policy, findings)
}

/// How the project uses a package a manifest lists.
#[derive(Default)]
struct PackageUse {
    /// The package's modules the project imports.
    entries: BTreeSet<ModuleId>,
    /// The files that import it.
    importers: HashSet<ModuleId>,
}

/// The number of unique modules reachable from `starts`, them included.
fn reach(graph: &ModuleGraph, starts: &BTreeSet<ModuleId>) -> usize {
    let mut seen = vec![false; graph.len()];
    let mut stack: Vec<ModuleId> = starts.iter().copied().collect();
    let mut count = 0;
    while let Some(id) = stack.pop() {
        if std::mem::replace(&mut seen[id.index()], true) {
            continue;
        }
        count += 1;
        stack.extend(graph.module(id).edges.iter().map(|e| e.target));
    }
    count
}
