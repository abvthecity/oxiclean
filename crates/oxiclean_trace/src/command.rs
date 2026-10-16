//! The `trace` command: every rule of this crate the config file leaves
//! on, from one read of the trace directory.

use std::path::PathBuf;

use oxiclean_core::{
    CheckResult, CommandResult, CommandRule, Error, Result, Workspace, run_command,
};

use crate::{
    config::{
        Config, DUPLICATE_PACKAGES_RULE, DUPLICATE_PACKAGES_STYLE, HOT_SPOTS_RULE, HOT_SPOTS_STYLE,
    },
    duplicates, hot_spots,
    trace::Trace,
};

/// Runs one rule on a workspace and its trace.
type Run = fn(&Workspace, &Trace) -> Result<CheckResult>;

/// The rules the `trace` command runs, in order, with how each is worded
/// and run.
const RULES: [CommandRule<Run>; 2] = [
    (HOT_SPOTS_RULE, HOT_SPOTS_STYLE, hot_spots::run_on),
    (DUPLICATE_PACKAGES_RULE, DUPLICATE_PACKAGES_STYLE, duplicates::run_on),
];

/// The names of the rules the `trace` command runs, in order.
#[must_use]
pub fn rules() -> Vec<&'static str> {
    oxiclean_core::rule_names(&RULES)
}

/// Runs each rule of [`rules`] the config file leaves on, on the trace in
/// `cfg.dir`, or in `trace.dir` of the config file. Rules that are off
/// don't run; a rule that fails is reported and the other still runs.
///
/// # Errors
/// Fails if the root can't be found or read, the config file is invalid,
/// no trace directory is given, or the directory holds no readable trace.
pub fn run_trace_command(cfg: &Config) -> Result<CommandResult> {
    run_command(&cfg.common, "trace", &RULES, |workspace, rules| {
        let dir = trace_dir(cfg, &workspace)?;
        let trace = Trace::load(&workspace, &dir, rules.contains(HOT_SPOTS_RULE))?;
        let mut result = rules.run(|run| run(&workspace, &trace));
        // Once, on the first rule that ran.
        if let Some(first) = result.rules.iter_mut().find(|r| r.error.is_none()) {
            first.result.notes.extend(trace.notes.iter().cloned());
        }
        Ok(result)
    })
}

/// Runs `hot-spots` alone, on the trace in `cfg.dir` or in `trace.dir`
/// of the config file.
///
/// # Errors
/// As [`run_trace_command`], and fails if the config file has an invalid
/// glob.
pub fn run_hot_spots_check(cfg: &Config) -> Result<CheckResult> {
    let workspace = cfg.common.workspace()?;
    let trace = Trace::load(&workspace, &trace_dir(cfg, &workspace)?, true)?;
    with_notes(hot_spots::run_on(&workspace, &trace), &trace)
}

/// Runs `duplicate-packages` alone, on the trace in `cfg.dir` or in
/// `trace.dir` of the config file.
///
/// # Errors
/// As [`run_hot_spots_check`].
pub fn run_duplicate_packages_check(cfg: &Config) -> Result<CheckResult> {
    let workspace = cfg.common.workspace()?;
    let trace = Trace::load(&workspace, &trace_dir(cfg, &workspace)?, false)?;
    with_notes(duplicates::run_on(&workspace, &trace), &trace)
}

/// `result` with what loading `trace` skipped.
fn with_notes(result: Result<CheckResult>, trace: &Trace) -> Result<CheckResult> {
    result.map(|mut r| {
        r.notes.extend(trace.notes.iter().cloned());
        r
    })
}

/// The trace directory: the one given, else `trace.dir` under the root.
fn trace_dir(cfg: &Config, workspace: &Workspace) -> Result<PathBuf> {
    if let Some(dir) = &cfg.dir {
        // Absolute, so findings on the trace's own files are under the root
        // when the directory is.
        return Ok(dir
            .canonicalize()
            .or_else(|_| std::path::absolute(dir))
            .unwrap_or_else(|_| dir.clone()));
    }
    let dir = workspace.config.trace.dir.as_deref().ok_or(Error::NoTraceDir)?;
    Ok(workspace.root.join(dir))
}
