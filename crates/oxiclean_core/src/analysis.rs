//! Options shared by every checker, and the one-time setup they all need:
//! find the root, collect entry files, and build the module graph.

use std::path::{Path, PathBuf};

use dashmap::DashMap;
use tracing::{debug, info};

use crate::{
    CollectorConfig, ConfigFile, Project, ResolutionStats, RulePolicy, collect_entries,
    config::find_git_root,
    error::{Error, Result},
    graph::{BuildOptions, ModuleGraph, ModuleId, ParsedFile, RawImport},
    parse_imports,
    report::{CheckResult, Finding, sort_findings},
    resolve,
    settings::file_globs,
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

        let mut entry_paths = collect_entries(&CollectorConfig {
            root: root.clone(),
            entry_globs: config.entries.clone(),
        })?;

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
                parse_imports(file).map(|parsed| ParsedFile {
                    imports: parsed
                        .specs
                        .into_iter()
                        .map(|s| RawImport { kind: s.kind, request: s.request, line: s.line })
                        .collect(),
                    exports: parsed.exports,
                    error: parsed.error,
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
        Ok(Analysis { root, graph, entries, resolution, project, config, config_path })
    }
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
    /// The config file's settings, or the defaults when there is none.
    pub config: ConfigFile,
    /// Where the config file was read from.
    pub config_path: Option<PathBuf>,
}

impl Analysis {
    /// The config file's settings for `rule`.
    ///
    /// # Errors
    /// Fails if the config file has an invalid glob.
    pub fn policy(&self, rule: &str, default_threshold: usize) -> Result<RulePolicy> {
        self.config.policy(rule, default_threshold)
    }

    /// Applies `policy` to a checker's candidate findings, dropping those it
    /// disables, ignores or that fall below their file's threshold, and
    /// packages the rest, sorted deterministically, with the statistics of
    /// this analysis.
    #[must_use]
    pub fn finish(&self, policy: &RulePolicy, findings: Vec<Finding>) -> CheckResult {
        let mut findings: Vec<Finding> = findings
            .into_iter()
            .filter_map(|mut f| {
                let package = self.project.package_for(&f.file);
                let rel = f.file.strip_prefix(&self.root).unwrap_or(&f.file);
                let rule = policy.resolve(rel, package);
                if !rule.reports(f.value) {
                    return None;
                }
                f.threshold = rule.threshold;
                f.severity = rule.severity;
                f.package = package.map(str::to_string);
                Some(f)
            })
            .collect();
        sort_findings(&mut findings);
        CheckResult {
            findings,
            root: self.root.clone(),
            files_analyzed: self.graph.len(),
            parse_failures: self.graph.failures().to_vec(),
            resolution: self.resolution,
            threshold: policy.base().threshold,
        }
    }
}
