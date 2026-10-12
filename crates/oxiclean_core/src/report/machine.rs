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
    /// The rule's name, e.g. `import-bloat`.
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

/// One sentence describing `f`.
#[must_use]
pub fn message(style: &ReportStyle, f: &Finding) -> String {
    let value = (style.value_label)(f.value);
    let limit = format!("threshold: {} {}", f.threshold, style.threshold_unit);
    match &f.kind {
        FindingKind::Import { request, .. } => format!("import '{request}' ({value}, {limit})"),
        FindingKind::WholeGraph => format!("this entry's import graph ({value}, {limit})"),
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
                "message": message(meta.style, f),
            });
            match &f.kind {
                FindingKind::Import { request, resolved, line } => {
                    v["kind"] = json!("import");
                    v["line"] = json!(line);
                    v["request"] = json!(request);
                    v["resolved"] = json!(rel(resolved, root));
                }
                FindingKind::WholeGraph => v["kind"] = json!("entry"),
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
            let mut location = json!({
                "artifactLocation": { "uri": rel(&f.file, root), "uriBaseId": "%SRCROOT%" },
            });
            if f.line() > 0 {
                location["region"] = json!({ "startLine": f.line() });
            }
            json!({
                "ruleId": meta.rule,
                "level": match f.severity {
                    Severity::Error => "error",
                    Severity::Warn => "warning",
                },
                "message": { "text": message(meta.style, f) },
                "locations": [{ "physicalLocation": location }],
                "properties": { "value": f.value, "threshold": f.threshold },
            })
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
            "originalUriBaseIds": { "%SRCROOT%": { "uri": root_uri } },
            "results": results,
        }],
    });
    serde_json::to_writer_pretty(&mut *writer, &doc)?;
    writeln!(writer)
}

/// A `file://` URI for an absolute path, percent-encoding what URIs reserve.
fn url_path(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    // Windows verbatim paths (`\\?\C:\...`) become `C:/...`.
    let s = s.strip_prefix("//?/").unwrap_or(&s);
    let mut out = String::from(if s.starts_with('/') { "file://" } else { "file:///" });
    for b in s.bytes() {
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
        ReportMeta { rule: "import-bloat", style: &STYLE, version: "0.1.0", elapsed_ms: 5 }
    }

    #[test]
    fn json_report() {
        let mut out = Vec::new();
        write_json(&mut out, meta(), &result()).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["rule"], "import-bloat");
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
        assert_eq!(run["tool"]["driver"]["rules"][0]["id"], "import-bloat");
        assert_eq!(run["originalUriBaseIds"]["%SRCROOT%"]["uri"], "file:///repo/");
        let results = run["results"].as_array().unwrap();
        assert_eq!(results[0]["level"], "warning");
        assert!(results[0]["locations"][0]["physicalLocation"]["region"].is_null());
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
