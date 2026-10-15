//! The `typecheck` command: every rule of this crate the config file
//! leaves on, from one read of the workspace and its tsconfig files.

use oxiclean_core::{CheckResult, CommandResult, Result, Workspace, report::ReportStyle};
use tracing::info;

use crate::{
    ambient_types,
    config::{
        AMBIENT_TYPES_RULE, AMBIENT_TYPES_STYLE, Config, ISOLATED_DECLARATIONS_RULE,
        ISOLATED_DECLARATIONS_STYLE, LARGE_LITERALS_RULE, LARGE_LITERALS_STYLE, OVERLAP_RULE,
        OVERLAP_STYLE, PROGRAM_FILES_RULE, PROGRAM_FILES_STYLE, REFERENCES_RULE, REFERENCES_STYLE,
        TSCONFIG_RULE, TSCONFIG_STYLE, TYPE_BLOAT_RULE, TYPE_BLOAT_STYLE,
    },
    isolated_declarations, large_literals, overlap, program_files, references,
    tsconfig::TsConfigs,
    tsconfig_options, type_bloat,
};

/// Runs one rule on a workspace and its tsconfig files.
type Run = fn(&Workspace, &TsConfigs) -> Result<CheckResult>;

/// The rules the `typecheck` command runs, in order, with how each is
/// worded and run.
const CHECKS: [(&str, ReportStyle, Run); 8] = [
    (PROGRAM_FILES_RULE, PROGRAM_FILES_STYLE, program_files::run_on),
    (OVERLAP_RULE, OVERLAP_STYLE, overlap::run_on),
    (REFERENCES_RULE, REFERENCES_STYLE, references::run_on),
    (TSCONFIG_RULE, TSCONFIG_STYLE, tsconfig_options::run_on),
    (AMBIENT_TYPES_RULE, AMBIENT_TYPES_STYLE, ambient_types::run_on),
    (ISOLATED_DECLARATIONS_RULE, ISOLATED_DECLARATIONS_STYLE, isolated_declarations::run_on),
    (LARGE_LITERALS_RULE, LARGE_LITERALS_STYLE, large_literals::run_on),
    (TYPE_BLOAT_RULE, TYPE_BLOAT_STYLE, type_bloat::run_on),
];

/// The rules the `typecheck` command runs, in order.
pub const RULES: [&str; 8] = [
    PROGRAM_FILES_RULE,
    OVERLAP_RULE,
    REFERENCES_RULE,
    TSCONFIG_RULE,
    AMBIENT_TYPES_RULE,
    ISOLATED_DECLARATIONS_RULE,
    LARGE_LITERALS_RULE,
    TYPE_BLOAT_RULE,
];

/// Runs each rule in [`RULES`] the config file leaves on, reading the
/// project and its tsconfig files once for all of them. Rules that are off
/// don't run.
///
/// # Errors
/// Fails if the root can't be found or read, or the config file is invalid.
pub fn run_typecheck_command(cfg: &Config) -> Result<CommandResult> {
    let workspace = cfg.common.workspace()?;
    let mut out = CommandResult::new(workspace.root.clone());
    let on: Vec<&(&'static str, ReportStyle, Run)> =
        CHECKS.iter().filter(|(rule, ..)| workspace.enabled(rule)).collect();
    if on.is_empty() {
        info!("Every typecheck rule is off");
        return Ok(out);
    }
    let configs = TsConfigs::load(&workspace.project);
    for &(rule, style, run) in on {
        out.push(rule, style, run(&workspace, &configs)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_match_checks() {
        assert_eq!(RULES, CHECKS.map(|(rule, ..)| rule));
    }
}
