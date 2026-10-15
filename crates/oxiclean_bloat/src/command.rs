//! The `bloat` command: import bloat and barrel files, from one module
//! graph.

use oxiclean_core::{BuildOptions, CommandResult, Result};
use tracing::info;

use crate::{
    barrels::check_barrels,
    checker::check,
    config::{
        BARRELS_DEFAULT_THRESHOLD, BARRELS_RULE, BARRELS_STYLE, Config, DEFAULT_THRESHOLD,
        REPORT_STYLE, RULE,
    },
};

/// The rules the `bloat` command runs, in order.
pub const RULES: [&str; 2] = [RULE, BARRELS_RULE];

/// Runs each rule in [`RULES`] the config file leaves on, on one module
/// graph. The graph reads exports only when `barrels` is on, as only it
/// needs them. Rules that are off don't run.
///
/// # Errors
/// Fails if the project root can't be found, the config file is invalid,
/// or no entry files are found.
pub fn run_bloat_command(cfg: &Config) -> Result<CommandResult> {
    let workspace = cfg.common.workspace()?;
    let mut out = CommandResult::new(workspace.root.clone());
    let (bloat, barrels) = (workspace.enabled(RULE), workspace.enabled(BARRELS_RULE));
    if !bloat && !barrels {
        info!("Every bloat rule is off");
        return Ok(out);
    }
    let analysis = workspace.analyze_with(BuildOptions::default().read_exports(barrels))?;
    if bloat {
        let policy = analysis.policy(RULE, DEFAULT_THRESHOLD)?;
        out.push(RULE, REPORT_STYLE, check(&analysis, &policy));
    }
    if barrels {
        let policy = analysis.policy(BARRELS_RULE, BARRELS_DEFAULT_THRESHOLD)?;
        out.push(BARRELS_RULE, BARRELS_STYLE, check_barrels(&analysis, &policy));
    }
    Ok(out)
}
