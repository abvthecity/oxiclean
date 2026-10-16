//! Findings and the reporters shared by every checker: terminal text, JSON
//! and SARIF.

use std::{
    collections::BTreeMap,
    io::{self, Write},
    path::{Component, Path, PathBuf},
};

use colored::Colorize;

mod machine;
pub use machine::{OutputFormat, ReportMeta, message, write_json, write_sarif};

use crate::{
    exports::ExportKind,
    fix::Fix,
    graph::ParseFailure,
    resolver::ResolutionStats,
    savings::{ProjectScale, Savings, format_hours},
    settings::{SavingsSettings, Severity},
};

/// What a finding points at.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FindingKind {
    /// A single import in the file.
    Import {
        /// The specifier as written, e.g. `./utils`.
        request: String,
        /// The file the import resolved to.
        resolved: PathBuf,
        /// 1-based line of the import, or 0 when unknown.
        line: u32,
    },
    /// The file's whole import graph.
    WholeGraph,
    /// A circular import that starts in the file.
    Cycle(CycleFinding),
    /// Names the file exports for the same thing.
    DuplicateExport(DuplicateExport),
    /// An export nothing imports.
    UnusedExport(UnusedExport),
    /// A member of a used enum or namespace that nothing uses. The name is
    /// the export and the member's path, as `Fruits.Tropical.mango`.
    UnusedMember(UnusedExport),
    /// A package a package.json lists that nothing uses. The finding's file
    /// is the package.json.
    UnusedDependency(UnusedDependency),
    /// A pnpm catalog entry no package.json refers to with `catalog:`. The
    /// finding's file is pnpm-workspace.yaml.
    UnusedCatalogEntry(UnusedCatalogEntry),
    /// A barrel file: a module that mostly re-exports others. The value is
    /// how many modules importing it pulls in.
    Barrel(Barrel),
    /// A third-party package that pulls in many modules. The finding's file
    /// is the package.json that lists it; the value is the number of
    /// modules importing it pulls in.
    HeavyDependency(HeavyDependency),
    /// A package installed at more than one version. The finding's file is
    /// the lockfile; the value is the number of versions.
    DuplicateVersions(DuplicateVersions),
    /// A problem described in words, as the tsconfig and lint config
    /// checks report: a misconfigured option, a missing project reference.
    Issue(Issue),
}

/// A problem a check describes in words, for [`FindingKind::Issue`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Issue {
    /// What kind of problem it is within its rule, e.g. `missing-reference`.
    pub code: String,
    /// What the finding is about, stable across commits, e.g. the other
    /// package's name. Code scanning tells findings apart by it.
    pub key: String,
    /// One sentence describing it.
    pub message: String,
    /// 1-based line in the finding's file, or 0 for the whole file.
    pub line: u32,
    /// Other places involved, such as the second tsconfig covering a file.
    pub related: Vec<Related>,
    /// How `--fix` resolves it, when that can be done without a person
    /// deciding.
    pub fix: Option<Fix>,
}

impl Issue {
    /// An issue of kind `code` about `key`, described by `message`, at
    /// `line` of the finding's file.
    #[must_use]
    pub fn new(
        code: impl Into<String>,
        key: impl Into<String>,
        message: impl Into<String>,
        line: u32,
    ) -> Self {
        Self {
            code: code.into(),
            key: key.into(),
            message: message.into(),
            line,
            related: Vec::new(),
            fix: None,
        }
    }

    /// The issue with `related` places listed.
    #[must_use]
    pub fn with_related(mut self, related: Vec<Related>) -> Self {
        self.related = related;
        self
    }

    /// The issue with a fix for `--fix`.
    #[must_use]
    pub fn with_fix(mut self, fix: Fix) -> Self {
        self.fix = Some(fix);
        self
    }
}

/// Another place an [`Issue`] involves.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Related {
    /// The other file.
    pub file: PathBuf,
    /// 1-based line, or 0 for the whole file.
    pub line: u32,
    /// What is there, e.g. `also includes it`.
    pub message: String,
}

impl Related {
    /// `message` about `line` of `file`.
    #[must_use]
    pub fn new(file: PathBuf, line: u32, message: impl Into<String>) -> Self {
        Self { file, line, message: message.into() }
    }

    /// The finding this one is part of, at `line` of `file`, such as a span
    /// of a trace inside a longer one, whose time includes it.
    #[must_use]
    pub fn inside(file: PathBuf, line: u32, description: &str) -> Self {
        Self::new(file, line, format!("{INSIDE}{description}"))
    }

    /// Whether this names the finding the issue is part of.
    #[must_use]
    pub fn is_inside(&self) -> bool {
        self.message.starts_with(INSIDE)
    }
}

/// How [`Related::inside`] starts its message.
const INSIDE: &str = "inside ";

/// The versions of a package a lockfile installs, for
/// [`FindingKind::DuplicateVersions`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DuplicateVersions {
    /// The package's name.
    pub name: String,
    /// Each version, lowest first.
    pub versions: Vec<String>,
    /// 1-based line of the package's first entry in the lockfile.
    pub line: u32,
}

impl DuplicateVersions {
    #[must_use]
    pub fn new(name: String, versions: Vec<String>, line: u32) -> Self {
        Self { name, versions, line }
    }
}

/// A package that pulls in many modules, for
/// [`FindingKind::HeavyDependency`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HeavyDependency {
    /// The package's name.
    pub name: String,
    /// 1-based line of the package in the package.json, or 0 when unknown.
    pub line: u32,
    /// How many of the project's files import it.
    pub importers: usize,
}

impl HeavyDependency {
    #[must_use]
    pub fn new(name: String, line: u32, importers: usize) -> Self {
        Self { name, line, importers }
    }
}

/// What a barrel file forwards, for [`FindingKind::Barrel`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Barrel {
    /// How many modules it re-exports from.
    pub reexports: usize,
    /// How many files import it.
    pub importers: usize,
}

impl Barrel {
    #[must_use]
    pub fn new(reexports: usize, importers: usize) -> Self {
        Self { reexports, importers }
    }
}

/// A pnpm catalog entry nothing refers to, for
/// [`FindingKind::UnusedCatalogEntry`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UnusedCatalogEntry {
    /// The package's name.
    pub name: String,
    /// The catalog: `default` for `catalog`, else the name under `catalogs`.
    pub catalog: String,
    /// 1-based line of the entry in pnpm-workspace.yaml.
    pub line: u32,
}

impl UnusedCatalogEntry {
    #[must_use]
    pub fn new(name: String, catalog: String, line: u32) -> Self {
        Self { name, catalog, line }
    }
}

/// A package a package.json lists that nothing uses, for
/// [`FindingKind::UnusedDependency`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UnusedDependency {
    /// The package's name, as `react` or `@types/node`.
    pub name: String,
    /// The package.json field that lists it, as `devDependencies`.
    pub field: String,
    /// 1-based line of the package in the package.json, or 0 when unknown.
    pub line: u32,
}

impl UnusedDependency {
    #[must_use]
    pub fn new(name: String, field: String, line: u32) -> Self {
        Self { name, field, line }
    }
}

/// An export nothing imports, for [`FindingKind::UnusedExport`], or a
/// member nothing uses, for [`FindingKind::UnusedMember`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UnusedExport {
    /// The exported name; `default` for the default export.
    pub name: String,
    /// 1-based line of the name.
    pub line: u32,
    pub kind: ExportKind,
}

impl UnusedExport {
    #[must_use]
    pub fn new(name: String, line: u32, kind: ExportKind) -> Self {
        Self { name, line, kind }
    }
}

/// Names a file exports for the same thing, for
/// [`FindingKind::DuplicateExport`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DuplicateExport {
    /// The original export first, then each name that repeats it.
    pub names: Vec<ExportName>,
}

impl DuplicateExport {
    #[must_use]
    pub fn new(names: Vec<ExportName>) -> Self {
        Self { names }
    }
}

/// An exported name and where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExportName {
    /// The name importers use; `default` for the default export.
    pub name: String,
    /// 1-based line of the name.
    pub line: u32,
}

impl ExportName {
    #[must_use]
    pub fn new(name: String, line: u32) -> Self {
        Self { name, line }
    }
}

/// The imports of a circular import, for [`FindingKind::Cycle`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CycleFinding {
    /// One import per file, in order: the first file's import of the second,
    /// ..., and the last file's import of the first. The first step is in the
    /// finding's file.
    pub steps: Vec<CycleStep>,
    /// Number of files that import each other, directly or indirectly,
    /// together with this cycle's. Larger than the cycle when it is part of a
    /// wider tangle.
    pub group_size: usize,
}

impl CycleFinding {
    #[must_use]
    pub fn new(steps: Vec<CycleStep>, group_size: usize) -> Self {
        Self { steps, group_size }
    }
}

/// One import of a circular import.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CycleStep {
    /// The importing file.
    pub file: PathBuf,
    /// The specifier as written, e.g. `./utils`.
    pub request: String,
    /// 1-based line of the import, or 0 when unknown.
    pub line: u32,
}

impl CycleStep {
    #[must_use]
    pub fn new(file: PathBuf, request: String, line: u32) -> Self {
        Self { file, request, line }
    }
}

/// One threshold violation reported by a checker.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Finding {
    /// Absolute path of the file the finding is about.
    pub file: PathBuf,
    pub kind: FindingKind,
    /// The measured value (module count, depth, ...).
    pub value: usize,
    /// The threshold that applied to this file.
    pub threshold: usize,
    pub severity: Severity,
    /// The workspace package the file belongs to.
    pub package: Option<String>,
}

impl Finding {
    /// A finding with error severity and no threshold or package yet;
    /// [`crate::Analysis::finish`] fills those in from the config.
    #[must_use]
    pub fn new(file: PathBuf, kind: FindingKind, value: usize) -> Self {
        Self { file, kind, value, threshold: 0, severity: Severity::Error, package: None }
    }

    /// 1-based line the finding points at, or 0 for the whole file.
    #[must_use]
    pub fn line(&self) -> u32 {
        match &self.kind {
            FindingKind::Import { line, .. } => *line,
            FindingKind::Cycle(cycle) => cycle.steps.first().map_or(0, |s| s.line),
            FindingKind::DuplicateExport(dup) => dup.names.first().map_or(0, |n| n.line),
            FindingKind::UnusedExport(unused) | FindingKind::UnusedMember(unused) => unused.line,
            FindingKind::UnusedDependency(dep) => dep.line,
            FindingKind::UnusedCatalogEntry(entry) => entry.line,
            FindingKind::HeavyDependency(dep) => dep.line,
            FindingKind::DuplicateVersions(dup) => dup.line,
            FindingKind::Issue(issue) => issue.line,
            FindingKind::Barrel(_) | FindingKind::WholeGraph => 0,
        }
    }
}

/// The output of a checker run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CheckResult {
    /// Findings, sorted by file then by descending value.
    pub findings: Vec<Finding>,
    /// Canonical project root the analysis ran on.
    pub root: PathBuf,
    /// Number of modules in the analysed graph.
    pub files_analyzed: usize,
    /// Files that could not be read or parsed.
    pub parse_failures: Vec<ParseFailure>,
    /// How many imports resolved.
    pub resolution: ResolutionStats,
    /// The rule's threshold outside any override, for the report heading.
    pub threshold: usize,
    /// The rule's severity outside any override, or `None` when the rule
    /// is off there and runs only for the files an override turns it on
    /// for.
    pub severity: Option<Severity>,
}

impl CheckResult {
    /// Number of findings with error severity; a run with any fails.
    #[must_use]
    pub fn error_count(&self) -> usize {
        self.findings.iter().filter(|f| f.severity == Severity::Error).count()
    }
}

/// What one rule of a command found.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RuleResult {
    /// The rule's name in `.oxicleanrc.json`, e.g. `unused-types`.
    pub rule: &'static str,
    /// How its findings are worded.
    pub style: ReportStyle,
    /// What it found; empty when it failed.
    pub result: CheckResult,
    /// Why the rule couldn't run, when it failed. The command's other rules
    /// still run, and the run fails.
    pub error: Option<String>,
}

impl RuleResult {
    #[must_use]
    pub fn new(rule: &'static str, style: ReportStyle, result: CheckResult) -> Self {
        Self { rule, style, result, error: None }
    }

    /// A rule on `root` that couldn't run, because of `error`.
    #[must_use]
    pub fn failed(
        rule: &'static str,
        style: ReportStyle,
        root: PathBuf,
        error: impl Into<String>,
    ) -> Self {
        let result = CheckResult { root, ..CheckResult::default() };
        Self { rule, style, result, error: Some(error.into()) }
    }
}

/// The output of a command: what each of its rules that is on found, or
/// why it couldn't run. Rules the config file turns off don't run and
/// aren't listed.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CommandResult {
    /// Canonical project root the rules ran on.
    pub root: PathBuf,
    /// One entry per rule that is on, in the command's order.
    pub rules: Vec<RuleResult>,
    /// How the team works, from the config file, for estimating the time
    /// findings cost.
    pub team: SavingsSettings,
    /// The project's size, for the same.
    pub scale: ProjectScale,
}

impl CommandResult {
    /// A command on `root` that has run no rule yet.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root, ..Self::default() }
    }

    /// A command on `workspace` that has run no rule yet, with the team
    /// settings and project size the time estimates need.
    #[must_use]
    pub fn of_workspace(workspace: &crate::Workspace) -> Self {
        let project = &workspace.project;
        Self {
            root: workspace.root.clone(),
            rules: Vec::new(),
            team: workspace.config.savings.clone(),
            scale: ProjectScale::new(
                project.source_file_count(),
                project.workspace_package_count(),
            ),
        }
    }

    /// Adds the rules `other` ran, after this one's. Both must have run on
    /// the same root.
    pub fn extend(&mut self, other: CommandResult) {
        self.rules.extend(other.rules);
    }

    /// Adds what `rule` found.
    pub fn push(&mut self, rule: &'static str, style: ReportStyle, result: CheckResult) {
        self.rules.push(RuleResult::new(rule, style, result));
    }

    /// The rules that couldn't run; a run with any fails.
    pub fn failed(&self) -> impl Iterator<Item = &RuleResult> {
        self.rules.iter().filter(|r| r.error.is_some())
    }

    /// Whether the run fails: a rule couldn't run, or a finding with error
    /// severity crossed its threshold. Warnings alone don't fail it.
    #[must_use]
    pub fn fails(&self) -> bool {
        self.error_count() > 0 || self.failed().next().is_some()
    }

    /// Number of findings across every rule.
    #[must_use]
    pub fn finding_count(&self) -> usize {
        self.rules.iter().map(|r| r.result.findings.len()).sum()
    }

    /// Number of findings with error severity across every rule; a run with
    /// any fails.
    #[must_use]
    pub fn error_count(&self) -> usize {
        self.rules.iter().map(|r| r.result.error_count()).sum()
    }

    /// The most files any rule analysed. Rules that share a module graph
    /// analyse the same files, so this is the largest set the command read.
    #[must_use]
    pub fn files_analyzed(&self) -> usize {
        self.rules.iter().map(|r| r.result.files_analyzed).max().unwrap_or(0)
    }

    /// The files any rule could not parse, each once, by path.
    #[must_use]
    pub fn parse_failures(&self) -> Vec<&ParseFailure> {
        let mut failures: BTreeMap<&Path, &ParseFailure> = BTreeMap::new();
        for f in self.rules.iter().flat_map(|r| &r.result.parse_failures) {
            failures.entry(&f.path).or_insert(f);
        }
        failures.into_values().collect()
    }

    /// The import resolution of the rule that resolved the most imports.
    #[must_use]
    pub fn resolution(&self) -> ResolutionStats {
        self.rules
            .iter()
            .map(|r| r.result.resolution)
            .max_by_key(|r| r.resolved + r.unresolved)
            .unwrap_or_default()
    }
}

/// Sorts by file, whole-graph findings first, then by descending value and
/// request, so output never depends on thread scheduling.
pub fn sort_findings(findings: &mut [Finding]) {
    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| is_import(a).cmp(&is_import(b)))
            .then_with(|| b.value.cmp(&a.value))
            .then_with(|| request(a).cmp(request(b)))
    });
}

/// Whether `f` is listed under its file rather than being about the whole
/// file.
fn is_import(f: &Finding) -> bool {
    !matches!(f.kind, FindingKind::WholeGraph)
}

fn request(f: &Finding) -> &str {
    match &f.kind {
        FindingKind::Import { request, .. } => request,
        FindingKind::Cycle(cycle) => cycle.steps.first().map_or("", |s| s.request.as_str()),
        FindingKind::DuplicateExport(dup) => dup.names.first().map_or("", |n| n.name.as_str()),
        FindingKind::UnusedExport(unused) | FindingKind::UnusedMember(unused) => &unused.name,
        FindingKind::UnusedDependency(dep) => &dep.name,
        FindingKind::UnusedCatalogEntry(entry) => &entry.name,
        FindingKind::HeavyDependency(dep) => &dep.name,
        FindingKind::DuplicateVersions(dup) => &dup.name,
        FindingKind::Issue(issue) => &issue.key,
        FindingKind::Barrel(_) | FindingKind::WholeGraph => "",
    }
}

/// How a finding listed under its file is introduced, e.g. `import './a'`.
fn label(f: &Finding) -> String {
    match &f.kind {
        FindingKind::DuplicateExport(_) | FindingKind::UnusedExport(_) => {
            format!("export '{}'", request(f))
        }
        FindingKind::UnusedMember(_) => format!("member '{}'", request(f)),
        FindingKind::UnusedDependency(_) => format!("package '{}'", request(f)),
        FindingKind::UnusedCatalogEntry(entry) => {
            format!("catalog entry '{}' ({})", entry.name, entry.catalog)
        }
        FindingKind::Barrel(barrel) => format!(
            "barrel re-exporting {} modules, imported by {} files",
            barrel.reexports, barrel.importers
        ),
        FindingKind::HeavyDependency(dep) => {
            format!("package '{}', imported by {} files", dep.name, dep.importers)
        }
        FindingKind::DuplicateVersions(dup) => {
            format!("package '{}' ({})", dup.name, dup.versions.join(", "))
        }
        FindingKind::Issue(issue) => match issue.line {
            0 => issue.message.clone(),
            line => format!("line {line}: {}", issue.message),
        },
        _ => format!("import '{}'", request(f)),
    }
}

/// Lines printed under a finding: the files of a cycle, or the names that
/// repeat an export.
fn details(f: &Finding, ctx: ReportContext<'_>) -> Vec<String> {
    if let FindingKind::DuplicateExport(dup) = &f.kind {
        let file = ctx.display(&f.file);
        return dup
            .names
            .iter()
            .skip(1)
            .map(|n| format!("{file}:{} also exports it as '{}'", n.line, n.name))
            .collect();
    }
    if let FindingKind::Issue(issue) = &f.kind {
        let mut lines: Vec<String> = issue
            .related
            .iter()
            .map(|r| match r.line {
                0 => format!("{}: {}", ctx.display(&r.file), r.message),
                line => format!("{}:{line}: {}", ctx.display(&r.file), r.message),
            })
            .collect();
        if let Some(fix) = &issue.fix {
            lines.push(format!("fixable with --fix: {}", fix.description));
        }
        return lines;
    }
    let FindingKind::Cycle(cycle) = &f.kind else { return Vec::new() };
    let mut lines: Vec<String> = cycle
        .steps
        .iter()
        .map(|s| {
            let file = ctx.display(&s.file);
            match s.line {
                0 => format!("{file} imports '{}'", s.request),
                line => format!("{file}:{line} imports '{}'", s.request),
            }
        })
        .collect();
    if cycle.group_size > cycle.steps.len() {
        lines
            .push(format!("part of a tangle of {} files that import each other", cycle.group_size));
    }
    lines
}

/// Wording that differs between checkers.
#[derive(Debug, Clone, Copy)]
pub struct ReportStyle {
    /// One line describing what the rule flags, for SARIF rule metadata.
    pub description: &'static str,
    /// e.g. "Import bloat detected".
    pub heading: &'static str,
    /// Unit of the threshold in the heading, e.g. "modules". Empty to leave
    /// the threshold out, for checks that report every finding.
    pub threshold_unit: &'static str,
    /// Renders a value next to an import, e.g. "123 modules" or "depth: 4".
    pub value_label: fn(usize) -> String,
    /// e.g. "Maximum bloat". Empty to leave the maximum out of the summary,
    /// for checks whose findings all have the same value.
    pub max_label: &'static str,
    /// Unit after the maximum, e.g. "modules".
    pub max_unit: &'static str,
    /// Printed when nothing crossed the threshold.
    pub ok_message: &'static str,
}

/// Paths the reporter displays relative to.
#[derive(Debug, Clone, Copy)]
pub struct ReportContext<'a> {
    /// Project root. Shown when a path can't be made relative to `cwd`.
    pub root: &'a Path,
    /// Directory paths are shown relative to, so they are clickable.
    pub cwd: &'a Path,
}

impl ReportContext<'_> {
    /// `path` as printed: relative to `cwd` when possible, and with `/`
    /// separators on every platform, as in JSON and SARIF output.
    fn display(&self, path: &Path) -> String {
        let shown = make_relative(path, self.cwd)
            .or_else(|| path.strip_prefix(self.root).ok().map(Path::to_path_buf))
            .unwrap_or_else(|| path.to_path_buf());
        let shown = shown.to_string_lossy();
        if std::path::MAIN_SEPARATOR == '\\' {
            shown.replace('\\', "/")
        } else {
            shown.into_owned()
        }
    }
}

/// Prints the success line.
///
/// # Errors
/// Returns any error from writing to `writer`.
pub fn print_ok<W: Write>(writer: &mut W, style: &ReportStyle, threshold: usize) -> io::Result<()> {
    if style.threshold_unit.is_empty() {
        return writeln!(writer, "{} {}", "✓".green().bold(), style.ok_message);
    }
    writeln!(writer, "{} {} Threshold: {threshold}", "✓".green().bold(), style.ok_message)
}

/// Prints findings grouped by file, worst file first, followed by a summary.
///
/// # Errors
/// Returns any error from writing to `writer`.
pub fn print_findings<W: Write>(
    writer: &mut W,
    findings: &[Finding],
    style: &ReportStyle,
    threshold: usize,
    ctx: ReportContext<'_>,
) -> io::Result<()> {
    if style.threshold_unit.is_empty() {
        writeln!(writer, "{} {}\n", "⚠".yellow().bold(), style.heading)?;
    } else {
        writeln!(
            writer,
            "{} {} (threshold: {} {})\n",
            "⚠".yellow().bold(),
            style.heading,
            threshold.to_string().yellow(),
            style.threshold_unit,
        )?;
    }

    let mut by_file: BTreeMap<&Path, Vec<&Finding>> = BTreeMap::new();
    for f in findings {
        by_file.entry(&f.file).or_default().push(f);
    }
    let mut files: Vec<(&Path, Vec<&Finding>)> = by_file.into_iter().collect();
    files.sort_by_key(|(path, fs)| (std::cmp::Reverse(fs.iter().map(|f| f.value).max()), *path));

    for (path, mut file_findings) in files {
        let display_path = ctx.display(path);
        match file_findings.iter().find(|f| f.kind == FindingKind::WholeGraph) {
            Some(whole) => writeln!(
                writer,
                "{} ({}){}",
                display_path.blue(),
                (style.value_label)(whole.value).red().bold(),
                warn_tag(whole)
            )?,
            None => writeln!(writer, "{}", display_path.blue().bold())?,
        }

        file_findings.retain(|f| is_import(f));
        file_findings.sort_by_key(|f| (std::cmp::Reverse(f.value), request(f)));
        let last = file_findings.len().saturating_sub(1);
        for (idx, f) in file_findings.iter().enumerate() {
            let prefix = if idx == last { "└──" } else { "├──" };
            writeln!(
                writer,
                "{}  {} ({}){}",
                prefix.dimmed(),
                label(f),
                (style.value_label)(f.value).red(),
                warn_tag(f)
            )?;
            let indent = if idx == last { "   " } else { "│  " };
            for line in details(f, ctx) {
                writeln!(writer, "{}     {}", indent.dimmed(), line.dimmed())?;
            }
        }
        writeln!(writer)?;
    }

    print_summary(writer, findings, style, ctx)
}

/// Marks findings that don't fail the run.
fn warn_tag(f: &Finding) -> String {
    match f.severity {
        Severity::Warn => format!(" {}", "[warn]".yellow()),
        Severity::Error => String::new(),
    }
}

fn print_summary<W: Write>(
    writer: &mut W,
    findings: &[Finding],
    style: &ReportStyle,
    ctx: ReportContext<'_>,
) -> io::Result<()> {
    let mut violations: Vec<&Finding> = findings.iter().filter(|f| is_import(f)).collect();
    if violations.is_empty() {
        return Ok(());
    }
    violations.sort_by_key(|f| (std::cmp::Reverse(f.value), &f.file, request(f)));
    let max = violations[0].value;

    writeln!(writer, "{}", "─".repeat(60).dimmed())?;
    writeln!(writer, "{}", "Summary".bold())?;
    writeln!(writer, "  Total violations: {}", violations.len().to_string().yellow().bold())?;
    let warnings = violations.iter().filter(|f| f.severity == Severity::Warn).count();
    if warnings > 0 {
        writeln!(writer, "  Warnings (don't fail the run): {}", warnings.to_string().yellow())?;
    }
    if !style.max_label.is_empty() {
        writeln!(
            writer,
            "  {}: {} {}",
            style.max_label,
            max.to_string().red().bold(),
            style.max_unit
        )?;
    }

    let top = &violations[..violations.len().min(5)];
    writeln!(writer, "  Top {} offenders:", top.len())?;
    for (idx, f) in top.iter().enumerate() {
        writeln!(
            writer,
            "    {}. {} ({}) - {}",
            idx + 1,
            label(f),
            (style.value_label)(f.value).red(),
            ctx.display(&f.file).blue()
        )?;
    }
    Ok(())
}

/// Prints what every rule of `command` found: a section for each rule with
/// findings, then the success line of each rule without, then a line for
/// each rule that couldn't run, then the closing lines.
///
/// # Errors
/// Returns any error from writing to `writer`.
pub fn print_command<W: Write>(
    writer: &mut W,
    command: &str,
    result: &CommandResult,
    elapsed_ms: u128,
    threads: usize,
    ctx: ReportContext<'_>,
) -> io::Result<()> {
    let (failed, ran): (Vec<&RuleResult>, Vec<&RuleResult>) =
        result.rules.iter().partition(|r| r.error.is_some());
    let (found, clean): (Vec<&RuleResult>, Vec<&RuleResult>) =
        ran.into_iter().partition(|r| !r.result.findings.is_empty());
    for (i, r) in found.iter().enumerate() {
        if i > 0 {
            writeln!(writer)?;
        }
        print_findings(writer, &r.result.findings, &r.style, r.result.threshold, ctx)?;
    }
    if !found.is_empty() && !clean.is_empty() {
        writeln!(writer)?;
    }
    for r in &clean {
        print_ok(writer, &r.style, r.result.threshold)?;
    }
    if !failed.is_empty() && !(found.is_empty() && clean.is_empty()) {
        writeln!(writer)?;
    }
    for r in failed {
        let error = r.error.as_deref().unwrap_or_default();
        writeln!(writer, "{} {} couldn't run: {error}", "✗".red().bold(), r.rule)?;
    }
    if result.rules.is_empty() {
        writeln!(
            writer,
            "{} The config file turns off every rule {command} runs.",
            "⚠".yellow().bold()
        )?;
    } else {
        writeln!(writer)?;
        print_savings(writer, result, &Savings::of(result), COMMAND_TOP, ctx)?;
    }
    print_finished(writer, result, elapsed_ms, threads, ctx)
}

/// How many findings a command's report ranks by the time they cost.
const COMMAND_TOP: usize = 5;
/// How many the `health` report ranks.
const HEALTH_TOP: usize = 15;

/// Prints the health score, what the findings cost a week, and the `top`
/// findings that cost the most.
///
/// # Errors
/// Returns any error from writing to `writer`.
pub fn print_savings<W: Write>(
    writer: &mut W,
    result: &CommandResult,
    savings: &Savings,
    top: usize,
    ctx: ReportContext<'_>,
) -> io::Result<()> {
    let team = &savings.team;
    let health = savings.health;
    let score = format!("{}/100 ({})", health.score, health.grade);
    let score = match health.grade {
        'A' | 'B' => score.green().bold(),
        'C' | 'D' => score.yellow().bold(),
        _ => score.red().bold(),
    };
    writeln!(writer, "{} {score}", "Health:".bold())?;
    if savings.is_empty() {
        return writeln!(writer, "  Nothing found costs build, test or editor time.");
    }
    writeln!(
        writer,
        "  Fixing these findings saves about {} of developer time and {} of CI time a week, \
         for {} developers and {} CI runs a day (`savings` in .oxicleanrc.json).",
        format_hours(savings.total.developer_hours).cyan(),
        format_hours(savings.total.ci_hours).cyan(),
        team.developers,
        team.ci_runs_per_day,
    )?;
    let shown = savings.ranked.len().min(top);
    writeln!(writer, "  Biggest wins:")?;
    for (rank, &(r, i)) in savings.ranked.iter().take(top).enumerate() {
        let (rule, f) = (&result.rules[r], &result.rules[r].result.findings[i]);
        let Some(e) = savings.estimate(r, i) else { continue };
        let at = match f.line() {
            0 => ctx.display(&f.file),
            line => format!("{}:{line}", ctx.display(&f.file)),
        };
        writeln!(
            writer,
            "    {}. {} dev, {} CI a week  {}  {}",
            rank + 1,
            format_hours(e.developer_hours).cyan(),
            format_hours(e.ci_hours).cyan(),
            rule.rule.bold(),
            at.blue(),
        )?;
        writeln!(writer, "       {}", shorten(&short_label(f, &rule.style), 100).dimmed())?;
    }
    if savings.ranked.len() > shown {
        writeln!(
            writer,
            "    ... and {} more (--format json lists every estimate)",
            savings.ranked.len() - shown
        )?;
    }
    Ok(())
}

/// Prints the report of `oxiclean health`: the score, what each rule's
/// findings cost, the findings that cost the most, and the rules that
/// couldn't run.
///
/// # Errors
/// Returns any error from writing to `writer`.
pub fn print_health<W: Write>(
    writer: &mut W,
    result: &CommandResult,
    elapsed_ms: u128,
    threads: usize,
    ctx: ReportContext<'_>,
) -> io::Result<()> {
    let savings = Savings::of(result);
    print_savings(writer, result, &savings, HEALTH_TOP, ctx)?;
    if !savings.by_rule.is_empty() {
        writeln!(writer, "  By rule (a week):")?;
        let width = savings.by_rule.iter().map(|r| r.rule.len()).max().unwrap_or(0);
        for r in &savings.by_rule {
            writeln!(
                writer,
                "    {:width$}  {:>4} dev  {:>4} CI  {}",
                r.rule,
                format_hours(r.developer_hours),
                format_hours(r.ci_hours),
                count(r.findings, "finding").dimmed(),
            )?;
        }
        writeln!(
            writer,
            "  Run the command of a rule to see every finding, e.g. `oxiclean typecheck`."
        )?;
    }
    let failed: Vec<&RuleResult> = result.failed().collect();
    if !failed.is_empty() {
        writeln!(writer)?;
    }
    for r in failed {
        let error = r.error.as_deref().unwrap_or_default();
        writeln!(writer, "{} {} couldn't run: {error}", "✗".red().bold(), r.rule)?;
    }
    print_finished(writer, result, elapsed_ms, threads, ctx)
}

/// One line saying what `f` is, for the ranking.
fn short_label(f: &Finding, style: &ReportStyle) -> String {
    match &f.kind {
        FindingKind::Issue(issue) => issue.message.clone(),
        FindingKind::WholeGraph => format!("entry pulls in {} modules", f.value),
        // Only depth ranks imports: bloat counts an entry's whole graph.
        FindingKind::Import { .. } => format!("{} ({})", label(f), (style.value_label)(f.value)),
        _ => label(f),
    }
}

/// `text` cut to `max` characters, with an ellipsis when cut.
fn shorten(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

/// `n` and `noun`, plural unless `n` is 1.
fn count(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {noun}") } else { format!("{n} {noun}s") }
}

/// Prints the closing line, plus a note about files that could not be parsed
/// and how many imports resolved.
///
/// # Errors
/// Returns any error from writing to `writer`.
pub fn print_finished<W: Write>(
    writer: &mut W,
    result: &CommandResult,
    elapsed_ms: u128,
    threads: usize,
    ctx: ReportContext<'_>,
) -> io::Result<()> {
    let failures = result.parse_failures();
    if !failures.is_empty() {
        writeln!(
            writer,
            "\n{} {} file(s) could not be parsed; their imports were skipped:",
            "⚠".yellow(),
            failures.len().to_string().yellow()
        )?;
        for failure in failures.iter().take(5) {
            writeln!(writer, "    {}", ctx.display(&failure.path).dimmed())?;
        }
        if failures.len() > 5 {
            writeln!(writer, "    ... (set RUST_LOG=warn for the full list)")?;
        }
    }
    writeln!(
        writer,
        "\n{} Finished in {}ms on {} files (using {} threads).",
        "●".bright_blue(),
        elapsed_ms.to_string().cyan(),
        result.files_analyzed().to_string().cyan(),
        threads.to_string().cyan()
    )?;
    if result.rules.len() > 1 {
        let errors = result.error_count();
        let names: Vec<&str> = result.rules.iter().map(|r| r.rule).collect();
        let failed = result.failed().count();
        let failed = if failed == 0 { String::new() } else { format!(", {failed} couldn't run") };
        writeln!(
            writer,
            "{} Ran {} ({}): {}, {}{}.",
            "●".bright_blue(),
            count(result.rules.len(), "rule"),
            names.join(", "),
            count(errors, "error").cyan(),
            count(result.finding_count() - errors, "warning").cyan(),
            failed.red()
        )?;
    }

    let resolution = result.resolution();
    // Checks of config files resolve no imports.
    if resolution.resolved + resolution.unresolved == 0 {
        return Ok(());
    }
    writeln!(
        writer,
        "{} Resolved {} of {} imports ({}%), {} built-ins skipped.",
        "●".bright_blue(),
        resolution.resolved.to_string().cyan(),
        (resolution.resolved + resolution.unresolved).to_string().cyan(),
        format!("{:.1}", resolution.resolved_percent()).cyan(),
        resolution.builtin.to_string().cyan()
    )
}

/// Builds a relative path from `base` to `target`, or `None` when they share
/// no root (e.g. different Windows drives).
#[must_use]
pub fn make_relative(target: &Path, base: &Path) -> Option<PathBuf> {
    let mut target_components = target.components().peekable();
    let mut base_components = base.components().peekable();

    if target_components.peek() != base_components.peek() {
        return None;
    }
    while let (Some(t), Some(b)) = (target_components.peek(), base_components.peek()) {
        if t != b {
            break;
        }
        target_components.next();
        base_components.next();
    }

    let mut result = PathBuf::new();
    for _ in base_components {
        result.push("..");
    }
    for component in target_components {
        match component {
            Component::Normal(p) => result.push(p),
            Component::ParentDir => result.push(".."),
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    if result.as_os_str().is_empty() { Some(PathBuf::from(".")) } else { Some(result) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_relative_same_dir() {
        let r = make_relative(Path::new("/project/src/file.ts"), Path::new("/project/src"));
        assert_eq!(r, Some(PathBuf::from("file.ts")));
    }

    #[test]
    fn make_relative_child_dir() {
        let r = make_relative(
            Path::new("/project/src/components/Button.tsx"),
            Path::new("/project/src"),
        );
        assert_eq!(r, Some(PathBuf::from("components/Button.tsx")));
    }

    #[test]
    fn make_relative_parent_dir() {
        let r =
            make_relative(Path::new("/project/src/file.ts"), Path::new("/project/src/components"));
        assert_eq!(r, Some(PathBuf::from("../file.ts")));
    }

    #[test]
    fn make_relative_sibling_dir() {
        let r =
            make_relative(Path::new("/project/apps/web/index.ts"), Path::new("/project/apps/api"));
        assert_eq!(r, Some(PathBuf::from("../web/index.ts")));
    }

    #[test]
    fn make_relative_same_path() {
        let r = make_relative(Path::new("/project/src"), Path::new("/project/src"));
        assert_eq!(r, Some(PathBuf::from(".")));
    }

    #[test]
    fn make_relative_target_is_ancestor() {
        let r = make_relative(Path::new("/project"), Path::new("/project/src/a"));
        assert_eq!(r, Some(PathBuf::from("../..")));
    }

    #[test]
    fn make_relative_multiple_levels_up() {
        let r = make_relative(Path::new("/project/file.ts"), Path::new("/project/apps/web/src"));
        assert_eq!(r, Some(PathBuf::from("../../../file.ts")));
    }

    #[test]
    fn make_relative_needs_shared_root() {
        assert_eq!(make_relative(Path::new("a/b"), Path::new("/a")), None);
    }

    fn import(file: &str, request: &str, value: usize) -> Finding {
        Finding::new(
            PathBuf::from(file),
            FindingKind::Import {
                request: request.into(),
                resolved: PathBuf::from(request),
                line: 1,
            },
            value,
        )
    }

    const STYLE: ReportStyle = ReportStyle {
        description: "Imports that pull in too many modules",
        heading: "Import bloat detected",
        threshold_unit: "modules",
        value_label: |n| format!("{n} modules"),
        max_label: "Maximum bloat",
        max_unit: "modules",
        ok_message: "No bloat detected.",
    };

    #[test]
    fn findings_are_sorted_deterministically() {
        let mut fs = vec![
            import("/p/b.ts", "./x", 5),
            Finding::new("/p/a.ts".into(), FindingKind::WholeGraph, 9),
            import("/p/a.ts", "./y", 3),
            import("/p/a.ts", "./z", 7),
        ];
        sort_findings(&mut fs);
        let order: Vec<(String, usize)> =
            fs.iter().map(|f| (f.file.display().to_string(), f.value)).collect();
        assert_eq!(
            order,
            vec![
                ("/p/a.ts".into(), 9),
                ("/p/a.ts".into(), 7),
                ("/p/a.ts".into(), 3),
                ("/p/b.ts".into(), 5)
            ]
        );
    }

    #[test]
    fn report_lists_worst_file_first_with_summary() {
        colored::control::set_override(false);
        let fs = vec![import("/p/src/a.ts", "./small", 3), import("/p/src/b.ts", "./big", 10)];
        let mut out = Vec::new();
        let ctx = ReportContext { root: Path::new("/p"), cwd: Path::new("/p") };
        print_findings(&mut out, &fs, &STYLE, 2, ctx).unwrap();
        let out = String::from_utf8(out).unwrap();
        // Paths are printed with `/` on every platform.
        let b = out.find("src/b.ts").unwrap();
        let a = out.find("src/a.ts").unwrap();
        assert!(b < a, "{out}");
        assert!(out.contains("Total violations: 2"), "{out}");
        assert!(out.contains("Maximum bloat: 10 modules"), "{out}");
        assert!(out.contains("1. import './big' (10 modules) - src/b.ts"), "{out}");
    }

    fn failure(path: &str, message: &str) -> ParseFailure {
        ParseFailure { path: PathBuf::from(path), message: message.into() }
    }

    fn stats(resolved: usize, unresolved: usize, builtin: usize) -> ResolutionStats {
        ResolutionStats { resolved, unresolved, builtin }
    }

    #[test]
    fn command_results_combine_their_rules() {
        let empty = CommandResult::new(PathBuf::from("/p"));
        assert_eq!((empty.finding_count(), empty.error_count(), empty.files_analyzed()), (0, 0, 0));
        assert!(empty.parse_failures().is_empty() && !empty.fails());
        assert_eq!(empty.resolution(), ResolutionStats::default());

        let mut result = CommandResult::new(PathBuf::from("/p"));
        result.push(
            "a",
            STYLE,
            CheckResult {
                files_analyzed: 4,
                parse_failures: vec![failure("/p/x.ts", "first"), failure("/p/y.ts", "y")],
                resolution: stats(3, 1, 9),
                ..CheckResult::default()
            },
        );
        result.push(
            "b",
            STYLE,
            CheckResult {
                files_analyzed: 7,
                // Rules that share a graph see the same failures.
                parse_failures: vec![failure("/p/x.ts", "second"), failure("/p/a.ts", "a")],
                // More imports in all, though fewer resolved.
                resolution: stats(2, 5, 0),
                ..CheckResult::default()
            },
        );
        result.push("c", STYLE, CheckResult::default());
        assert_eq!(result.files_analyzed(), 7);
        let failures: Vec<(&str, &str)> = result
            .parse_failures()
            .into_iter()
            .map(|f| (f.path.to_str().unwrap(), f.message.as_str()))
            .collect();
        assert_eq!(failures, [("/p/a.ts", "a"), ("/p/x.ts", "first"), ("/p/y.ts", "y")]);
        assert_eq!(result.resolution(), stats(2, 5, 0));
        assert!(!result.fails());
    }

    #[test]
    fn command_report_lists_findings_then_clean_then_failed_rules() {
        colored::control::set_override(false);
        let clean = ReportStyle { ok_message: "Nothing to see.", ..STYLE };
        let mut result = CommandResult::new(PathBuf::from("/p"));
        let mut warn = import("/p/src/a.ts", "./warned", 4);
        warn.severity = Severity::Warn;
        let findings = vec![import("/p/src/b.ts", "./big", 10), warn];
        result.push(
            "bloat",
            STYLE,
            CheckResult { findings, threshold: 2, ..CheckResult::default() },
        );
        result.push("clean", clean, CheckResult { threshold: 5, ..CheckResult::default() });
        result.rules.push(RuleResult::failed("broken", STYLE, "/p".into(), "no entry files found"));
        let other = vec![import("/p/src/c.ts", "./other", 3)];
        result.push(
            "other",
            STYLE,
            CheckResult { findings: other, threshold: 3, ..CheckResult::default() },
        );

        let mut out = Vec::new();
        let ctx = ReportContext { root: Path::new("/p"), cwd: Path::new("/p") };
        print_command(&mut out, "mixed", &result, 7, 2, ctx).unwrap();
        let out = String::from_utf8(out).unwrap();
        let at = |needle: &str| out.find(needle).unwrap_or_else(|| panic!("{needle}: {out}"));
        assert!(at("import './big'") < at("import './other'"), "{out}");
        assert!(at("import './other'") < at("Nothing to see. Threshold: 5"), "{out}");
        assert!(at("Nothing to see.") < at("✗ broken couldn't run: no entry files found"), "{out}");
        assert_eq!(out.matches("Import bloat detected").count(), 2, "{out}");
        assert!(!out.contains("No bloat detected."), "only clean rules print the ok line: {out}");
        assert!(
            out.contains(
                "Ran 4 rules (bloat, clean, broken, other): 2 errors, 1 warning, 1 couldn't run."
            ),
            "{out}"
        );
        assert!(!out.contains("turns off every rule"), "{out}");
        assert!(result.fails());

        let mut out = Vec::new();
        print_command(&mut out, "mixed", &CommandResult::new("/p".into()), 7, 2, ctx).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("The config file turns off every rule mixed runs."), "{out}");
        assert!(!out.contains("Ran "), "{out}");
    }
}
