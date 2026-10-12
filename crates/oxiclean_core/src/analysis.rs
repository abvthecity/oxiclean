//! Options shared by every checker, and the one-time setup they all need:
//! find the root, collect entry files, and build the module graph.

use std::path::{Path, PathBuf};

use dashmap::DashMap;
use log::{debug, info};

use crate::{
    CollectorConfig, Project, ResolutionStats, SpecKind, collect_entries,
    config::find_git_root,
    error::{Error, Result},
    graph::{BuildOptions, ModuleGraph, ModuleId, ParsedFile, RawImport},
    parse_imports,
    report::{CheckResult, Finding, sort_findings},
    resolve,
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

    /// Resolves the root, collects entry files and builds the module graph.
    ///
    /// # Errors
    /// Fails if the root can't be found or read, a glob is invalid, or no
    /// entry files are found.
    pub fn analyze(&self) -> Result<Analysis> {
        let root = match &self.root {
            Some(r) => {
                r.canonicalize().map_err(|source| Error::Root { path: r.clone(), source })?
            }
            None => find_git_root()?,
        };
        info!("Using root directory: {}", root.display());

        let project = Project::load(&root);
        debug!("Found {} workspace packages", project.workspace_package_count());

        let entry_paths = collect_entries(&CollectorConfig {
            root: root.clone(),
            entry_globs: self.entry_globs.clone(),
        })?;
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
                        })
                        .collect(),
                    error: parsed.error,
                })
            },
            |from: &Path, request: &str| {
                resolve(&project, from, request, &resolve_cache).ok().flatten()
            },
            BuildOptions::default().follow_dynamic(self.include_dynamic),
        );

        let entries = entry_paths.iter().filter_map(|p| graph.id(p)).collect();
        let resolution = ResolutionStats::from_cache(&resolve_cache);
        Ok(Analysis { root, graph, entries, resolution })
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
}

impl Analysis {
    /// Packages a checker's findings, sorted deterministically, with the
    /// statistics of this analysis.
    #[must_use]
    pub fn finish(&self, mut findings: Vec<Finding>) -> CheckResult {
        sort_findings(&mut findings);
        CheckResult {
            findings,
            root: self.root.clone(),
            files_analyzed: self.graph.len(),
            parse_failures: self.graph.failures().to_vec(),
            resolution: self.resolution,
        }
    }
}
