//! How much time each finding costs, and the health score built from it.
//!
//! Every finding gets an [`Estimate`]: the seconds it adds to one full run
//! of each tool it slows (`tsc` and the editor, the linter, the test runner,
//! the dev server or bundler, the package manager), turned into hours a week
//! from how often the team runs those tools ([`SavingsSettings`]). Findings
//! are ranked by those hours, with CI time weighed against developer time,
//! and the health score compares the hours they cost with what the same
//! project would cost without them.
//!
//! The numbers are estimates from per-file and per-module costs measured on
//! typical projects, not measurements of this one; the README's "Time saved
//! and health score" section lists every assumption. They are meant for
//! ranking findings against each other, and are conservative where a fix's
//! effect is uncertain: a finding about how many modules something reaches
//! saves only what brings it back under its threshold.

use std::collections::HashMap;

use crate::{
    report::{CommandResult, Finding, FindingKind},
    settings::SavingsSettings,
};

/// The size of the project a command ran on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProjectScale {
    /// JS and TS source files, declarations left out.
    pub source_files: usize,
    /// Workspace packages; 0 outside a monorepo.
    pub packages: usize,
}

impl ProjectScale {
    #[must_use]
    pub fn new(source_files: usize, packages: usize) -> Self {
        Self { source_files, packages }
    }

    /// Source files in an average package: what one tsconfig program or
    /// one project reference covers.
    fn files_per_package(self) -> f64 {
        self.source_files as f64 / self.packages.max(1) as f64
    }

    fn files(self) -> f64 {
        self.source_files as f64
    }
}

/// Seconds added to one full run of each tool.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
#[non_exhaustive]
pub struct ToolSeconds {
    /// `tsc`, and the editor loading the project.
    pub typecheck: f64,
    /// ESLint or oxlint.
    pub lint: f64,
    /// The test runner.
    pub test: f64,
    /// The dev server or bundler.
    pub build: f64,
    /// The package manager installing dependencies.
    pub install: f64,
}

impl ToolSeconds {
    /// The tools, named as in the config file's `localRunsPerDay`.
    pub const TOOLS: [&'static str; 5] = ["typecheck", "lint", "test", "build", "install"];

    /// The seconds of each tool, in the order of [`Self::TOOLS`].
    #[must_use]
    pub fn by_tool(&self) -> [f64; 5] {
        [self.typecheck, self.lint, self.test, self.build, self.install]
    }

    /// Seconds across every tool: what one CI run pays.
    #[must_use]
    pub fn total(&self) -> f64 {
        self.by_tool().iter().sum()
    }

    /// The tools this costs time in.
    #[must_use]
    pub fn tools(&self) -> Vec<&'static str> {
        Self::TOOLS.iter().zip(self.by_tool()).filter(|(_, s)| *s > 0.0).map(|(t, _)| *t).collect()
    }

    fn typecheck(s: f64) -> Self {
        Self { typecheck: s, ..Self::default() }
    }

    fn lint(s: f64) -> Self {
        Self { lint: s, ..Self::default() }
    }

    fn install(s: f64) -> Self {
        Self { install: s, ..Self::default() }
    }

    /// No tool slower than [`MAX_SECONDS_PER_RUN`], so one finding with an
    /// enormous measured value can't drown out the rest.
    fn capped(self) -> Self {
        let cap = |s: f64| s.clamp(0.0, MAX_SECONDS_PER_RUN);
        Self {
            typecheck: cap(self.typecheck),
            lint: cap(self.lint),
            test: cap(self.test),
            build: cap(self.build),
            install: cap(self.install),
        }
    }
}

/// Where a finding costs time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Scope {
    /// On developers' machines and in CI.
    Everywhere,
    /// Only on developers' machines, such as rebuilding what an
    /// incremental build would keep.
    Local,
}

impl Scope {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Everywhere => "everywhere",
            Self::Local => "local",
        }
    }
}

/// What fixing a finding saves.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct Estimate {
    /// Seconds saved on each full run of each tool.
    pub per_run: ToolSeconds,
    pub scope: Scope,
    /// Hours developers wait on these tools a week, across the team.
    pub developer_hours: f64,
    /// Hours of CI time a week.
    pub ci_hours: f64,
    /// Developer hours plus CI hours weighed as developer hours: what
    /// findings are ranked by.
    pub weighted_hours: f64,
}

impl Estimate {
    /// `per_run` seconds, `scope`, over a week of `team`.
    #[must_use]
    pub fn new(per_run: ToolSeconds, scope: Scope, team: &SavingsSettings) -> Self {
        let runs = team.local_runs_per_day.by_tool();
        let days = f64::from(team.work_days_per_week.get());
        let developer_seconds: f64 =
            per_run.by_tool().iter().zip(runs).map(|(s, n)| s * f64::from(n)).sum::<f64>()
                * f64::from(team.developers.get())
                * days;
        let ci_seconds = match scope {
            Scope::Everywhere => per_run.total() * f64::from(team.ci_runs_per_day) * days,
            Scope::Local => 0.0,
        };
        let developer_hours = developer_seconds / 3600.0;
        let ci_hours = ci_seconds / 3600.0;
        let weighted_hours =
            developer_hours + ci_hours / f64::from(team.ci_hours_per_developer_hour.get());
        Self { per_run, scope, developer_hours, ci_hours, weighted_hours }
    }
}

/// A score from 0 to 100 and its letter grade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Health {
    pub score: u8,
    /// `A` from 90, `B` from 80, `C` from 70, `D` from 60, else `F`.
    pub grade: char,
}

impl Health {
    /// The share of the time `waste` and `baseline` take together that
    /// `baseline` takes: 100 when nothing is wasted, 50 when the findings
    /// cost as much again as the project itself.
    #[must_use]
    pub fn from_hours(baseline: f64, waste: f64) -> Self {
        let total = baseline + waste;
        let share = if total > 0.0 { baseline / total } else { 1.0 };
        // In 0..=100 by construction, so the cast can't truncate.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let score = (share * 100.0).round().clamp(0.0, 100.0) as u8;
        let grade = match score {
            90.. => 'A',
            80..=89 => 'B',
            70..=79 => 'C',
            60..=69 => 'D',
            _ => 'F',
        };
        Self { score, grade }
    }
}

/// What one rule's findings cost together.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct RuleSavings {
    pub rule: &'static str,
    /// Findings that cost some time each run.
    pub findings: usize,
    pub developer_hours: f64,
    pub ci_hours: f64,
    pub weighted_hours: f64,
}

/// The estimates of every finding of a command, ranked, and the health
/// score they add up to.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Savings {
    /// One entry per rule of the result and finding of that rule, in the
    /// result's order. `None` when the finding's time is counted with
    /// another finding's, such as each import of an entry whose whole graph
    /// is also reported.
    pub estimates: Vec<Vec<Option<Estimate>>>,
    /// `(rule, finding)` indices into the result of the findings that save
    /// time, most weighted hours first.
    pub ranked: Vec<(usize, usize)>,
    /// Each rule that saves time, most weighted hours first.
    pub by_rule: Vec<RuleSavings>,
    /// What every finding saves together.
    pub total: Estimate,
    /// What the project's tools take a week without the findings.
    pub baseline: Estimate,
    pub health: Health,
    /// The settings the hours were computed with.
    pub team: SavingsSettings,
    pub scale: ProjectScale,
}

impl Savings {
    /// Estimates every finding of `result`.
    #[must_use]
    pub fn of(result: &CommandResult) -> Self {
        let team = &result.team;
        let scale = result.scale;
        let mut estimates: Vec<Vec<Option<Estimate>>> = result
            .rules
            .iter()
            .map(|r| {
                r.result
                    .findings
                    .iter()
                    .map(|f| {
                        model(r.rule, f, scale)
                            .map(|(seconds, scope)| Estimate::new(seconds.capped(), scope, team))
                    })
                    .collect()
            })
            .collect();
        for (r, rule_estimates) in result.rules.iter().zip(&mut estimates) {
            let findings = &r.result.findings;
            match r.rule {
                // A bundler transforms a module once however many entries
                // reach it, and an entry's deep imports share one chain:
                // the largest of each package stands for the rest.
                "bloat" | "depth" => keep_largest_per(findings, rule_estimates, |f| {
                    f.package.clone().unwrap_or_default()
                }),
                // tsc reads a package's declarations once a program, and
                // installs a package once, however many import or list it.
                "type-bloat" | "heavy-dependencies" => {
                    keep_largest_per(findings, rule_estimates, |f| key_of(f).to_string());
                }
                // Each kind is priced on the whole project's lint time, so
                // several configs with it share one estimate.
                "lint-config" => keep_largest_per(findings, rule_estimates, |f| match &f.kind {
                    FindingKind::Issue(issue) => issue.code.clone(),
                    _ => String::new(),
                }),
                _ => {}
            }
        }

        let mut ranked: Vec<(usize, usize)> = estimates
            .iter()
            .enumerate()
            .flat_map(|(r, es)| {
                es.iter()
                    .enumerate()
                    .filter(|(_, e)| e.is_some_and(|e| e.per_run.total() > 0.0))
                    .map(move |(f, _)| (r, f))
            })
            .collect();
        let weight = |&(r, f): &(usize, usize)| {
            estimates[r][f].map_or((0.0, 0.0), |e| (e.weighted_hours, e.per_run.total()))
        };
        // Seconds a run break ties, such as when the team settings make
        // every week's hours zero. Other ties keep the result's order,
        // which is already deterministic.
        ranked.sort_by(|a, b| {
            let (a, b) = (weight(a), weight(b));
            b.0.total_cmp(&a.0).then(b.1.total_cmp(&a.1))
        });

        let mut total_seconds = ToolSeconds::default();
        let mut by_rule: Vec<RuleSavings> = Vec::new();
        let (mut developer_hours, mut ci_hours) = (0.0, 0.0);
        for (r, es) in result.rules.iter().zip(&estimates) {
            let mut rule = RuleSavings {
                rule: r.rule,
                findings: 0,
                developer_hours: 0.0,
                ci_hours: 0.0,
                weighted_hours: 0.0,
            };
            for e in es.iter().flatten().filter(|e| e.per_run.total() > 0.0) {
                rule.findings += 1;
                rule.developer_hours += e.developer_hours;
                rule.ci_hours += e.ci_hours;
                rule.weighted_hours += e.weighted_hours;
                let s = e.per_run;
                total_seconds.typecheck += s.typecheck;
                total_seconds.lint += s.lint;
                total_seconds.test += s.test;
                total_seconds.build += s.build;
                total_seconds.install += s.install;
            }
            developer_hours += rule.developer_hours;
            ci_hours += rule.ci_hours;
            if rule.findings > 0 {
                by_rule.push(rule);
            }
        }
        by_rule.sort_by(|a, b| b.weighted_hours.total_cmp(&a.weighted_hours));
        let weighted_hours =
            developer_hours + ci_hours / f64::from(team.ci_hours_per_developer_hour.get());
        let total = Estimate {
            per_run: total_seconds,
            scope: Scope::Everywhere,
            developer_hours,
            ci_hours,
            weighted_hours,
        };
        let baseline = Estimate::new(baseline(scale), Scope::Everywhere, team);
        let health = Health::from_hours(baseline.weighted_hours, total.weighted_hours);
        Self { estimates, ranked, by_rule, total, baseline, health, team: team.clone(), scale }
    }

    /// The estimate of finding `finding` of rule `rule` of the result.
    #[must_use]
    pub fn estimate(&self, rule: usize, finding: usize) -> Option<&Estimate> {
        self.estimates.get(rule)?.get(finding)?.as_ref()
    }

    /// Whether any finding saves time.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ranked.is_empty()
    }
}

/// Keeps the estimate of the finding with the largest value among those
/// with the same `key`, and counts the others with it.
fn keep_largest_per(
    findings: &[Finding],
    estimates: &mut [Option<Estimate>],
    key: impl Fn(&Finding) -> String,
) {
    let mut largest: HashMap<String, usize> = HashMap::new();
    for (i, f) in findings.iter().enumerate() {
        if estimates[i].is_none() {
            continue;
        }
        let best = largest.entry(key(f)).or_insert(i);
        if f.value > findings[*best].value {
            *best = i;
        }
    }
    for (i, f) in findings.iter().enumerate() {
        if estimates[i].is_some() && largest.get(&key(f)) != Some(&i) {
            estimates[i] = None;
        }
    }
}

/// What `f` is about: the package, the import or the issue's key.
fn key_of(f: &Finding) -> &str {
    match &f.kind {
        FindingKind::HeavyDependency(d) => &d.name,
        FindingKind::Issue(issue) => &issue.key,
        _ => "",
    }
}

// What a typical project's tools take, cold, on one CI runner core. tsc
// 5.9 checks 1000 average files in about 10 seconds; linting without types
// takes about 60% of that; test runners and dev servers transform and
// evaluate a module in a millisecond or two.

/// `tsc` parsing, binding and checking one average source file.
const TSC_PER_FILE: f64 = 0.010;
/// The linter linting one file without type information.
const LINT_PER_FILE: f64 = 0.006;
/// A test runner transforming and evaluating one module.
const TEST_PER_MODULE: f64 = 0.002;
/// A bundler or dev server transforming one module.
const BUILD_PER_MODULE: f64 = 0.001;
/// A dev server serving unbundled modules waiting one more round trip
/// before it finds the next level of imports.
const BUILD_PER_LEVEL: f64 = 0.005;
/// Installing one package from a warm cache.
const INSTALL_PER_PACKAGE: f64 = 0.25;
/// Installing a project's dependencies from a warm cache.
const INSTALL_BASELINE: f64 = 15.0;
/// `tsc` parsing and binding one line of declarations: 100000 lines take
/// about a quarter of a second in tsc 5.9.
const TSC_PER_DECLARATION_LINE: f64 = 2.5e-6;
/// `tsc` inferring the type of one value of a literal: 10000 values take
/// 0.06 to 0.4 seconds in tsc 5.9.
const TSC_PER_LITERAL_VALUE: f64 = 2.0e-5;
/// `tsc` creating and comparing one of the types a complex type expands to.
const TSC_PER_TYPE: f64 = 5.0e-5;
/// `tsc` checking the declarations in `node_modules` for one program, as it
/// does with `skipLibCheck` off.
const LIB_CHECK_PER_PROGRAM: f64 = 1.0;
/// `tsc` loading one `@types` package into a program.
const TSC_PER_TYPES_PACKAGE: f64 = 0.05;
/// `tsc` loading the declarations of a package a second time.
const TSC_PER_PACKAGE_COPY: f64 = 0.2;
/// What an export, type or member nothing uses costs `tsc` and the linter:
/// about a twentieth of an average file.
const DEAD_CODE_SHARE: f64 = 0.05;
/// Share of the files importing a barrel or heavy package that load it in
/// a module registry of their own: test files, which test runners isolate.
const ISOLATED_IMPORTER_SHARE: f64 = 0.25;
/// Share of a traced hot spot's time a fix wins back.
const HOT_SPOT_RECOVERED: f64 = 0.8;
/// No finding is estimated to cost one tool more than this many seconds a
/// run.
const MAX_SECONDS_PER_RUN: f64 = 120.0;

/// What the tools take on each full run of a project of `scale` without
/// any findings.
fn baseline(scale: ProjectScale) -> ToolSeconds {
    let files = scale.files();
    ToolSeconds {
        typecheck: files * TSC_PER_FILE,
        lint: files * LINT_PER_FILE,
        test: files * TEST_PER_MODULE,
        build: files * BUILD_PER_MODULE,
        install: INSTALL_BASELINE,
    }
}

/// The names of the rules [`Savings`] has a model for. Every rule of every
/// command must be listed.
pub const MODELED_RULES: &[&str] = &[
    "bloat",
    "barrels",
    "depth",
    "cycles",
    "duplicates",
    "unused-exports",
    "unused-types",
    "unused-enum-members",
    "unused-namespace-members",
    "dependencies",
    "heavy-dependencies",
    "catalog",
    "duplicate-versions",
    "program-files",
    "overlap",
    "references",
    "tsconfig",
    "ambient-types",
    "isolated-declarations",
    "large-literals",
    "type-bloat",
    "complex-types",
    "hot-spots",
    "duplicate-packages",
    "lint-config",
];

/// The seconds finding `f` of `rule` costs each run of each tool, and
/// where; `None` when its time is counted with another finding.
fn model(rule: &str, f: &Finding, scale: ProjectScale) -> Option<(ToolSeconds, Scope)> {
    let value = f.value as f64;
    // Rules about how much an import reaches save what brings it back to
    // the threshold; a fix rarely removes the whole graph.
    let excess = f.value.saturating_sub(f.threshold) as f64;
    // Rules about items save the whole item: it is deleted or annotated.
    let dead_code = ToolSeconds {
        typecheck: TSC_PER_FILE * DEAD_CODE_SHARE,
        lint: LINT_PER_FILE * DEAD_CODE_SHARE,
        ..ToolSeconds::default()
    };
    let issue = match &f.kind {
        FindingKind::Issue(issue) => Some(issue),
        _ => None,
    };
    let code = issue.map_or("", |i| i.code.as_str());
    let key = issue.map_or("", |i| i.key.as_str());
    let package_tsc = scale.files_per_package() * TSC_PER_FILE;
    let everywhere = |s: ToolSeconds| Some((s, Scope::Everywhere));

    match rule {
        // The entry's whole graph carries the time; its imports say where
        // to cut.
        "bloat" => match f.kind {
            FindingKind::WholeGraph => everywhere(ToolSeconds {
                test: excess * TEST_PER_MODULE,
                build: excess * BUILD_PER_MODULE,
                ..ToolSeconds::default()
            }),
            _ => None,
        },
        "barrels" | "heavy-dependencies" => {
            let importers = match &f.kind {
                FindingKind::Barrel(b) => b.importers,
                FindingKind::HeavyDependency(d) => d.importers,
                _ => 1,
            };
            let loads = (importers as f64 * ISOLATED_IMPORTER_SHARE).max(1.0);
            everywhere(ToolSeconds {
                test: excess * TEST_PER_MODULE * loads,
                build: excess * BUILD_PER_MODULE,
                ..ToolSeconds::default()
            })
        }
        "depth" => Some((
            ToolSeconds { build: excess * BUILD_PER_LEVEL, ..ToolSeconds::default() },
            Scope::Local,
        )),
        // Editing any file of a cycle makes hot reloading re-run all of it.
        "cycles" => Some((
            ToolSeconds {
                test: value * TEST_PER_MODULE,
                build: value * BUILD_PER_MODULE,
                ..ToolSeconds::default()
            },
            Scope::Local,
        )),
        "unused-exports" | "unused-types" | "unused-enum-members" | "unused-namespace-members" => {
            everywhere(dead_code)
        }
        "duplicates" => everywhere(ToolSeconds {
            typecheck: dead_code.typecheck * (value - 1.0).max(0.0),
            lint: dead_code.lint * (value - 1.0).max(0.0),
            ..ToolSeconds::default()
        }),
        "dependencies" => everywhere(ToolSeconds::install(INSTALL_PER_PACKAGE)),
        "duplicate-versions" => {
            everywhere(ToolSeconds::install(INSTALL_PER_PACKAGE * (value - 1.0).max(0.0)))
        }
        // Tidier, but no faster.
        "catalog" => everywhere(ToolSeconds::default()),
        "program-files" => {
            // Minified or very large files are about ten average ones.
            let per_file = if code == "large-js" { TSC_PER_FILE * 10.0 } else { TSC_PER_FILE };
            everywhere(ToolSeconds::typecheck(value * per_file))
        }
        "overlap" => everywhere(ToolSeconds::typecheck(value * TSC_PER_FILE)),
        "references" => match code {
            // tsc -b checks the other package's sources again instead of
            // reading its declarations, which costs about half as much.
            "missing-reference" => everywhere(ToolSeconds::typecheck(package_tsc * 0.5)),
            "source-imports" => everywhere(ToolSeconds::typecheck(value * package_tsc * 0.5)),
            // tsc -b builds a project nothing needs before this one.
            "stale-reference" => everywhere(ToolSeconds::typecheck(package_tsc * 0.1)),
            // tsc -b stops with an error: a broken build, not a slow one.
            _ => everywhere(ToolSeconds::default()),
        },
        "tsconfig" => match code {
            "skip-lib-check" => everywhere(ToolSeconds::typecheck(value * LIB_CHECK_PER_PROGRAM)),
            // Only a rerun keeps what the last run built.
            "incremental" => Some((ToolSeconds::typecheck(package_tsc * 0.5), Scope::Local)),
            "debug-option" => {
                let share = match key {
                    "traceResolution" | "explainFiles" | "generateTrace" => 0.3,
                    _ => 0.05,
                };
                everywhere(ToolSeconds::typecheck(package_tsc * share))
            }
            _ => everywhere(ToolSeconds::default()),
        },
        "ambient-types" => everywhere(ToolSeconds::typecheck(value * TSC_PER_TYPES_PACKAGE)),
        // Declarations emitted without type-checking, and packages built in
        // parallel rather than one after another.
        "isolated-declarations" => everywhere(ToolSeconds::typecheck(package_tsc * 0.2)),
        "large-literals" => everywhere(ToolSeconds::typecheck(value * TSC_PER_LITERAL_VALUE)),
        "type-bloat" => everywhere(ToolSeconds::typecheck(excess * TSC_PER_DECLARATION_LINE)),
        "complex-types" => everywhere(ToolSeconds::typecheck(value * TSC_PER_TYPE)),
        // A span inside another is part of its time.
        "hot-spots" => {
            if issue.is_some_and(|i| i.related.iter().any(|r| r.inside)) {
                return None;
            }
            everywhere(ToolSeconds::typecheck(value / 1000.0 * HOT_SPOT_RECOVERED))
        }
        "duplicate-packages" => {
            everywhere(ToolSeconds::typecheck((value - 1.0).max(0.0) * TSC_PER_PACKAGE_COPY))
        }
        "lint-config" => {
            let typed = scale.files() * TSC_PER_FILE;
            let untyped = scale.files() * LINT_PER_FILE;
            let lint = match code {
                // One typed program per tsconfig, instead of the project
                // service's shared ones.
                "project-globs" | "project-service" => typed * 0.3,
                "typed-js-files" => typed * 0.1,
                // Builds a TypeScript program no rule reads.
                "type-aware-unused" => typed * 0.5,
                "slow-rule" if key.contains("no-cycle") || key.contains("no-unused-modules") => {
                    untyped * 0.5
                }
                "slow-rule" => untyped * 0.15,
                // Each directory of build output about a tenth of the source.
                "build-output" => value * untyped * 0.1,
                "eslint-plugin-oxlint" => untyped * 0.2,
                _ => 0.0,
            };
            everywhere(ToolSeconds::lint(lint))
        }
        _ => None,
    }
}

/// `hours` for a person to read: seconds under a minute, minutes under an
/// hour.
#[must_use]
pub fn format_hours(hours: f64) -> String {
    let seconds = hours * 3600.0;
    if seconds < 0.5 {
        "<1s".to_string()
    } else if seconds < 59.5 {
        format!("{seconds:.0}s")
    } else if seconds < 3570.0 {
        format!("{:.0}m", seconds / 60.0)
    } else if hours < 9.95 {
        format!("{hours:.1}h")
    } else {
        format!("{hours:.0}h")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{Barrel, Issue, Related, ReportStyle};
    use std::path::PathBuf;

    const STYLE: ReportStyle = ReportStyle {
        description: "d",
        heading: "h",
        threshold_unit: "",
        value_label: |n| n.to_string(),
        max_label: "",
        max_unit: "",
        ok_message: "ok",
    };

    fn issue(file: &str, code: &str, key: &str, value: usize) -> Finding {
        Finding::new(PathBuf::from(file), FindingKind::Issue(Issue::new(code, key, "m", 1)), value)
    }

    fn result(rules: Vec<(&'static str, Vec<Finding>)>) -> CommandResult {
        let mut result = CommandResult::new(PathBuf::from("/p"));
        result.scale = ProjectScale::new(1000, 4);
        for (rule, findings) in rules {
            result.push(rule, STYLE, crate::CheckResult { findings, ..Default::default() });
        }
        result
    }

    #[test]
    fn hours_follow_the_team() {
        let team = SavingsSettings::default();
        // 1 second of tsc a run: 10 runs a day for 10 developers over 5 days
        // locally, 30 CI runs a day.
        let e = Estimate::new(ToolSeconds::typecheck(1.0), Scope::Everywhere, &team);
        assert!((e.developer_hours - 500.0 / 3600.0).abs() < 1e-9, "{e:?}");
        assert!((e.ci_hours - 150.0 / 3600.0).abs() < 1e-9, "{e:?}");
        assert!((e.weighted_hours - (500.0 + 150.0 / 4.0) / 3600.0).abs() < 1e-9, "{e:?}");

        let local = Estimate::new(ToolSeconds::typecheck(1.0), Scope::Local, &team);
        assert!(local.ci_hours == 0.0 && local.developer_hours == e.developer_hours);
    }

    #[test]
    fn findings_are_ranked_by_weighted_hours() {
        let result = result(vec![
            ("unused-exports", vec![issue("/p/a.ts", "x", "a", 1)]),
            ("overlap", vec![issue("/p/tsconfig.json", "shared-files", "b", 200)]),
            ("tsconfig", vec![issue("/p/tsconfig.json", "skip-lib-check", "skipLibCheck", 1)]),
            ("catalog", vec![issue("/p/pnpm-workspace.yaml", "x", "c", 1)]),
        ]);
        let savings = Savings::of(&result);
        // 200 files checked twice beat one program's lib check, which beats
        // one unused export; an unused catalog entry costs no time.
        assert_eq!(savings.ranked, [(1, 0), (2, 0), (0, 0)]);
        assert_eq!(
            savings.by_rule.iter().map(|r| r.rule).collect::<Vec<_>>(),
            ["overlap", "tsconfig", "unused-exports"]
        );
        assert!(savings.estimate(3, 0).is_some_and(|e| e.weighted_hours == 0.0));
        assert!(savings.health.score < 100 && savings.health.score > 50, "{:?}", savings.health);
    }

    #[test]
    fn overlapping_findings_count_once() {
        let mut deep = Vec::new();
        for (request, depth) in [("./a", 25), ("./b", 30)] {
            let mut f = Finding::new(
                PathBuf::from("/p/e.ts"),
                FindingKind::Import { request: request.into(), resolved: PathBuf::new(), line: 1 },
                depth,
            );
            f.threshold = 20;
            deep.push(f);
        }
        let mut whole = Finding::new(PathBuf::from("/p/e.ts"), FindingKind::WholeGraph, 500);
        whole.threshold = 300;
        let mut import = Finding::new(
            PathBuf::from("/p/e.ts"),
            FindingKind::Import { request: "./x".into(), resolved: PathBuf::new(), line: 1 },
            400,
        );
        import.threshold = 300;
        let mut nested = Issue::new("check-expression", "k", "m", 1);
        nested.related = vec![Related::inside(PathBuf::from("/p/a.ts"), 1, "checking a.ts")];
        let spans = vec![
            Finding::new(PathBuf::from("/p/a.ts"), FindingKind::Issue(nested), 900),
            issue("/p/a.ts", "check-source-file", "k", 1000),
        ];
        let savings = Savings::of(&result(vec![
            ("depth", deep),
            ("bloat", vec![import, whole]),
            ("hot-spots", spans),
        ]));
        assert!(savings.estimate(0, 0).is_none() && savings.estimate(0, 1).is_some());
        assert!(savings.estimate(1, 0).is_none() && savings.estimate(1, 1).is_some());
        assert!(savings.estimate(2, 0).is_none());
        let span = savings.estimate(2, 1).unwrap();
        assert!((span.per_run.typecheck - 0.8).abs() < 1e-9, "{span:?}");
    }

    #[test]
    fn findings_about_one_thing_share_an_estimate() {
        // Five files importing one declaration tree, and three lint configs
        // with the same kind of issue, each priced on the whole project.
        let mut bloat = Vec::new();
        for file in ["/p/a.ts", "/p/b.ts", "/p/c.ts", "/p/d.ts", "/p/e.ts"] {
            bloat.push(issue(file, "heavy-types", "big", 300_000));
        }
        let lint = vec![
            issue("/p/.oxlintrc.json", "type-aware-unused", "typeAware", 1),
            issue("/p/a/.oxlintrc.json", "type-aware-unused", "typeAware", 1),
            issue("/p/b/.oxlintrc.json", "slow-rule", "import/no-cycle", 1),
        ];
        let savings = Savings::of(&result(vec![("type-bloat", bloat), ("lint-config", lint)]));
        let priced = |r: usize| savings.estimates[r].iter().filter(|e| e.is_some()).count();
        assert_eq!((priced(0), priced(1)), (1, 2));
    }

    #[test]
    fn barrels_cost_more_the_more_files_import_them() {
        let barrel = |importers| {
            let mut f = Finding::new(
                PathBuf::from("/p/index.ts"),
                FindingKind::Barrel(Barrel::new(10, importers)),
                125,
            );
            f.threshold = 25;
            f
        };
        let savings = Savings::of(&result(vec![("barrels", vec![barrel(1), barrel(40)])]));
        let one = savings.estimate(0, 0).unwrap().per_run;
        let many = savings.estimate(0, 1).unwrap().per_run;
        assert!((one.test - 100.0 * TEST_PER_MODULE).abs() < 1e-9, "{one:?}");
        assert!((many.test - 10.0 * one.test).abs() < 1e-9, "{many:?}");
        assert_eq!(one.build, many.build);
    }

    #[test]
    fn huge_values_are_capped() {
        let savings = Savings::of(&result(vec![(
            "complex-types",
            vec![issue("/p/a.ts", "x", "y", usize::MAX)],
        )]));
        assert_eq!(savings.estimate(0, 0).unwrap().per_run.typecheck, MAX_SECONDS_PER_RUN);
    }

    #[test]
    fn health_compares_waste_with_the_project() {
        assert_eq!(Health::from_hours(0.0, 0.0), Health { score: 100, grade: 'A' });
        assert_eq!(Health::from_hours(9.0, 1.0), Health { score: 90, grade: 'A' });
        assert_eq!(Health::from_hours(1.0, 1.0), Health { score: 50, grade: 'F' });
        assert_eq!(Health::from_hours(3.0, 1.0).grade, 'C');
        let clean = Savings::of(&result(vec![]));
        assert_eq!(clean.health.score, 100);
        assert!(clean.is_empty());
    }

    #[test]
    fn every_modeled_rule_has_a_model() {
        let scale = ProjectScale::new(100, 1);
        for rule in MODELED_RULES {
            let mut f = issue("/p/a", "x", "y", 2);
            if *rule == "bloat" {
                f.kind = FindingKind::WholeGraph;
            }
            assert!(model(rule, &f, scale).is_some(), "{rule}");
        }
        assert!(model("not-a-rule", &issue("/p/a", "x", "y", 2), scale).is_none());
    }

    #[test]
    fn hours_read_naturally() {
        assert_eq!(format_hours(0.0001), "<1s");
        assert_eq!(format_hours(0.01), "36s");
        assert_eq!(format_hours(0.99), "59m");
        assert_eq!(format_hours(0.999), "1.0h");
        assert_eq!(format_hours(0.25), "15m");
        assert_eq!(format_hours(1.3), "1.3h");
        assert_eq!(format_hours(42.4), "42h");
    }
}
