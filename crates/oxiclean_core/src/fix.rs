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
    error::{Error, Result},
    jsonc_tree::{JsoncDoc, Segment},
    report::{CheckResult, CommandResult, Finding, FindingKind},
};

/// One change a fix makes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FixOp {
    /// Sets the JSON member at `path`, creating the objects on the way.
    JsonSet { path: Vec<Segment>, value: Value },
    /// Appends `value` to the JSON array at `path` unless it is there
    /// already. A missing array is created holding `seed`, then `value`, so
    /// several fixes can each append to an array none of them found.
    JsonAppend { path: Vec<Segment>, value: Value, seed: Vec<Value> },
    /// Removes the items of the JSON array at `path` equal to `value`.
    JsonRemove { path: Vec<Segment>, value: Value },
    /// Replaces the bytes `start..end`, which must still read `expected`.
    Replace { start: usize, end: usize, expected: String, text: String },
}

/// The edits that resolve a finding, to one file. Its operations are
/// either all JSON operations or all byte replacements.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Fix {
    /// The file to change.
    pub file: PathBuf,
    /// What the fix does, e.g. `set "skipLibCheck": true`.
    pub description: String,
    /// The changes, applied in order.
    pub ops: Vec<FixOp>,
}

impl Fix {
    /// A fix to `file` making the changes `ops`, described by
    /// `description`.
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

/// A byte replacement of a [`FixOp::Replace`].
struct Replace<'a> {
    finding: usize,
    start: usize,
    end: usize,
    expected: &'a str,
    text: &'a str,
}

/// Applies the fixes of `result`'s findings to the files on disk, and
/// removes the findings they resolved. Returns how many findings were
/// fixed. A fix that no longer applies (the file changed, or isn't valid
/// JSONC) is skipped and its finding kept.
///
/// # Errors
/// Fails if a file can't be read or written.
pub fn apply_fixes(result: &mut CheckResult) -> Result<usize> {
    let fixed = fix_findings(&result.findings.iter().collect::<Vec<_>>())?;
    let mut flags = fixed.iter();
    result.findings.retain(|_| !flags.next().copied().unwrap_or(false));
    Ok(fixed.iter().filter(|f| **f).count())
}

/// As [`apply_fixes`], for the findings of every rule of a command at
/// once, so that fixes from different rules to one file stack.
///
/// # Errors
/// Fails if a file can't be read or written.
pub fn apply_command_fixes(result: &mut CommandResult) -> Result<usize> {
    let findings: Vec<&Finding> = result.rules.iter().flat_map(|r| &r.result.findings).collect();
    let fixed = fix_findings(&findings)?;
    let mut flags = fixed.iter();
    for r in &mut result.rules {
        r.result.findings.retain(|_| !flags.next().copied().unwrap_or(false));
    }
    Ok(fixed.iter().filter(|f| **f).count())
}

/// Applies the fixes of `findings` to the files on disk, and returns which
/// findings they resolved.
fn fix_findings(findings: &[&Finding]) -> Result<Vec<bool>> {
    let mut by_file: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
    for (i, f) in findings.iter().enumerate() {
        if let Some(fix) = fix_of(&f.kind) {
            by_file.entry(fix.file.clone()).or_default().push(i);
        }
    }
    let mut fixed = vec![false; findings.len()];
    for (file, indices) in by_file {
        let io_err = |source| Error::Fix { path: file.clone(), source };
        let original = fs::read_to_string(&file).map_err(io_err)?;
        let mut text = original.clone();
        let mut replaces: Vec<Replace<'_>> = Vec::new();
        for &i in &indices {
            let Some(fix) = fix_of(&findings[i].kind) else { continue };
            let skip = |why: &str| {
                warn!("Skipping fix '{}' to {}: {why}", fix.description, file.display());
            };
            let (json, bytes): (Vec<&FixOp>, Vec<&FixOp>) =
                fix.ops.iter().partition(|op| !matches!(op, FixOp::Replace { .. }));
            if !json.is_empty() && !bytes.is_empty() {
                skip("it mixes JSON operations and byte replacements");
                continue;
            }
            if json.is_empty() {
                replaces.extend(bytes.into_iter().filter_map(|op| match op {
                    FixOp::Replace { start, end, expected, text } => {
                        Some(Replace { finding: i, start: *start, end: *end, expected, text })
                    }
                    _ => None,
                }));
                continue;
            }
            // All of a fix's JSON operations apply, or none do.
            match json.into_iter().try_fold(text.clone(), |t, op| apply_json(&t, op)) {
                Some(next) => {
                    text = next;
                    fixed[i] = true;
                }
                None => skip("it no longer applies"),
            }
        }
        // Byte offsets refer to the original text, so replacements only
        // apply when no JSON operation changed it. They go last to first,
        // and one overlapping another is skipped.
        let json_changed = text != original;
        replaces.sort_by_key(|r| std::cmp::Reverse(r.start));
        let mut floor = usize::MAX;
        let mut failed: Vec<usize> = Vec::new();
        for r in &replaces {
            if !json_changed && r.end <= floor && text.get(r.start..r.end) == Some(r.expected) {
                text.replace_range(r.start..r.end, r.text);
                floor = r.start;
            } else {
                failed.push(r.finding);
            }
        }
        // A fix with several replacements counts only if all of them applied.
        for r in &replaces {
            fixed[r.finding] = !failed.contains(&r.finding);
        }
        for i in failed {
            if let Some(fix) = fix_of(&findings[i].kind) {
                warn!("Skipping fix '{}' to {}: the text changed", fix.description, file.display());
            }
        }
        if text != original {
            debug!("Writing fixes to {}", file.display());
            write_atomic(&file, &text).map_err(io_err)?;
        }
    }
    Ok(fixed)
}

/// The fix a finding offers, if any.
fn fix_of(kind: &FindingKind) -> Option<&Fix> {
    match kind {
        FindingKind::Issue(issue) => issue.fix.as_ref(),
        _ => None,
    }
}

/// `text` with a JSON operation applied, or `None` when it can't be. An
/// append of what is there already, and a removal of what isn't, change
/// nothing.
fn apply_json(text: &str, op: &FixOp) -> Option<String> {
    let doc = JsoncDoc::parse(text)?;
    match op {
        FixOp::JsonSet { path, value } => doc.set(path, value),
        FixOp::JsonAppend { path, value, seed } => match doc.at(path) {
            Some(node) if node.items().iter().any(|n| doc.value(n).as_ref() == Some(value)) => {
                Some(text.to_string())
            }
            Some(_) => doc.append(path, value),
            None => {
                let mut items: Vec<Value> = Vec::with_capacity(seed.len() + 1);
                for v in seed.iter().chain(std::iter::once(value)) {
                    if !items.contains(v) {
                        items.push(v.clone());
                    }
                }
                doc.set(path, &Value::Array(items))
            }
        },
        FixOp::JsonRemove { path, value } => {
            Some(doc.remove_item(path, value).unwrap_or_else(|| text.to_string()))
        }
        FixOp::Replace { .. } => None,
    }
}

/// Writes `text` to a temporary file next to `path` and renames it over
/// `path`, so a crash never leaves a half-written file. A symlink is
/// followed, so the link stays a link, and the file keeps its permissions.
fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let path = fs::canonicalize(path)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.oxiclean-fix"));
    let written = fs::write(&tmp, text)
        .and_then(|()| fs::set_permissions(&tmp, fs::metadata(&path)?.permissions()))
        .and_then(|()| fs::rename(&tmp, &path));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
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
    fn appends_to_a_missing_array_stack() {
        let t = TempDir::new().unwrap();
        let file = t.path().join("tsconfig.json");
        fs::write(&file, "{\n  \"extends\": \"./base.json\"\n}\n").unwrap();
        let append = |value: &str| {
            let path = vec![Segment::from("exclude")];
            let op = FixOp::JsonAppend { path, value: json!(value), seed: vec![json!("out")] };
            Fix::new(file.clone(), "a", vec![op])
        };
        let mut result = CheckResult {
            findings: vec![
                finding(&file, append("dist")),
                finding(&file, append("build")),
                finding(&file, append("dist")),
            ],
            ..CheckResult::default()
        };
        assert_eq!(apply_fixes(&mut result).unwrap(), 3);
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "{\n  \"extends\": \"./base.json\",\n  \"exclude\": [\"out\", \"dist\", \"build\"]\n}\n"
        );
    }

    #[test]
    fn mixed_fixes_are_skipped_and_files_left_alone() {
        let t = TempDir::new().unwrap();
        let file = t.path().join("tsconfig.json");
        fs::write(&file, "{}").unwrap();
        let ops = vec![
            FixOp::JsonSet { path: vec![Segment::from("a")], value: json!(1) },
            FixOp::Replace { start: 0, end: 1, expected: "{".into(), text: "{".into() },
        ];
        let mut result = CheckResult {
            findings: vec![finding(&file, Fix::new(file.clone(), "m", ops))],
            ..CheckResult::default()
        };
        assert_eq!(apply_fixes(&mut result).unwrap(), 0);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(fs::read_to_string(&file).unwrap(), "{}");
    }

    #[cfg(unix)]
    #[test]
    fn writes_through_symlinks() {
        let t = TempDir::new().unwrap();
        let real = t.path().join("base.json");
        let link = t.path().join("tsconfig.json");
        fs::write(&real, "{}").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let mut result = CheckResult {
            findings: vec![finding(&link, Fix::json_set(link.clone(), &["a"], json!(1)))],
            ..CheckResult::default()
        };
        assert_eq!(apply_fixes(&mut result).unwrap(), 1);
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(&real).unwrap(), "{\n  \"a\": 1\n}");
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
