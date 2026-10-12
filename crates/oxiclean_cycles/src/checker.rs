use std::path::Path;

use oxiclean_core::{
    Analysis, CheckResult, CycleFinding, CycleStep, Finding, FindingKind, Result, RulePolicy,
};
use tracing::info;

use crate::config::{Config, DEFAULT_THRESHOLD, RULE};

/// Runs the import cycle check described by `cfg`.
///
/// # Errors
/// Fails if the project root can't be found, the config file is invalid, or
/// no entry files are found.
pub fn run_cycles_check(cfg: &Config) -> Result<CheckResult> {
    info!("Starting import cycle check");
    let analysis = cfg.common.analyze()?;
    let policy = analysis.policy(RULE, DEFAULT_THRESHOLD)?;
    Ok(check(&analysis, &policy))
}

/// Reports the circular imports in the analysed graph, one finding per cycle.
/// Only runtime imports count: type-only imports are not edges of the graph,
/// and dynamic `import()` is followed only when `includeDynamic` is set.
///
/// A cycle is reported on its first file by path that the rule applies to,
/// so `ignorePatterns` and `overrides` hide a cycle only when they cover
/// every file in it. Cycles come from [`oxiclean_core::ModuleGraph::cycles`]:
/// removing the last import of each reported cycle leaves no cycles.
#[must_use]
pub fn check(analysis: &Analysis, policy: &RulePolicy) -> CheckResult {
    if policy.min_threshold().is_none() {
        info!("{} is disabled", RULE);
        return analysis.finish(policy, Vec::new());
    }
    let graph = &analysis.graph;
    let applies = |path: &Path, len: usize| {
        let rel = path.strip_prefix(&analysis.root).unwrap_or(path);
        policy.resolve(rel, analysis.project.package_for(path)).reports(len)
    };
    let findings: Vec<Finding> = graph
        .cycles()
        .into_iter()
        .filter_map(|cycle| {
            let n = cycle.modules.len();
            let first = (0..n).find(|&i| applies(&graph.module(cycle.modules[i]).path, n))?;
            let steps = (0..n)
                .map(|k| {
                    let i = (first + k) % n;
                    let (from, to) = (cycle.modules[i], cycle.modules[(i + 1) % n]);
                    let edge = graph.edge(from, to).expect("a cycle follows graph edges");
                    CycleStep::new(graph.module(from).path.clone(), edge.request.clone(), edge.line)
                })
                .collect();
            Some(Finding::new(
                graph.module(cycle.modules[first]).path.clone(),
                FindingKind::Cycle(CycleFinding::new(steps, cycle.group_size)),
                n,
            ))
        })
        .collect();

    info!("Import cycle check complete. Found {} cycles", findings.len());
    analysis.finish(policy, findings)
}
