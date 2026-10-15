//! The `lint` command: every rule of this crate the config file leaves
//! on, from one read of the workspace.

use oxiclean_core::{CheckResult, CommandResult, CommandRule, Result, Workspace, run_command};

use crate::{
    config::{Config, LINT_CONFIG_RULE, LINT_CONFIG_STYLE},
    lint_config,
};

/// Runs one rule on a workspace.
type Run = fn(&Workspace) -> Result<CheckResult>;

/// The rules the `lint` command runs, in order, with how each is worded
/// and run.
const RULES: [CommandRule<Run>; 1] = [(LINT_CONFIG_RULE, LINT_CONFIG_STYLE, lint_config::run_on)];

/// The names of the rules the `lint` command runs, in order.
#[must_use]
pub fn rules() -> Vec<&'static str> {
    oxiclean_core::rule_names(&RULES)
}

/// Runs each rule of [`rules`] the config file leaves on. Rules that are
/// off don't run; a rule that fails is reported and the others still run.
///
/// # Errors
/// Fails if the root can't be found or read, or the config file is invalid.
pub fn run_lint_command(cfg: &Config) -> Result<CommandResult> {
    run_command(
        &cfg.common,
        "lint",
        &RULES,
        |workspace, rules| Ok(rules.run(|run| run(&workspace))),
    )
}
