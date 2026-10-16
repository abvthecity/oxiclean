//! A trace directory, read once for every rule of the `trace` command.

use std::path::{Path, PathBuf};

use oxiclean_core::{Error, Result, Severity, Workspace, settings::RuleSetting};
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
    /// The programs whose traces couldn't be read, and why, for the report.
    pub notes: Vec<String>,
}

impl Trace {
    /// Reads `dir`, building hot-spot trees with the `hot-spots` settings
    /// of `workspace`'s config file when `hot_spots` is set.
    ///
    /// # Errors
    /// Fails if `dir` isn't a directory, holds no trace, or no trace in it
    /// can be read.
    pub(crate) fn load(workspace: &Workspace, dir: &Path, hot_spots: bool) -> Result<Self> {
        let err = |message: String, source: Option<std::io::Error>| Error::Trace {
            path: dir.to_path_buf(),
            message,
            source,
        };
        let projects = layout::projects(dir).map_err(|e| err(e.to_string(), Some(e)))?;
        if projects.is_empty() {
            return Err(err(
                "it holds no trace.json; run `tsc --generateTrace <dir>`".into(),
                None,
            ));
        }
        let thresholds = if hot_spots { thresholds(workspace)? } else { Thresholds::none() };
        let parsed: Vec<_> = projects
            .into_par_iter()
            .map(|p| {
                let parsed = analyze::parse(&p, thresholds);
                (p, parsed)
            })
            .collect();
        let mut read = Vec::new();
        let mut failures = Vec::new();
        let mut first_error = None;
        for (project, parsed) in parsed {
            match parsed {
                Ok(parsed) => read.push((project, parsed)),
                Err(e) => {
                    warn!("Skipping {}: {e}", project.trace.display());
                    failures.push(format!("{}: {e}", project.trace.display()));
                    first_error.get_or_insert(e);
                }
            }
        }
        if read.is_empty() {
            return Err(err(failures.join("; "), first_error));
        }
        let notes = failures
            .into_iter()
            .map(|f| format!("Skipped a trace that can't be read: {f}."))
            .collect();
        // Where the trace was recorded, from every program's paths, before
        // any is looked up.
        let mut paths = PathMap::new(&workspace.root);
        paths.learn(
            read.iter().flat_map(|(p, parsed)| {
                p.config_file.as_deref().into_iter().chain(parsed.config_file.as_deref())
            }),
            read.iter().flat_map(|(_, parsed)| parsed.sample_files.iter().map(String::as_str)),
        );
        let projects = read
            .into_par_iter()
            .map(|(project, parsed)| analyze::analyze(project, parsed, thresholds, &paths))
            .collect();
        Ok(Self { projects, paths, root: workspace.root.clone(), notes })
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
        self.paths.here(raw).map_or_else(|| raw.replace('\\', "/"), |p| self.show(&p))
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
    let in_overrides = workspace.config.overrides.iter().any(|o| {
        matches!(
            o.rules.get(HOT_SPOTS_RULE),
            Some(RuleSetting::WithOptions(_, options)) if options.skip_millis.is_some()
        )
    });
    if in_overrides {
        warn!(
            "{HOT_SPOTS_RULE}: skipMillis is read only from `rules`, as the whole trace is read with one setting; the overrides' skipMillis is ignored"
        );
    }
    if skip > force {
        warn!(
            "{HOT_SPOTS_RULE}: skipMillis ({skip}) is above the threshold ({force}); using {force}"
        );
        skip = force;
    }
    Ok(Thresholds { force: force as f64 * 1000.0, skip: skip as f64 * 1000.0 })
}
