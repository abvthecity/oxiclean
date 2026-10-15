//! Fixes: edits a finding can make to the project's files, applied with
//! `--fix`.
//!
//! A fix to a JSON(C) file is a list of operations on paths into the
//! document rather than byte offsets, so that several fixes to the same
//! file (such as two compiler options added to one tsconfig) apply one
//! after another without stepping on each other. A fix to any other file
//! replaces bytes, and only applies while those bytes are what the finding
//! saw.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use serde_json::Value;
use tracing::{debug, warn};

use crate::{
    jsonc_tree::{JsoncDoc, Segment},
    report::{CheckResult, FindingKind},
};

/// One change a fix makes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FixOp {
    /// Sets the JSON member at `path`, creating the objects on the way.
    JsonSet { path: Vec<Segment>, value: Value },
    /// Appends to the JSON array at `path`, creating it when missing.
    JsonAppend { path: Vec<Segment>, value: Value },
    /// Removes the items of the JSON array at `path` equal to `value`.
    JsonRemove { path: Vec<Segment>, value: Value },
    /// Replaces the bytes `start..end`, which must still read `expected`.
    Replace { start: usize, end: usize, expected: String, text: String },
}

/// The edits that resolve a finding, to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Fix {
    /// The file to change.
    pub file: PathBuf,
    /// What the fix does, e.g. `set "skipLibCheck": true`.
    pub description: String,
    pub ops: Vec<FixOp>,
}

impl Fix {
    #[must_use]
    pub fn new(file: PathBuf, description: impl Into<String>, ops: Vec<FixOp>) -> Self {
        Self { file, description: description.into(), ops }
    }

    /// A fix setting the JSON member at `path` of `file` to `value`.
    #[must_use]
    pub fn json_set(file: PathBuf, path: &[&str], value: Value) -> Self {
        let description = format!("set \"{}\": {value}", path.join("."));
        let path = path.iter().map(|k| Segment::from(*k)).collect();
        Self::new(file, description, vec![FixOp::JsonSet { path, value }])
    }
}

/// Applies the fixes of `result`'s findings to the files on disk, and
/// removes the findings they resolved. Returns how many findings were
/// fixed. A fix that no longer applies (the file changed, or isn't valid
/// JSONC) is skipped and its finding kept.
///
/// # Errors
/// Fails if a file can't be read or written.
pub fn apply_fixes(result: &mut CheckResult) -> io::Result<usize> {
    let mut by_file: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
    for (i, f) in result.findings.iter().enumerate() {
        if let Some(fix) = fix_of(&f.kind) {
            by_file.entry(fix.file.clone()).or_default().push(i);
        }
    }
    let mut fixed = vec![false; result.findings.len()];
    for (file, indices) in by_file {
        let original = fs::read_to_string(&file)?;
        let mut text = original.clone();
        // Byte replacements refer to the original text: apply them last to
        // first, after any JSON operations they can't overlap with.
        let mut replaces: Vec<(usize, &FixOp)> = Vec::new();
        for &i in &indices {
            let Some(fix) = fix_of(&result.findings[i].kind) else { continue };
            let mut next = text.clone();
            let mut ok = true;
            for op in &fix.ops {
                match apply_json(&next, op) {
                    Some(Some(t)) => next = t,
                    Some(None) => {
                        ok = false;
                        break;
                    }
                    None => replaces.push((i, op)),
                }
            }
            if ok {
                text = next;
                fixed[i] = true;
            } else {
                warn!(
                    "Skipping fix '{}' to {}: it no longer applies",
                    fix.description,
                    file.display()
                );
            }
        }
        replaces.sort_by_key(|(_, op)| match op {
            FixOp::Replace { start, .. } => std::cmp::Reverse(*start),
            _ => std::cmp::Reverse(0),
        });
        // Byte offsets refer to the original text, so they only apply when
        // no JSON operation changed it, and never overlap each other.
        let json_changed = text != original;
        let mut floor = usize::MAX;
        for (i, op) in replaces {
            let FixOp::Replace { start, end, expected, text: with } = op else { continue };
            if !json_changed && *end <= floor && text.get(*start..*end) == Some(expected.as_str()) {
                text.replace_range(*start..*end, with);
                floor = *start;
            } else {
                fixed[i] = false;
            }
        }
        if text != original {
            debug!("Writing fixes to {}", file.display());
            write_atomic(&file, &text)?;
        }
    }
    let count = fixed.iter().filter(|f| **f).count();
    let mut i = 0;
    result.findings.retain(|_| {
        i += 1;
        !fixed[i - 1]
    });
    Ok(count)
}

/// The fix a finding offers, if any.
fn fix_of(kind: &FindingKind) -> Option<&Fix> {
    match kind {
        FindingKind::Issue(issue) => issue.fix.as_ref(),
        _ => None,
    }
}

/// `Some(Some(text))` with a JSON operation applied, `Some(None)` when it
/// can't be, and `None` for a byte replacement.
fn apply_json(text: &str, op: &FixOp) -> Option<Option<String>> {
    let doc = || JsoncDoc::parse(text);
    Some(match op {
        FixOp::JsonSet { path, value } => doc().and_then(|d| d.set(path, value)),
        FixOp::JsonAppend { path, value } => doc().and_then(|d| d.append(path, value)),
        FixOp::JsonRemove { path, value } => doc().and_then(|d| d.remove_item(path, value)),
        FixOp::Replace { .. } => return None,
    })
}

/// Writes `text` to a temporary file next to `path` and renames it over
/// `path`, so a crash never leaves a half-written file.
fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.oxiclean-fix"));
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{Finding, Issue};
    use serde_json::json;
    use tempfile::TempDir;

    fn finding(file: &Path, fix: Fix) -> Finding {
        Finding::new(
            file.to_path_buf(),
            FindingKind::Issue(Issue::new("x", "k", "m", 1).with_fix(fix)),
            1,
        )
    }

    #[test]
    fn json_fixes_to_one_file_stack() {
        let t = TempDir::new().unwrap();
        let file = t.path().join("tsconfig.json");
        fs::write(&file, "{\n  // keep me\n  \"include\": [\"src\"]\n}\n").unwrap();
        let mut result = CheckResult {
            findings: vec![
                finding(&file, Fix::json_set(file.clone(), &["compilerOptions", "a"], json!(true))),
                finding(&file, Fix::json_set(file.clone(), &["compilerOptions", "b"], json!(1))),
                Finding::new(file.clone(), FindingKind::WholeGraph, 1),
            ],
            ..CheckResult::default()
        };
        assert_eq!(apply_fixes(&mut result).unwrap(), 2);
        assert_eq!(result.findings.len(), 1);
        let text = fs::read_to_string(&file).unwrap();
        assert_eq!(
            text,
            "{\n  // keep me\n  \"include\": [\"src\"],\n  \"compilerOptions\": { \"a\": true, \"b\": 1 }\n}\n"
        );
    }

    #[test]
    fn replacements_apply_only_to_unchanged_text() {
        let t = TempDir::new().unwrap();
        let file = t.path().join("eslint.config.js");
        fs::write(&file, "export default { project: true, x: 1 };").unwrap();
        let replace = |start, end, expected: &str, text: &str| {
            Fix::new(
                file.clone(),
                "r",
                vec![FixOp::Replace { start, end, expected: expected.into(), text: text.into() }],
            )
        };
        let mut result = CheckResult {
            findings: vec![
                finding(&file, replace(17, 30, "project: true", "projectService: true")),
                finding(&file, replace(32, 36, "nope", "y: 2")),
            ],
            ..CheckResult::default()
        };
        assert_eq!(apply_fixes(&mut result).unwrap(), 1);
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "export default { projectService: true, x: 1 };"
        );
        assert_eq!(result.findings.len(), 1);
    }
}
