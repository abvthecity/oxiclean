//! The `bloat` command: import bloat and barrel files, from one module
//! graph.

use oxiclean_core::{
    Analysis, BuildOptions, CheckResult, CommandResult, CommandRule, Result, run_command,
};

use crate::{
    barrels, checker,
    config::{BARRELS_RULE, BARRELS_STYLE, Config, REPORT_STYLE, RULE},
};

/// Runs one rule on the command's module graph.
type Run = fn(&Analysis) -> Result<CheckResult>;

/// The rules the `bloat` command runs, in order, with how each is worded
/// and run.
const RULES: [CommandRule<Run>; 2] =
    [(RULE, REPORT_STYLE, checker::run_on), (BARRELS_RULE, BARRELS_STYLE, barrels::run_on)];

/// Runs each of the `bloat` and `barrels` rules the config file leaves on,
/// on one module graph. The graph reads exports only when `barrels` is on,
/// as only it needs them. Rules that are off don't run; a rule that fails
/// is reported and the other still runs.
///
/// # Errors
/// Fails if the project root can't be found, the config file is invalid,
/// or no entry files are found.
pub fn run_bloat_command(cfg: &Config) -> Result<CommandResult> {
    run_command(&cfg.common, "bloat", &RULES, |workspace, rules| {
        let options = BuildOptions::default().read_exports(rules.contains(BARRELS_RULE));
        let analysis = workspace.analyze_with(options)?;
        Ok(rules.run(|run| run(&analysis)))
    })
}
