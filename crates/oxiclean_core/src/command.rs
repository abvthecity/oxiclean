//! What every command shares: a table of rules, each run unless the config
//! file turns it off, with a rule that fails recorded on its result rather
//! than ending the command.

use std::{error::Error as _, path::PathBuf};

use tracing::{info, warn};

use crate::{
    analysis::{CommonArgs, Workspace},
    error::{Error, Result},
    report::{CheckResult, CommandResult, ReportStyle, RuleResult},
};

/// One rule of a command: its name in the config file, how its findings are
/// worded, and how it runs. `F` is the command's way to run a rule, usually
/// a function of what the command's rules share.
pub type CommandRule<F> = (&'static str, ReportStyle, F);

/// The names of the rules of `table`, in order.
#[must_use]
pub fn rule_names<F>(table: &[CommandRule<F>]) -> Vec<&'static str> {
    table.iter().map(|(rule, ..)| *rule).collect()
}

/// Reads the workspace of `common`, keeps the rules of `table` the config
/// file turns on for any file, and, unless every one is off, hands both to
/// `run`. `run` prepares what the rules share and runs them with
/// [`Rules::run`]. When every rule is off, nothing is analysed and the
/// result lists no rule.
///
/// # Errors
/// Fails if the root can't be found or read, the config file is invalid,
/// or `run` fails to prepare what the rules share, such as a module graph
/// with no entry files.
pub fn run_command<'t, F>(
    common: &CommonArgs,
    command: &str,
    table: &'t [CommandRule<F>],
    run: impl FnOnce(Workspace, Rules<'t, F>) -> Result<CommandResult>,
) -> Result<CommandResult> {
    let workspace = common.workspace()?;
    let rules: Vec<(usize, &CommandRule<F>)> =
        table.iter().enumerate().filter(|(_, (rule, ..))| workspace.enabled(rule)).collect();
    if rules.is_empty() {
        info!("Every {command} rule is off");
        return Ok(CommandResult::new(workspace.root));
    }
    let root = workspace.root.clone();
    run(workspace, Rules { root, rules })
}

/// The rules of a command the config file turns on, ready to run.
#[derive(Debug)]
pub struct Rules<'t, F> {
    root: PathBuf,
    /// Each rule with its place in the command's table.
    rules: Vec<(usize, &'t CommandRule<F>)>,
}

impl<F> Rules<'_, F> {
    /// Whether `rule` is on.
    #[must_use]
    pub fn contains(&self, rule: &str) -> bool {
        self.rules.iter().any(|(_, (r, ..))| *r == rule)
    }

    /// Number of rules that are on.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Whether every rule is off; [`run_command`] never hands such a set
    /// to a command.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Runs the rules `last` picks after the others. The report keeps the
    /// table's order.
    pub fn run_last(&mut self, last: impl Fn(&str) -> bool) {
        self.rules.sort_by_key(|(_, (rule, ..))| last(rule));
    }

    /// Runs each rule with `run`, in order. A rule that fails doesn't stop
    /// the others: its error is logged and recorded on its result, and
    /// fails the run.
    pub fn run(self, mut run: impl FnMut(&F) -> Result<CheckResult>) -> CommandResult {
        let mut ran: Vec<(usize, RuleResult)> = self
            .rules
            .into_iter()
            .map(|(index, (rule, style, f))| {
                let result = match run(f) {
                    Ok(result) => RuleResult::new(rule, *style, result),
                    Err(e) => {
                        let message = describe(&e);
                        warn!("{rule} couldn't run: {message}");
                        RuleResult::failed(rule, *style, self.root.clone(), message)
                    }
                };
                (index, result)
            })
            .collect();
        ran.sort_by_key(|(index, _)| *index);
        let mut out = CommandResult::new(self.root);
        out.rules = ran.into_iter().map(|(_, r)| r).collect();
        out
    }
}

/// `e` and its causes, joined, for one line of the report.
fn describe(e: &Error) -> String {
    let mut message = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    const STYLE: ReportStyle = ReportStyle {
        description: "d",
        heading: "h",
        threshold_unit: "",
        value_label: |n| n.to_string(),
        max_label: "",
        max_unit: "",
        ok_message: "ok",
    };

    type Run = fn() -> Result<CheckResult>;

    const TABLE: [CommandRule<Run>; 3] = [
        ("depth", STYLE, || Ok(CheckResult { files_analyzed: 1, ..CheckResult::default() })),
        ("cycles", STYLE, || Err(Error::NoEntries(PathBuf::from("/nowhere")))),
        ("bloat", STYLE, || Ok(CheckResult { files_analyzed: 3, ..CheckResult::default() })),
    ];

    fn project(config: &str) -> TempDir {
        let t = TempDir::new().unwrap();
        fs::write(t.path().join(".oxicleanrc.json"), config).unwrap();
        t
    }

    #[test]
    fn a_failed_rule_does_not_stop_the_others() {
        let t = project("{}");
        let out = run_command(&CommonArgs::with_root(t.path()), "test", &TABLE, |_, rules| {
            Ok(rules.run(|run| run()))
        })
        .unwrap();
        let rules: Vec<(&str, Option<&str>)> =
            out.rules.iter().map(|r| (r.rule, r.error.as_deref())).collect();
        assert_eq!(
            rules,
            [
                ("depth", None),
                ("cycles", Some("no entry files found under /nowhere")),
                ("bloat", None)
            ]
        );
        assert!(out.fails());
        assert_eq!(out.files_analyzed(), 3);
    }

    #[test]
    fn rules_that_are_off_do_not_run() {
        let t = project(r#"{ "rules": { "cycles": "off" } }"#);
        let out = run_command(&CommonArgs::with_root(t.path()), "test", &TABLE, |_, rules| {
            assert!(!rules.contains("cycles") && rules.contains("bloat"));
            Ok(rules.run(|run| run()))
        })
        .unwrap();
        assert_eq!(out.rules.iter().map(|r| r.rule).collect::<Vec<_>>(), ["depth", "bloat"]);
        assert!(!out.fails());

        let t = project(r#"{ "rules": { "depth": "off", "cycles": "off", "bloat": "off" } }"#);
        let out = run_command(&CommonArgs::with_root(t.path()), "test", &TABLE, |_, _| {
            unreachable!("nothing is prepared when every rule is off")
        })
        .unwrap();
        assert!(out.rules.is_empty());
    }

    #[test]
    fn rules_run_last_keep_their_place_in_the_report() {
        let t = project("{}");
        let mut order = Vec::new();
        let out = run_command(&CommonArgs::with_root(t.path()), "test", &TABLE, |_, mut rules| {
            rules.run_last(|rule| rule == "depth");
            Ok(rules.run(|run| {
                let result = run();
                order.push(result.as_ref().map(|r| r.files_analyzed).unwrap_or(0));
                result
            }))
        })
        .unwrap();
        assert_eq!(order, [0, 3, 1]);
        assert_eq!(rule_names(&TABLE), out.rules.iter().map(|r| r.rule).collect::<Vec<_>>());
    }
}
