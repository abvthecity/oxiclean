//! The `dependencies` command: every rule of this crate the config file
//! leaves on, from one read of the workspace.

use oxiclean_core::{CommandResult, Result};
use tracing::info;

use crate::{
    catalog, checker,
    config::{
        CATALOG_RULE, CATALOG_STYLE, Config, DEPENDENCIES_RULE, DEPENDENCIES_STYLE, HEAVY_RULE,
        HEAVY_STYLE, VERSIONS_RULE, VERSIONS_STYLE,
    },
    heavy, versions,
};

/// The rules the `dependencies` command runs, in order.
pub const RULES: [&str; 4] = [DEPENDENCIES_RULE, HEAVY_RULE, CATALOG_RULE, VERSIONS_RULE];

/// Runs each rule in [`RULES`] the config file leaves on. They share the
/// root, the config file and the project's manifests; unused and heavy
/// dependencies each build their own module graph, as heavy dependencies
/// follow imports into `node_modules` and unused ones follow type-only
/// imports. Rules that are off don't run.
///
/// # Errors
/// Fails if the project root can't be found, the config file is invalid,
/// or a rule that builds a module graph finds no entry files.
pub fn run_dependencies_command(cfg: &Config) -> Result<CommandResult> {
    let workspace = cfg.common.workspace()?;
    let mut out = CommandResult::new(workspace.root.clone());
    for rule in RULES.into_iter().filter(|r| workspace.enabled(r)) {
        let (style, result) = match rule {
            DEPENDENCIES_RULE => (DEPENDENCIES_STYLE, checker::run_on(workspace.clone())?),
            HEAVY_RULE => (HEAVY_STYLE, heavy::run_on(workspace.clone())?),
            CATALOG_RULE => (CATALOG_STYLE, catalog::run_on(&workspace)?),
            _ => (VERSIONS_STYLE, versions::run_on(&workspace)?),
        };
        out.push(rule, style, result);
    }
    if out.rules.is_empty() {
        info!("Every dependencies rule is off");
    }
    Ok(out)
}
