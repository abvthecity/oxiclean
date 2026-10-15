//! Duplicate package versions: packages a lockfile installs at more than
//! one version, read from `package-lock.json` (v2 and v3) and
//! `pnpm-lock.yaml` (v6 and v9).

use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap},
    fs,
};

use oxiclean_core::{CheckResult, DuplicateVersions, Finding, FindingKind, Result, Workspace};
use serde_json::Value;
use tracing::{info, warn};

use crate::{
    config::{Config, VERSIONS_DEFAULT_THRESHOLD, VERSIONS_RULE},
    yaml::mapping_lines,
};

/// Runs the duplicate versions check with `cfg` on the root's lockfiles.
///
/// # Errors
/// Fails if the root can't be found or read, or the config file is invalid.
pub fn run_duplicate_versions_check(cfg: &Config) -> Result<CheckResult> {
    run_on(&cfg.common.workspace()?)
}

/// Runs the duplicate versions check on `workspace`.
pub(crate) fn run_on(workspace: &Workspace) -> Result<CheckResult> {
    info!("Starting {VERSIONS_RULE} check");
    let policy = workspace.policy(VERSIONS_RULE, VERSIONS_DEFAULT_THRESHOLD)?;
    let readers: [(&str, Reader); 2] =
        [("pnpm-lock.yaml", pnpm_installed), ("package-lock.json", npm_installed)];
    let mut findings = Vec::new();
    let mut lockfiles = 0;
    for (name, read) in readers {
        let path = workspace.root.join(name);
        let Ok(text) = fs::read_to_string(&path) else { continue };
        lockfiles += 1;
        for dup in duplicate_versions(read(&text)) {
            let count = dup.versions.len();
            findings.push(Finding::new(path.clone(), FindingKind::DuplicateVersions(dup), count));
        }
    }
    info!("{VERSIONS_RULE} check complete. Found {} packages", findings.len());
    Ok(workspace.finish(&policy, findings, lockfiles))
}

/// Reads the installed packages of a lockfile's text.
type Reader = fn(&str) -> Vec<Installed>;

/// A package at one version in a lockfile.
#[derive(Debug, PartialEq)]
pub(crate) struct Installed {
    name: String,
    version: String,
    /// 1-based line of its entry.
    line: u32,
}

/// The packages `installed` has at two or more versions, by name.
fn duplicate_versions(installed: Vec<Installed>) -> Vec<DuplicateVersions> {
    let mut by_name: BTreeMap<String, (Vec<String>, u32)> = BTreeMap::new();
    for i in installed {
        let (versions, line) = by_name.entry(i.name).or_insert_with(|| (Vec::new(), i.line));
        *line = (*line).min(i.line);
        if !versions.contains(&i.version) {
            versions.push(i.version);
        }
    }
    by_name
        .into_iter()
        .filter(|(_, (versions, _))| versions.len() > 1)
        .map(|(name, (mut versions, line))| {
            versions.sort_by(|a, b| compare_versions(a, b));
            DuplicateVersions::new(name, versions, line)
        })
        .collect()
}

/// Orders versions by their numeric parts, `1.10.0` after `1.9.0`, then
/// as text.
fn compare_versions(a: &str, b: &str) -> Ordering {
    let parts =
        |v: &str| -> Vec<u64> { v.split(['.', '-', '+']).map_while(|p| p.parse().ok()).collect() };
    parts(a).cmp(&parts(b)).then_with(|| a.cmp(b))
}

/// The registry packages of `pnpm-lock.yaml`: the keys of `packages` in its
/// last document, as `'@scope/a@1.0.0':` (v9) or `/a@1.0.0(peer@2):` (v6).
/// pnpm 10 and later put their own install in a first document.
fn pnpm_installed(text: &str) -> Vec<Installed> {
    let (offset, document) = last_document(text);
    mapping_lines(document)
        .into_iter()
        .filter_map(|l| match l.path.as_slice() {
            [top, key] if top == "packages" => {
                let (name, version) = pnpm_key(key)?;
                Some(Installed { name, version, line: l.line + offset })
            }
            _ => None,
        })
        .collect()
}

/// The text after the last `---` line of `text`, and how many lines come
/// before it.
fn last_document(text: &str) -> (u32, &str) {
    let mut start = 0;
    let mut lines_before = 0;
    let mut line = 0;
    for (i, l) in text.split_inclusive('\n').scan(0, |at, l| {
        let i = *at;
        *at += l.len();
        Some((i, l))
    }) {
        line += 1;
        if l.trim_end() == "---" {
            start = i + l.len();
            lines_before = line;
        }
    }
    (lines_before, &text[start..])
}

/// The name and version of a pnpm `packages` key: `@scope/a@1.0.0`, with
/// v6's leading `/` and a peer suffix such as `(react@18.2.0)` dropped.
fn pnpm_key(key: &str) -> Option<(String, String)> {
    let key = key.trim_start_matches('/');
    let key = key.split('(').next()?;
    let at = key[1..].find('@')? + 1;
    let (name, version) = (&key[..at], &key[at + 1..]);
    (!name.is_empty() && !version.is_empty()).then(|| (name.to_string(), version.to_string()))
}

/// The installed packages of `package-lock.json` v2 or v3: each
/// `node_modules/...` entry of `packages` that isn't a link to a workspace
/// package.
fn npm_installed(text: &str) -> Vec<Installed> {
    let json: Value = match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
        Ok(json) => json,
        Err(e) => {
            warn!("Skipping package-lock.json: {e}");
            return Vec::new();
        }
    };
    let Some(Value::Object(packages)) = json.get("packages") else {
        warn!("package-lock.json has no `packages`; lockfile v1 isn't read");
        return Vec::new();
    };
    let lines = key_lines(text);
    packages
        .iter()
        .filter(|(_, v)| v.get("link") != Some(&Value::Bool(true)))
        .filter_map(|(key, v)| {
            let (_, name) = key.rsplit_once("node_modules/")?;
            let version = v.get("version")?.as_str()?;
            let line = lines.get(key.as_str()).copied().unwrap_or(0);
            Some(Installed { name: name.to_string(), version: version.to_string(), line })
        })
        .collect()
}

/// The line of each `"key": {` in `text`.
fn key_lines(text: &str) -> HashMap<&str, u32> {
    let mut out = HashMap::new();
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix('"')
            && let Some(end) = rest.find("\": {")
        {
            out.entry(&rest[..end]).or_insert(u32::try_from(i + 1).unwrap_or(u32::MAX));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(installed: Vec<Installed>) -> Vec<(String, Vec<String>, u32)> {
        duplicate_versions(installed).into_iter().map(|d| (d.name, d.versions, d.line)).collect()
    }

    #[test]
    fn pnpm_v9_with_an_env_document() {
        let text = "---
lockfileVersion: '9.0'

packages:
  '@pnpm/exe@10.0.0':
    resolution: {integrity: sha512-x}

---
lockfileVersion: '9.0'

packages:

  '@scope/a@1.10.0':
    resolution: {integrity: sha512-a}

  '@scope/a@1.9.0':
    resolution: {integrity: sha512-b}

  b@2.0.0: {}

snapshots:

  '@scope/a@1.9.0(react@18.2.0)': {}
";
        assert_eq!(
            summary(pnpm_installed(text)),
            [("@scope/a".to_string(), vec!["1.9.0".to_string(), "1.10.0".to_string()], 13)]
        );
    }

    #[test]
    fn pnpm_v6_keys() {
        assert_eq!(pnpm_key("/a@1.0.0"), Some(("a".into(), "1.0.0".into())));
        assert_eq!(
            pnpm_key("/@scope/b@2.0.0(react@18.2.0)"),
            Some(("@scope/b".into(), "2.0.0".into()))
        );
        assert_eq!(pnpm_key("/nameonly"), None);
    }

    #[test]
    fn npm_lockfile() {
        let text = r#"{
  "lockfileVersion": 3,
  "packages": {
    "": { "name": "root" },
    "node_modules/a": {
      "version": "2.0.0"
    },
    "node_modules/b/node_modules/a": {
      "version": "1.0.0"
    },
    "node_modules/@acme/ui": {
      "resolved": "packages/ui",
      "link": true
    },
    "node_modules/b": {
      "version": "1.0.0"
    }
  }
}"#;
        assert_eq!(
            summary(npm_installed(text)),
            [("a".to_string(), vec!["1.0.0".to_string(), "2.0.0".to_string()], 5)]
        );
    }
}
