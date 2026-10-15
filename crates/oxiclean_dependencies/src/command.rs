//! The `dependencies` command: every rule of this crate the config file
//! leaves on, from one read of the workspace.

use std::borrow::Cow;

use oxiclean_core::{CheckResult, CommandResult, CommandRule, Result, Workspace, run_command};

use crate::{
    catalog, checker,
    config::{
        CATALOG_RULE, CATALOG_STYLE, Config, DEPENDENCIES_RULE, DEPENDENCIES_STYLE, HEAVY_RULE,
        HEAVY_STYLE, VERSIONS_RULE, VERSIONS_STYLE,
    },
    heavy, versions,
};

/// Runs one rule on the workspace, borrowed, or owned when no rule after it
/// needs it.
type Run = fn(Cow<'_, Workspace>) -> Result<CheckResult>;

/// The rules the `dependencies` command runs, in order, with how each is
/// worded and run. Unused and heavy dependencies each build their own
/// module graph, which keeps the workspace's project, so they take it.
const RULES: [CommandRule<Run>; 4] = [
    (DEPENDENCIES_RULE, DEPENDENCIES_STYLE, |w| checker::run_on(w.into_owned())),
    (HEAVY_RULE, HEAVY_STYLE, |w| heavy::run_on(w.into_owned())),
    (CATALOG_RULE, CATALOG_STYLE, |w| catalog::run_on(&w)),
    (VERSIONS_RULE, VERSIONS_STYLE, |w| versions::run_on(&w)),
];

/// Whether `rule` builds a module graph, and so takes the workspace.
fn builds_graph(rule: &str) -> bool {
    matches!(rule, DEPENDENCIES_RULE | HEAVY_RULE)
}

/// Runs each of the `dependencies`, `heavy-dependencies`, `catalog` and
/// `duplicate-versions` rules the config file leaves on. They share the
/// root, the config file and the project's manifests; unused and heavy
/// dependencies each build their own module graph, as heavy dependencies
/// follow imports into `node_modules` and unused ones follow type-only
/// imports. Rules that are off don't run; a rule that fails, as one that
/// builds a graph and finds no entry files, is reported and the others
/// still run.
///
/// # Errors
/// Fails if the project root can't be found or the config file is invalid.
pub fn run_dependencies_command(cfg: &Config) -> Result<CommandResult> {
    run_command(&cfg.common, "dependencies", &RULES, |workspace, mut rules| {
        // The rules that only read the workspace run first, so the last
        // rule that builds a graph takes the workspace rather than a clone.
        rules.run_last(builds_graph);
        let mut left = rules.len();
        let mut workspace = Some(workspace);
        Ok(rules.run(|run| {
            left -= 1;
            const TAKEN: &str = "only the last rule takes the workspace";
            run(if left == 0 {
                Cow::Owned(workspace.take().expect(TAKEN))
            } else {
                Cow::Borrowed(workspace.as_ref().expect(TAKEN))
            })
        }))
    })
}
