//! Options shared by every checker, and the one-time setup they all need:
//! find the root, collect entry files, and build the module graph.

use std::path::{Path, PathBuf};

use dashmap::DashMap;
use tracing::{debug, info};

use crate::{
    CollectorConfig, ConfigFile, Project, ResolutionStats, RulePolicy, SpecKind, collect_entries,
    config::find_git_root,
    error::{Error, Result},
    graph::{BuildOptions, ModuleGraph, ModuleId, ParsedFile, RawImport},
    parse_imports,
    report::{CheckResult, Finding, sort_findings},
    resolve,
    settings::{file_globs, name_globs},
};

/// Command-line options every checker accepts.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "cli", derive(clap::Args))]
#[non_exhaustive]
pub struct CommonArgs {
    /// Root directory of the project (defaults to the git root of the current directory)
    #[cfg_attr(feature = "cli", arg(long))]
    pub root: Option<PathBuf>,

    /// Only treat files matching this glob as entries, relative to the root
    /// (e.g. "src/pages/**/*.tsx"). A pattern without glob characters matches
    /// any path containing it. Repeat to give several. Without it, every
    /// JS/TS file in a `src` directory is an entry.
    #[cfg_attr(feature = "cli", arg(long = "entry-glob", value_name = "GLOB"))]
    pub entry_globs: Vec<String>,

    /// Also follow dynamic `import()` calls. They are skipped by default
    /// because lazily loaded modules are split into separate chunks.
    #[cfg_attr(feature = "cli", arg(long))]
    pub include_dynamic: bool,

    /// Config file to use instead of `oxiclean.toml` in the root
    #[cfg_attr(feature = "cli", arg(long, value_name = "FILE"))]
    pub config: Option<PathBuf>,

    /// Only analyse entries in this workspace package. Globs such as
    /// "@scope/*" are allowed. Repeat to give several.
    #[cfg_attr(feature = "cli", arg(long = "package", value_name = "NAME"))]
    pub packages: Vec<String>,
}

impl CommonArgs {
    /// Options for analysing `root`.
    #[must_use]
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: Some(root.into()), ..Self::default() }
    }

    #[must_use]
    pub fn entry_glob(mut self, glob: impl Into<String>) -> Self {
        self.entry_globs.push(glob.into());
        self
    }

    #[must_use]
    pub fn include_dynamic(mut self, yes: bool) -> Self {
        self.include_dynamic = yes;
        self
    }

    #[must_use]
    pub fn config(mut self, path: impl Into<PathBuf>) -> Self {
        self.config = Some(path.into());
        self
    }

    #[must_use]
    pub fn package(mut self, name: impl Into<String>) -> Self {
        self.packages.push(name.into());
        self
    }

    /// Resolves the root, collects entry files and builds the module graph.
    ///
    /// # Errors
    /// Fails if the root can't be found or read, the config file is invalid,
    /// a glob is invalid, no package matches `--package`, or no entry files
    /// are found.
    pub fn analyze(&self) -> Result<Analysis> {
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

        // Entry globs on the command line replace the config file's.
        let entry_globs =
            if self.entry_globs.is_empty() { &config.entry_globs } else { &self.entry_globs };
        let mut entry_paths = collect_entries(&CollectorConfig {
            root: root.clone(),
            entry_globs: entry_globs.clone(),
        })?;

        let ignore = file_globs(&config.ignore).map_err(|message| Error::Config {
            path: config_path.clone().unwrap_or_default(),
            message,
        })?;
        entry_paths.retain(|p| !ignore.is_match(p.strip_prefix(&root).unwrap_or(p)));

        if !self.packages.is_empty() {
            let dirs = package_dirs(&project, &self.packages)?;
            entry_paths.retain(|p| dirs.iter().any(|d| p.starts_with(d)));
        }
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
                        .map(|s| RawImport {
                            dynamic: matches!(s.kind, SpecKind::Dynamic),
                            request: s.request,
                            line: s.line,
                        })
                        .collect(),
                    error: parsed.error,
                })
            },
            |from: &Path, request: &str| {
                resolve(&project, from, request, &resolve_cache).ok().flatten()
            },
            BuildOptions::default()
                .follow_dynamic(self.include_dynamic || config.include_dynamic == Some(true)),
        );

        let entries = entry_paths.iter().filter_map(|p| graph.id(p)).collect();
        let resolution = ResolutionStats::from_cache(&resolve_cache);
        Ok(Analysis { root, graph, entries, resolution, project, config, config_path })
    }
}

/// Directories of the workspace packages matching `patterns`.
fn package_dirs(project: &Project, patterns: &[String]) -> Result<Vec<PathBuf>> {
    let globs = name_globs(patterns)
        .map_err(|message| Error::Config { path: PathBuf::from("--package"), message })?;
    let dirs: Vec<PathBuf> = project
        .workspace_packages()
        .into_iter()
        .filter(|(name, _)| globs.is_match(name))
        .map(|(_, dir)| dir.to_path_buf())
        .collect();
    if dirs.is_empty() {
        return Err(Error::NoSuchPackage(patterns.to_vec()));
    }
    Ok(dirs)
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
    /// The config file's settings for `rule`. `cli_threshold`, when given,
    /// replaces every threshold in the file.
    ///
    /// # Errors
    /// Fails if the config file has an invalid glob.
    pub fn policy(
        &self,
        rule: &str,
        default_threshold: usize,
        cli_threshold: Option<usize>,
    ) -> Result<RulePolicy> {
        self.config.policy(rule, default_threshold, cli_threshold)
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
