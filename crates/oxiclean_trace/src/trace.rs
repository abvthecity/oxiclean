//! A trace directory, read once for every rule of the `trace` command.

use std::path::{Path, PathBuf};

use oxiclean_core::{Error, Result, Severity, Workspace};
use rayon::prelude::*;
use tracing::warn;

use crate::{
    analyze::{self, ProjectTrace, Thresholds},
    config::{DEFAULT_SKIP_MILLIS, HOT_SPOTS_DEFAULT_THRESHOLD, HOT_SPOTS_RULE},
    layout,
    paths::PathMap,
};

/// Every program a trace directory holds, analysed.
#[derive(Debug)]
pub(crate) struct Trace {
    pub projects: Vec<ProjectTrace>,
    pub paths: PathMap,
    /// The root, to show paths relative to.
    pub root: PathBuf,
}

impl Trace {
    /// Reads `dir`, building hot-spot trees with the `hot-spots` settings
    /// of `workspace`'s config file when `hot_spots` is set.
    ///
    /// # Errors
    /// Fails if `dir` isn't a directory, holds no trace, or no trace in it
    /// can be read.
    pub(crate) fn load(workspace: &Workspace, dir: &Path, hot_spots: bool) -> Result<Self> {
        let err = |message: String| Error::Trace { path: dir.to_path_buf(), message };
        let projects = layout::projects(dir).map_err(|e| err(e.to_string()))?;
        if projects.is_empty() {
            return Err(err("it holds no trace.json; run `tsc --generateTrace <dir>`".into()));
        }
        let thresholds = if hot_spots { thresholds(workspace)? } else { Thresholds::none() };
        let paths = PathMap::new(&workspace.root);
        let results: Vec<_> = projects
            .into_par_iter()
            .map(|p| {
                let trace = p.trace.clone();
                analyze::analyze(p, thresholds, &paths).map_err(|e| (trace, e))
            })
            .collect();
        let mut read = Vec::new();
        let mut failures = Vec::new();
        for r in results {
            match r {
                Ok(p) => read.push(p),
                Err((trace, e)) => {
                    warn!("Skipping {}: {e}", trace.display());
                    failures.push(format!("{}: {e}", trace.display()));
                }
            }
        }
        if read.is_empty() {
            return Err(err(failures.join("; ")));
        }
        Ok(Self { projects: read, paths, root: workspace.root.clone() })
    }

    /// Files the traces checked.
    pub(crate) fn checked_files(&self) -> usize {
        self.projects.iter().map(|p| p.checked_files).sum()
    }

    /// `path` relative to the root with `/` separators, or as given.
    pub(crate) fn show(&self, path: &Path) -> String {
        let p = path.strip_prefix(&self.root).unwrap_or(path);
        p.to_string_lossy().replace('\\', "/")
    }

    /// A file the trace names, as shown in messages: relative to the root
    /// when it's here, else as the trace writes it.
    pub(crate) fn show_raw(&self, raw: &str) -> String {
        self.paths.local(raw).map_or_else(|| raw.to_string(), |p| self.show(&p))
    }

    /// Whether the directory holds more than one program's trace.
    pub(crate) fn is_build(&self) -> bool {
        self.projects.len() > 1
    }
}

/// The hot-spot settings of `workspace`'s config file, in microseconds.
fn thresholds(workspace: &Workspace) -> Result<Thresholds> {
    let policy = workspace.policy(HOT_SPOTS_RULE, HOT_SPOTS_DEFAULT_THRESHOLD, Severity::Warn)?;
    let Some(force) = policy.min_threshold() else { return Ok(Thresholds::none()) };
    let mut skip = workspace
        .config
        .rule_options(HOT_SPOTS_RULE)
        .and_then(|o| o.skip_millis)
        .unwrap_or(DEFAULT_SKIP_MILLIS);
    if skip > force {
        warn!(
            "{HOT_SPOTS_RULE}: skipMillis ({skip}) is above the threshold ({force}); using {force}"
        );
        skip = force;
    }
    Ok(Thresholds { force: force as f64 * 1000.0, skip: skip as f64 * 1000.0 })
}
