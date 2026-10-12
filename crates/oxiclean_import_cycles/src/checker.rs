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
pub fn run_import_cycles_check(cfg: &Config) -> Result<CheckResult> {
    info!("Starting import cycle check");
    let analysis = cfg.common.analyze()?;
    let policy = analysis.policy(RULE, DEFAULT_THRESHOLD)?;
    Ok(check(&analysis, &policy))
}

/// Reports the circular imports in the analysed graph, one finding per cycle,
/// on the cycle's first file by path. Only runtime imports count: type-only
/// imports are not edges of the graph, and dynamic `import()` is followed
/// only when `includeDynamic` is set.
///
/// Cycles come from [`oxiclean_core::ModuleGraph::cycles`]: every circular
/// import runs through the import that closes at least one reported cycle.
#[must_use]
pub fn check(analysis: &Analysis, policy: &RulePolicy) -> CheckResult {
    if policy.min_threshold().is_none() {
        info!("{} is disabled", RULE);
        return analysis.finish(policy, Vec::new());
    }
    let graph = &analysis.graph;
    let findings: Vec<Finding> = graph
        .cycles()
        .into_iter()
        .map(|cycle| {
            let n = cycle.modules.len();
            let steps = (0..n)
                .map(|i| {
                    let (from, to) = (cycle.modules[i], cycle.modules[(i + 1) % n]);
                    let edge = graph.edge(from, to).expect("a cycle follows graph edges");
                    CycleStep::new(graph.module(from).path.clone(), edge.request.clone(), edge.line)
                })
                .collect();
            Finding::new(
                graph.module(cycle.modules[0]).path.clone(),
                FindingKind::Cycle(CycleFinding::new(steps, cycle.group_size)),
                n,
            )
        })
        .collect();

    info!("Import cycle check complete. Found {} cycles", findings.len());
    analysis.finish(policy, findings)
}
