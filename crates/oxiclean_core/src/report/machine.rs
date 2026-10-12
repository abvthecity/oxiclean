//! Machine-readable reports: JSON for scripts and SARIF for code-scanning
//! tools such as GitHub code scanning.

use std::{
    io::{self, Write},
    path::Path,
};

use serde_json::{Value, json};

use super::{CheckResult, Finding, FindingKind, ReportStyle};
use crate::settings::Severity;

/// Output format of a report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
pub enum OutputFormat {
    /// Human-readable terminal output
    #[default]
    Text,
    /// One JSON document with every finding
    Json,
    /// SARIF 2.1.0, for code-scanning tools
    Sarif,
}

/// What a machine-readable report says about the rule and the tool.
#[derive(Debug, Clone, Copy)]
pub struct ReportMeta<'a> {
    /// The rule's name, e.g. `bloat`.
    pub rule: &'a str,
    pub style: &'a ReportStyle,
    /// Version of the tool, e.g. `env!("CARGO_PKG_VERSION")`.
    pub version: &'a str,
    pub elapsed_ms: u128,
}

const INFORMATION_URI: &str = "https://github.com/abvthecity/oxiclean";
const SARIF_SCHEMA: &str = "https://json.schemastore.org/sarif-2.1.0.json";

/// `path` relative to `root` with `/` separators, or as given when it is
/// outside the root.
fn rel(path: &Path, root: &Path) -> String {
    let p = path.strip_prefix(root).unwrap_or(path);
    p.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

/// One sentence describing `f`. Paths in it are relative to `root`.
#[must_use]
pub fn message(style: &ReportStyle, f: &Finding, root: &Path) -> String {
    let value = (style.value_label)(f.value);
    let limit = format!("threshold: {} {}", f.threshold, style.threshold_unit);
    match &f.kind {
        FindingKind::Import { request, .. } => format!("import '{request}' ({value}, {limit})"),
        FindingKind::WholeGraph => format!("this entry's import graph ({value}, {limit})"),
        FindingKind::Cycle(cycle) => {
            let mut files: Vec<String> = cycle.steps.iter().map(|s| rel(&s.file, root)).collect();
            files.extend(files.first().cloned());
            format!("import cycle {} ({value}, {limit})", files.join(" -> "))
        }
        FindingKind::DuplicateExport(dup) => {
            let names: Vec<String> = dup.names.iter().map(|n| format!("'{}'", n.name)).collect();
            format!("{} export the same thing ({value}, {limit})", names.join(", "))
        }
        FindingKind::UnusedExport(unused) => {
            format!("{} '{}' is exported but never imported", unused.kind.as_str(), unused.name)
        }
        FindingKind::UnusedMember(unused) => {
            format!("{} '{}' is never used", unused.kind.as_str(), unused.name)
        }
    }
}

/// Writes the result as one JSON document. Paths are relative to the root and
/// always use `/`.
///
/// # Errors
/// Returns any error from writing to `writer`.
pub fn write_json<W: Write>(
    writer: &mut W,
    meta: ReportMeta<'_>,
    result: &CheckResult,
) -> io::Result<()> {
    let root = &result.root;
    let findings: Vec<Value> = result
        .findings
        .iter()
        .map(|f| {
            let mut v = json!({
                "rule": meta.rule,
                "severity": f.severity,
                "file": rel(&f.file, root),
                "package": f.package,
                "value": f.value,
                "threshold": f.threshold,
                "message": message(meta.style, f, root),
            });
            match &f.kind {
                FindingKind::Import { request, resolved, line } => {
                    v["kind"] = json!("import");
                    v["line"] = json!(line);
                    v["request"] = json!(request);
                    v["resolved"] = json!(rel(resolved, root));
                }
                FindingKind::WholeGraph => v["kind"] = json!("entry"),
                FindingKind::Cycle(cycle) => {
                    v["kind"] = json!("cycle");
                    v["line"] = json!(f.line());
                    v["request"] = json!(cycle.steps.first().map(|s| &s.request));
                    v["group_size"] = json!(cycle.group_size);
                    v["cycle"] = cycle
                        .steps
                        .iter()
                        .map(|s| json!({ "file": rel(&s.file, root), "line": s.line, "request": s.request }))
                        .collect();
                }
                FindingKind::DuplicateExport(dup) => {
                    v["kind"] = json!("duplicate-export");
                    v["line"] = json!(f.line());
                    v["names"] = dup
                        .names
                        .iter()
                        .map(|n| json!({ "name": n.name, "line": n.line }))
                        .collect();
                }
                FindingKind::UnusedMember(unused) => {
                    v["kind"] = json!("unused-member");
                    v["line"] = json!(unused.line);
                    v["name"] = json!(unused.name);
                    v["symbol"] = json!(unused.kind.as_str());
                }
                FindingKind::UnusedExport(unused) => {
                    v["kind"] = json!("unused-export");
                    v["line"] = json!(unused.line);
                    v["name"] = json!(unused.name);
                    v["symbol"] = json!(unused.kind.as_str());
                }
            }
            v
        })
        .collect();
    let res = &result.resolution;
    let doc = json!({
        "tool": { "name": "oxiclean", "version": meta.version },
        "rule": meta.rule,
        "root": root.to_string_lossy(),
        "threshold": result.threshold,
        "summary": {
            "files_analyzed": result.files_analyzed,
            "findings": result.findings.len(),
            "errors": result.error_count(),
            "warnings": result.findings.len() - result.error_count(),
            "elapsed_ms": meta.elapsed_ms,
            "imports": {
                "resolved": res.resolved,
                "unresolved": res.unresolved,
                "builtin": res.builtin,
            },
        },
        "findings": findings,
        "parse_failures": result.parse_failures.iter().map(|p| json!({
            "file": rel(&p.path, root),
            "message": p.message,
        })).collect::<Vec<_>>(),
    });
    serde_json::to_writer_pretty(&mut *writer, &doc)?;
    writeln!(writer)
}

/// Writes the result as a SARIF 2.1.0 log with one run. File locations are
/// relative to the `%SRCROOT%` base, which is the project root.
///
/// # Errors
/// Returns any error from writing to `writer`.
pub fn write_sarif<W: Write>(
    writer: &mut W,
    meta: ReportMeta<'_>,
    result: &CheckResult,
) -> io::Result<()> {
    let root = &result.root;
    let results: Vec<Value> = result
        .findings
        .iter()
        .map(|f| {
            let mut result = json!({
                "ruleId": meta.rule,
                "level": match f.severity {
                    Severity::Error => "error",
                    Severity::Warn => "warning",
                },
                "message": { "text": message(meta.style, f, root) },
                // GitHub code scanning needs a region; a finding about the
                // whole file points at its first line.
                "locations": [{ "physicalLocation": physical_location(&f.file, f.line().max(1), root) }],
                "partialFingerprints": { FINGERPRINT_KEY: fingerprint(meta.rule, f, root) },
                "properties": { "value": f.value, "threshold": f.threshold },
            });
            // The other places the finding involves, so code-scanning UIs
            // can link to each of them: the rest of a cycle's imports, or
            // the names that repeat an export.
            let related: Vec<(&Path, u32, String)> = match &f.kind {
                FindingKind::Cycle(cycle) => cycle
                    .steps
                    .iter()
                    .skip(1)
                    .map(|s| (s.file.as_path(), s.line, format!("imports '{}'", s.request)))
                    .collect(),
                FindingKind::DuplicateExport(dup) => dup
                    .names
                    .iter()
                    .skip(1)
                    .map(|n| (f.file.as_path(), n.line, format!("exports it as '{}'", n.name)))
                    .collect(),
                FindingKind::Import { .. }
                | FindingKind::WholeGraph
                | FindingKind::UnusedExport(_)
                | FindingKind::UnusedMember(_) => Vec::new(),
            };
            if !related.is_empty() {
                result["relatedLocations"] = related
                    .into_iter()
                    .enumerate()
                    .map(|(i, (file, line, text))| {
                        json!({
                            "id": i + 1,
                            "physicalLocation": physical_location(file, line.max(1), root),
                            "message": { "text": text },
                        })
                    })
                    .collect();
            }
            result
        })
        .collect();

    let mut root_uri = url_path(root);
    if !root_uri.ends_with('/') {
        root_uri.push('/');
    }
    let doc = json!({
        "$schema": SARIF_SCHEMA,
        "version": "2.1.0",
        "runs": [{
            "tool": { "driver": {
                "name": "oxiclean",
                "version": meta.version,
                "informationUri": INFORMATION_URI,
                "rules": [{
                    "id": meta.rule,
                    "shortDescription": { "text": meta.style.description },
                    "helpUri": format!("{INFORMATION_URI}#tools"),
                }],
            }},
            // Code scanning keeps one set of alerts per tool and category, so
            // each rule gets its own category or its uploads replace each
            // other's.
            "automationDetails": { "id": format!("oxiclean/{}/", meta.rule) },
            "originalUriBaseIds": { "%SRCROOT%": { "uri": root_uri } },
            "results": results,
        }],
    });
    serde_json::to_writer_pretty(&mut *writer, &doc)?;
    writeln!(writer)
}

/// Key of the fingerprint in `partialFingerprints`. Bump the version when
/// [`fingerprint`] changes, so code scanning doesn't match old alerts to new
/// ones.
const FINGERPRINT_KEY: &str = "oxiclean/v1";

/// A SARIF `physicalLocation` for `line` of `file`, relative to `%SRCROOT%`.
fn physical_location(file: &Path, line: u32, root: &Path) -> Value {
    json!({
        // A `:` in the first segment would read as a URI scheme.
        "artifactLocation": {
            "uri": percent_encode(&rel(file, root)).replace(':', "%3A"),
            "uriBaseId": "%SRCROOT%",
        },
        "region": { "startLine": line },
    })
}

/// Identifies a finding across commits so code scanning tracks it as one
/// alert. It leaves out line numbers, which move when unrelated code above
/// changes, and the measured value, which changes as the graph grows.
fn fingerprint(rule: &str, f: &Finding, root: &Path) -> String {
    let mut key = format!("{rule}\0{}\0", rel(&f.file, root));
    match &f.kind {
        FindingKind::Import { request, .. } => key.push_str(request),
        FindingKind::WholeGraph => key.push_str("<entry>"),
        FindingKind::Cycle(cycle) => {
            for step in &cycle.steps {
                key.push_str(&rel(&step.file, root));
                key.push('\0');
            }
        }
        FindingKind::DuplicateExport(dup) => {
            for name in &dup.names {
                key.push_str(&name.name);
                key.push('\0');
            }
        }
        FindingKind::UnusedExport(unused) | FindingKind::UnusedMember(unused) => {
            key.push_str(&unused.name);
        }
    }
    // FNV-1a: stable across Rust versions and platforms, unlike `DefaultHasher`.
    let hash = key
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    format!("{hash:016x}")
}

/// A `file://` URI for an absolute path, percent-encoding what URIs reserve.
fn url_path(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    // Windows verbatim paths (`\\?\C:\...`) become `C:/...`.
    let s = s.strip_prefix("//?/").unwrap_or(&s);
    let prefix = if s.starts_with('/') { "file://" } else { "file:///" };
    format!("{prefix}{}", percent_encode(s))
}

/// `path` with every byte outside the URI path characters percent-encoded.
fn percent_encode(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~:@".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const STYLE: ReportStyle = ReportStyle {
        description: "Imports that pull in too many modules",
        heading: "Import bloat detected",
        threshold_unit: "modules",
        value_label: |n| format!("{n} modules"),
        max_label: "Maximum bloat",
        max_unit: "modules",
        ok_message: "No bloat detected.",
    };

    fn result() -> CheckResult {
        let root = PathBuf::from("/repo");
        let mut import = Finding::new(
            root.join("pkg/src/a.ts"),
            FindingKind::Import {
                request: "./big".into(),
                resolved: root.join("pkg/src/big.ts"),
                line: 3,
            },
            12,
        );
        import.threshold = 10;
        import.package = Some("pkg".into());
        let mut entry = Finding::new(root.join("pkg/src/a.ts"), FindingKind::WholeGraph, 13);
        entry.threshold = 10;
        entry.severity = Severity::Warn;
        CheckResult {
            findings: vec![entry, import],
            root,
            files_analyzed: 14,
            threshold: 10,
            ..CheckResult::default()
        }
    }

    fn meta() -> ReportMeta<'static> {
        ReportMeta { rule: "bloat", style: &STYLE, version: "0.1.0", elapsed_ms: 5 }
    }

    #[test]
    fn json_report() {
        let mut out = Vec::new();
        write_json(&mut out, meta(), &result()).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["rule"], "bloat");
        assert_eq!(v["summary"]["errors"], 1);
        assert_eq!(v["summary"]["warnings"], 1);
        let import = &v["findings"][1];
        assert_eq!(import["kind"], "import");
        assert_eq!(import["file"], "pkg/src/a.ts");
        assert_eq!(import["resolved"], "pkg/src/big.ts");
        assert_eq!(import["line"], 3);
        assert_eq!(import["package"], "pkg");
        assert_eq!(import["severity"], "error");
        assert_eq!(import["message"], "import './big' (12 modules, threshold: 10 modules)");
        assert_eq!(v["findings"][0]["kind"], "entry");
        assert_eq!(v["findings"][0]["severity"], "warn");
        assert!(v["findings"][0]["package"].is_null());
    }

    #[test]
    fn sarif_report() {
        let mut out = Vec::new();
        write_sarif(&mut out, meta(), &result()).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["version"], "2.1.0");
        let run = &v["runs"][0];
        assert_eq!(run["tool"]["driver"]["rules"][0]["id"], "bloat");
        assert_eq!(run["originalUriBaseIds"]["%SRCROOT%"]["uri"], "file:///repo/");
        let results = run["results"].as_array().unwrap();
        assert_eq!(results[0]["level"], "warning");
        assert_eq!(results[0]["locations"][0]["physicalLocation"]["region"]["startLine"], 1);
        assert_eq!(results[1]["level"], "error");
        let loc = &results[1]["locations"][0]["physicalLocation"];
        assert_eq!(loc["artifactLocation"]["uri"], "pkg/src/a.ts");
        assert_eq!(loc["region"]["startLine"], 3);
    }

    #[test]
    fn file_uris() {
        assert_eq!(url_path(Path::new("/a b/c")), "file:///a%20b/c");
        assert_eq!(url_path(Path::new(r"\\?\C:\work\x")), "file:///C:/work/x");
    }
}
