//! Options shared by every checker, and the one-time setup they all need:
//! find the root, collect entry files, and build the module graph.

use std::path::{Path, PathBuf};

use dashmap::DashMap;
use tracing::{debug, info, warn};

use crate::{
    CollectorConfig, ConfigFile, Project, ResolutionStats, RulePolicy, Severity, collect_entries,
    collector::{collect_files, is_test_file},
    config::find_git_root,
    constants::is_declaration,
    entry_points::package_entry_points,
    error::{Error, Result},
    graph::{BuildOptions, ModuleGraph, ModuleId, ParsedFile, RawImport},
    parser::parse_file,
    plugins::Plugins,
    report::{CheckResult, Finding, sort_findings},
    resolve,
    settings::{ResolvedRule, file_globs},
};

/// Command-line options every checker accepts. Everything else is set in
/// the config file.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "cli", derive(clap::Args))]
#[non_exhaustive]
pub struct CommonArgs {
    /// Root directory of the project (defaults to the git root of the current directory)
    #[cfg_attr(feature = "cli", arg(long))]
    pub root: Option<PathBuf>,

    /// Config file to use instead of `.oxicleanrc.json` in the root
    #[cfg_attr(feature = "cli", arg(long, short = 'c', value_name = "FILE"))]
    pub config: Option<PathBuf>,
}

impl CommonArgs {
    /// Options for analysing `root`.
    #[must_use]
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: Some(root.into()), ..Self::default() }
    }

    #[must_use]
    pub fn config(mut self, path: impl Into<PathBuf>) -> Self {
        self.config = Some(path.into());
        self
    }

    /// Resolves the root, reads the config file, collects entry files and
    /// builds the module graph.
    ///
    /// # Errors
    /// Fails if the root can't be found or read, the config file is invalid,
    /// or no entry files are found.
    pub fn analyze(&self) -> Result<Analysis> {
        self.analyze_with(BuildOptions::default())
    }

    /// Like [`Self::analyze`], with `extra` options for the graph on top of
    /// the config file's. Unused export checks set
    /// [`BuildOptions::resolve_passive`].
    ///
    /// # Errors
    /// As for [`Self::analyze`].
    pub fn analyze_with(&self, extra: BuildOptions) -> Result<Analysis> {
        self.analyze_from(extra, DefaultEntries::SourceFiles)
    }

    /// Like [`Self::analyze_with`], for the checks of exports. Unless the
    /// config sets `entries`, the entries are the files each package offers
    /// to others (package.json `main`, `exports`, `bin` and `scripts`, and
    /// `index`, `main` and `cli` files, and config files), test files, HTML
    /// pages that load scripts, and declaration files in a tsconfig's
    /// directory, as in knip,
    /// rather than every file in `src`: an entry's exports always count as
    /// used, so with every file an entry nothing would be reported.
    ///
    /// # Errors
    /// As for [`Self::analyze`].
    pub fn analyze_package_entries(&self, extra: BuildOptions) -> Result<Analysis> {
        self.analyze_from(extra, DefaultEntries::PackageEntryPoints)
    }

    /// Resolves the root and reads the config file and the project's
    /// package.json and tsconfig files, without building the module graph,
    /// for checks of the project's manifests.
    ///
    /// # Errors
    /// Fails if the root can't be found or read, or the config file is
    /// invalid.
    pub fn workspace(&self) -> Result<Workspace> {
        let root = match &self.root {
            Some(r) => {
                r.canonicalize().map_err(|source| Error::Root { path: r.clone(), source })?
            }
            None => find_git_root()?,
        };
        info!("Using root directory: {}", root.display());

        let (config, config_path) = ConfigFile::discover(&root, self.config.as_deref())?;
        if let Some(p) = &config_path {
            info!("Using config file: {}", p.display());
        }

        let project = Project::load(&root);
        debug!("Found {} workspace packages", project.workspace_package_count());
        Ok(Workspace { root, project, config, config_path })
    }

    fn analyze_from(&self, extra: BuildOptions, defaults: DefaultEntries) -> Result<Analysis> {
        self.workspace()?.analyze_from(extra, defaults)
    }
}

/// The entries of the checks of exports when the config sets none: each
/// package's entry points, test files, the files plugins make entries (as
/// HTML pages that load scripts and test setup files), and, as in knip, declaration files a
/// tsconfig covers, which declare types for code oxiclean can't see.
fn package_default_entries(
    project: &Project,
    plugins: &Plugins,
    root: &Path,
) -> Result<Vec<PathBuf>> {
    let mut paths = package_entry_points(project);
    debug!("Found {} package entry points", paths.len());
    if paths.is_empty() {
        warn!(
            "No package entry points (package.json main, exports or bin, or an index, main or \
             cli file) found; set \"entries\" in the config file"
        );
    }
    let mut dirs = vec![project.root.as_path()];
    dirs.extend(project.workspace_packages().into_iter().map(|(_, d)| d));
    paths.extend(dirs.into_iter().flat_map(|d| plugins.entries(d)));
    paths.extend(collect_files(root, |rel| {
        is_test_file(rel)
            || (is_declaration(rel) && project.tsconfig_for(&root.join(rel)).is_some())
            || plugins.is_entry(&root.join(rel))
    })?);
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Which files are entries when the config sets no `entries`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DefaultEntries {
    /// Every source file in a `src` directory.
    SourceFiles,
    /// Package entry points, test files and declaration files.
    PackageEntryPoints,
}

/// The prepared input for a checker.
#[derive(Debug)]
#[non_exhaustive]
pub struct Analysis {
    /// Canonical project root.
    pub root: PathBuf,
    pub graph: ModuleGraph,
    /// Entry modules, sorted by path.
    pub entries: Vec<ModuleId>,
    /// How many imports resolved while building the graph.
    pub resolution: ResolutionStats,
    /// tsconfig and workspace information.
    pub project: Project,
    /// The plugins that are on.
    pub plugins: Plugins,
    /// The config file's settings, or the defaults when there is none.
    pub config: ConfigFile,
    /// Where the config file was read from.
    pub config_path: Option<PathBuf>,
}

impl Analysis {
    /// The config file's settings for `rule`, with the rule's defaults
    /// where the file sets none. See [`ConfigFile::policy`].
    ///
    /// # Errors
    /// Fails if the config file has an invalid glob.
    pub fn policy(
        &self,
        rule: &str,
        default_threshold: usize,
        default_severity: Severity,
    ) -> Result<RulePolicy> {
        self.config.policy(rule, default_threshold, default_severity)
    }

    /// Applies `policy` to a checker's candidate findings, dropping those it
    /// disables, ignores or that fall below their file's threshold, and
    /// packages the rest, sorted deterministically, with the statistics of
    /// this analysis.
    #[must_use]
    pub fn finish(&self, policy: &RulePolicy, findings: Vec<Finding>) -> CheckResult {
        CheckResult {
            findings: apply_policy(&self.root, &self.project, policy, findings),
            root: self.root.clone(),
            files_analyzed: self.graph.len(),
            parse_failures: self.graph.failures().to_vec(),
            resolution: self.resolution,
            threshold: policy.base().threshold,
            severity: policy.base().enabled.then_some(policy.base().severity),
            notes: Vec::new(),
        }
    }
}

/// The root, config file and project, read once by
/// [`CommonArgs::workspace`] for every rule of a command. Rules that check
/// only manifests and tsconfig files run on it directly; rules that need a
/// module graph build one from it with [`Self::analyze_with`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Workspace {
    /// Canonical project root.
    pub root: PathBuf,
    /// tsconfig and workspace information.
    pub project: Project,
    /// The config file's settings, or the defaults when there is none.
    pub config: ConfigFile,
    /// Where the config file was read from.
    pub config_path: Option<PathBuf>,
}

impl Workspace {
    /// As [`CommonArgs::analyze_with`], from this workspace.
    ///
    /// # Errors
    /// Fails if the config file has an invalid glob, or no entry files are
    /// found.
    pub fn analyze_with(self, extra: BuildOptions) -> Result<Analysis> {
        self.analyze_from(extra, DefaultEntries::SourceFiles)
    }

    /// As [`CommonArgs::analyze_package_entries`], from this workspace.
    ///
    /// # Errors
    /// As for [`Self::analyze_with`].
    pub fn analyze_package_entries(self, extra: BuildOptions) -> Result<Analysis> {
        self.analyze_from(extra, DefaultEntries::PackageEntryPoints)
    }

    fn analyze_from(self, extra: BuildOptions, defaults: DefaultEntries) -> Result<Analysis> {
        let Self { root, project, config, config_path } = self;
        let plugins = Plugins::detect(&project, &config.plugins);
        debug!("Plugins: {:?}", plugins);

        let mut entry_paths =
            if config.entries.is_empty() && defaults == DefaultEntries::PackageEntryPoints {
                package_default_entries(&project, &plugins, &root)?
            } else {
                collect_entries(&CollectorConfig {
                    root: root.clone(),
                    entry_globs: config.entries.clone(),
                })?
            };

        let ignore = file_globs(&config.ignore_patterns).map_err(|message| Error::Config {
            path: config_path.clone().unwrap_or_default(),
            message,
        })?;
        entry_paths.retain(|p| !ignore.is_match(p.strip_prefix(&root).unwrap_or(p)));

        if entry_paths.is_empty() {
            return Err(Error::NoEntries(root));
        }
        info!("Found {} entry files", entry_paths.len());

        // The graph parses each file exactly once, so only resolution is cached.
        let resolve_cache = DashMap::new();
        let graph = ModuleGraph::build(
            &entry_paths,
            |file: &Path| {
                parse_file(file, &plugins, extra.read_exports, extra.read_members).map(|parsed| {
                    ParsedFile {
                        imports: parsed
                            .specs
                            .into_iter()
                            .map(|s| RawImport { kind: s.kind, request: s.request, line: s.line })
                            .collect(),
                        exports: parsed.exports,
                        uses: parsed.uses,
                        error: parsed.error,
                    }
                })
            },
            |from: &Path, request: &str| {
                resolve(&project, from, request, &resolve_cache).ok().flatten()
            },
            extra
                .follow_dynamic(extra.follow_dynamic || config.include_dynamic)
                .follow_node_modules(extra.follow_node_modules || config.include_node_modules),
        );

        let entries = entry_paths.iter().filter_map(|p| graph.id(p)).collect();
        let resolution = ResolutionStats::from_cache(&resolve_cache);
        Ok(Analysis { root, graph, entries, resolution, project, plugins, config, config_path })
    }

    /// Whether the config file turns `rule` on for any file. See
    /// [`ConfigFile::enabled`].
    #[must_use]
    pub fn enabled(&self, rule: &str) -> bool {
        self.config.enabled(rule)
    }

    /// The config file's settings for `rule`, with the rule's defaults
    /// where the file sets none. See [`ConfigFile::policy`].
    ///
    /// # Errors
    /// Fails if the config file has an invalid glob.
    pub fn policy(
        &self,
        rule: &str,
        default_threshold: usize,
        default_severity: Severity,
    ) -> Result<RulePolicy> {
        self.config.policy(rule, default_threshold, default_severity)
    }

    /// As [`Self::finish`], with `decide` choosing which findings are
    /// reported, and how severely, from each one's file's settings
    /// instead of comparing its value with the threshold: `None` drops a
    /// finding. `decide` sees the findings in the order given, so it can
    /// keep a finding because of one before it.
    #[must_use]
    pub fn finish_with(
        &self,
        policy: &RulePolicy,
        findings: Vec<Finding>,
        files_analyzed: usize,
        decide: impl FnMut(&Finding, &ResolvedRule) -> Option<Severity>,
    ) -> CheckResult {
        CheckResult {
            findings: apply_policy_with(&self.root, &self.project, policy, findings, decide),
            ..self.finish(policy, Vec::new(), files_analyzed)
        }
    }

    /// As [`Analysis::finish`], for a check of `files_analyzed` files.
    #[must_use]
    pub fn finish(
        &self,
        policy: &RulePolicy,
        findings: Vec<Finding>,
        files_analyzed: usize,
    ) -> CheckResult {
        CheckResult {
            findings: apply_policy(&self.root, &self.project, policy, findings),
            root: self.root.clone(),
            files_analyzed,
            parse_failures: Vec::new(),
            resolution: ResolutionStats::default(),
            threshold: policy.base().threshold,
            severity: policy.base().enabled.then_some(policy.base().severity),
            notes: Vec::new(),
        }
    }
}

/// `findings` without those `policy` disables, ignores or that fall below
/// their file's threshold, each with its file's severity and package,
/// sorted.
fn apply_policy(
    root: &Path,
    project: &Project,
    policy: &RulePolicy,
    findings: Vec<Finding>,
) -> Vec<Finding> {
    apply_policy_with(root, project, policy, findings, |f, rule| {
        rule.reports(f.value).then_some(rule.severity)
    })
}

/// `findings` as `decide` keeps them, given each one's file's settings:
/// `None` drops a finding, `Some` keeps it with that severity. Each kept
/// finding gets its file's threshold and package; the result is sorted.
/// `decide` sees the findings in the order given.
fn apply_policy_with(
    root: &Path,
    project: &Project,
    policy: &RulePolicy,
    findings: Vec<Finding>,
    mut decide: impl FnMut(&Finding, &ResolvedRule) -> Option<Severity>,
) -> Vec<Finding> {
    let mut findings: Vec<Finding> = findings
        .into_iter()
        .filter_map(|mut f| {
            let package = project.package_for(&f.file);
            let rel = f.file.strip_prefix(root).unwrap_or(&f.file);
            let rule = policy.resolve(rel, package);
            f.severity = decide(&f, &rule)?;
            f.threshold = rule.threshold;
            f.package = package.map(str::to_string);
            Some(f)
        })
        .collect();
    sort_findings(&mut findings);
    findings
}
