//! The files of a trace directory, found as analyze-trace finds them.
//!
//! `tsc --generateTrace <dir>` writes `trace.json` and `types.json`.
//! `tsc --build` writes one pair per project it checks, `trace.N.json`
//! and `types.N.json`, and a `legend.json` naming the tsconfig of each;
//! with several checkers, one trace can come with several types files,
//! told apart by `checkerId`.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use tracing::warn;

/// A `types.json` and the checker it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypeSource {
    pub path: PathBuf,
    pub checker_id: Option<serde_json::Value>,
}

/// One program's trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Project {
    /// The tsconfig the program was built from, as `legend.json` gives
    /// it.
    pub config_file: Option<String>,
    pub trace: PathBuf,
    /// The types files that exist.
    pub types: Vec<TypeSource>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegendEntry {
    config_file_path: Option<String>,
    trace_path: String,
    types_path: Option<String>,
    checker_id: Option<serde_json::Value>,
}

/// The base name of `path`, written with either separator.
fn base_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// The programs traced in `dir`: as `legend.json` lists them, or else one
/// for each `trace*.json`, with the `types*.json` of the same suffix.
///
/// # Errors
/// Fails if `dir` can't be read.
pub(crate) fn projects(dir: &Path) -> std::io::Result<Vec<Project>> {
    let legend = dir.join("legend.json");
    let mut projects = if legend.is_file() {
        match fs::read_to_string(&legend)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<Vec<LegendEntry>>(&t).map_err(|e| e.to_string()))
        {
            Ok(entries) if !entries.is_empty() => Some(from_legend(dir, entries)),
            Ok(_) => {
                warn!("{} lists no programs; reading every trace*.json instead", legend.display());
                None
            }
            Err(e) => {
                warn!("Can't read {}: {e}; reading every trace*.json instead", legend.display());
                None
            }
        }
    } else {
        None
    };
    if projects.is_none() {
        let mut found = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            // Following symlinks: a trace.json may link to one elsewhere.
            if !is_regular_file(&entry.path()) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(suffix) = name.strip_prefix("trace").filter(|s| s.ends_with(".json")) else {
                continue;
            };
            found.push(Project {
                config_file: None,
                trace: dir.join(&name),
                types: vec![TypeSource {
                    path: dir.join(format!("types{suffix}")),
                    checker_id: None,
                }],
            });
        }
        found.sort_by(|a, b| a.trace.cmp(&b.trace));
        projects = Some(found);
    }
    let mut projects = projects.unwrap_or_default();
    projects.retain(|p| {
        let ok = is_regular_file(&p.trace);
        if !ok {
            warn!("Skipping {}: it isn't a file", p.trace.display());
        }
        ok
    });
    for p in &mut projects {
        p.types.retain(|t| is_regular_file(&t.path));
    }
    Ok(projects)
}

/// Whether `path` is a regular file, following symlinks: not a FIFO or a
/// device, which would hang or never end.
fn is_regular_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file())
}

/// The projects of `legend.json`, each trace once with every types file
/// that goes with it.
fn from_legend(dir: &Path, entries: Vec<LegendEntry>) -> Vec<Project> {
    let mut projects: Vec<Project> = Vec::new();
    for e in entries {
        let trace = dir.join(base_name(&e.trace_path));
        let source = e
            .types_path
            .as_deref()
            .map(|t| TypeSource { path: dir.join(base_name(t)), checker_id: e.checker_id.clone() });
        let existing =
            projects.iter_mut().find(|p| p.trace == trace && p.config_file == e.config_file_path);
        match existing {
            Some(p) => {
                if let Some(s) = source
                    && !p.types.contains(&s)
                {
                    p.types.push(s);
                }
            }
            None => projects.push(Project {
                config_file: e.config_file_path,
                trace,
                types: source.into_iter().collect(),
            }),
        }
    }
    projects
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn single_and_build_layouts() {
        let t = TempDir::new().unwrap();
        let d = t.path();
        fs::write(d.join("trace.json"), "[]").unwrap();
        fs::write(d.join("types.json"), "[]").unwrap();
        fs::write(d.join("trace.1.json"), "[]").unwrap();
        fs::write(d.join("notes.txt"), "").unwrap();
        let ps = projects(d).unwrap();
        assert_eq!(ps.len(), 2);
        assert_eq!(ps[0].trace, d.join("trace.1.json"));
        assert!(ps[0].types.is_empty(), "types.1.json is missing");
        assert_eq!(ps[1].types[0].path, d.join("types.json"));

        // A legend from Windows, naming two checkers of one trace.
        fs::write(
            d.join("legend.json"),
            r#"[
              {"configFilePath":"C:/r/a/tsconfig.json","tracePath":"C:\\r\\t\\trace.1.json","typesPath":"C:\\r\\t\\types.json","checkerId":1},
              {"configFilePath":"C:/r/a/tsconfig.json","tracePath":"C:\\r\\t\\trace.1.json","typesPath":"C:\\r\\t\\types.json","checkerId":2},
              {"configFilePath":"C:/r/b/tsconfig.json","tracePath":"/r/t/trace.json"}
            ]"#,
        )
        .unwrap();
        let ps = projects(d).unwrap();
        assert_eq!(ps.len(), 2);
        assert_eq!(ps[0].config_file.as_deref(), Some("C:/r/a/tsconfig.json"));
        assert_eq!(ps[0].trace, d.join("trace.1.json"));
        assert_eq!(ps[0].types.len(), 2);
        assert!(ps[1].types.is_empty());

        // A broken or empty legend: every trace*.json instead.
        fs::write(d.join("legend.json"), "[{").unwrap();
        assert_eq!(projects(d).unwrap().len(), 2);
        fs::write(d.join("legend.json"), "[]").unwrap();
        assert_eq!(projects(d).unwrap().len(), 2);
        // A trace.json that links to one elsewhere.
        #[cfg(unix)]
        {
            let other = TempDir::new().unwrap();
            fs::write(other.path().join("real.json"), "[]").unwrap();
            fs::remove_file(d.join("trace.json")).unwrap();
            std::os::unix::fs::symlink(other.path().join("real.json"), d.join("trace.json"))
                .unwrap();
            assert_eq!(projects(d).unwrap().len(), 2);
        }
        assert!(projects(&d.join("missing")).is_err());
    }
}
