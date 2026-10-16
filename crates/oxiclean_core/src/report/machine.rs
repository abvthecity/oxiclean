//! Machine-readable reports: JSON for scripts and SARIF for code-scanning
//! tools such as GitHub code scanning.

use std::{
    io::{self, Write},
    path::Path,
};

use serde_json::{Value, json};

use super::{CommandResult, Finding, FindingKind, ReportStyle, RuleResult};
use crate::{
    savings::{Estimate, Savings},
    settings::Severity,
};

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

/// What a machine-readable report says about the command and the tool.
#[derive(Debug, Clone, Copy)]
pub struct ReportMeta<'a> {
    /// The command's name, e.g. `unused`.
    pub command: &'a str,
    /// Version of the tool, e.g. `env!("CARGO_PKG_VERSION")`.
    pub version: &'a str,
    pub elapsed_ms: u128,
}

const INFORMATION_URI: &str = "https://github.com/abvthecity/oxiclean";
const SARIF_SCHEMA: &str = "https://json.schemastore.org/sarif-2.1.0.json";

/// `path` relative to `root` with `/` separators, or as given when it is
/// outside the root.
fn rel(path: &Path, root: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(p) => {
            p.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
        }
        // Joining the components of an absolute path would double its
        // leading separator.
        Err(_) => path.to_string_lossy().replace('\\', "/"),
    }
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
        FindingKind::UnusedDependency(dep) => {
            format!("'{}' is listed in {} but never imported or run", dep.name, dep.field)
        }
        FindingKind::UnusedCatalogEntry(entry) => format!(
            "'{}' is in the {} catalog but no package.json refers to it",
            entry.name, entry.catalog
        ),
        FindingKind::Barrel(barrel) => format!(
            "barrel file re-exporting {} modules: importing it pulls in {value} ({limit})",
            barrel.reexports
        ),
        FindingKind::HeavyDependency(dep) => format!(
            "package '{}' pulls in {value} ({limit}), imported by {} files",
            dep.name, dep.importers
        ),
        FindingKind::DuplicateVersions(dup) => format!(
            "package '{}' is installed at {value}: {} ({limit})",
            dup.name,
            dup.versions.join(", ")
        ),
        FindingKind::Issue(issue) => issue.message.clone(),
    }
}

/// Writes the result as one JSON document, with an entry per rule that ran
/// and every rule's findings, each naming its rule. Paths are relative to
/// the root and always use `/`.
///
/// # Errors
/// Returns any error from writing to `writer`.
pub fn write_json<W: Write>(
    writer: &mut W,
    meta: ReportMeta<'_>,
    result: &CommandResult,
) -> io::Result<()> {
    let root = &result.root;
    let savings = Savings::of(result);
    let findings: Vec<Value> = result
        .rules
        .iter()
        .enumerate()
        .flat_map(|(r, rule)| rule.result.findings.iter().enumerate().map(move |(i, f)| (r, i, rule, f)))
        .map(|(r, i, RuleResult { rule, style, .. }, f)| {
            let mut v = json!({
                "rule": rule,
                "severity": f.severity,
                "file": rel(&f.file, root),
                "package": f.package,
                "value": f.value,
                "threshold": f.threshold,
                "message": message(style, f, root),
                "savings": savings.estimate(r, i).map(estimate_json),
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
                FindingKind::UnusedDependency(dep) => {
                    v["kind"] = json!("unused-dependency");
                    v["line"] = json!(dep.line);
                    v["name"] = json!(dep.name);
                    v["field"] = json!(dep.field);
                }
                FindingKind::UnusedCatalogEntry(entry) => {
                    v["kind"] = json!("unused-catalog-entry");
                    v["line"] = json!(entry.line);
                    v["name"] = json!(entry.name);
                    v["catalog"] = json!(entry.catalog);
                }
                FindingKind::Barrel(barrel) => {
                    v["kind"] = json!("barrel");
                    v["reexports"] = json!(barrel.reexports);
                    v["importers"] = json!(barrel.importers);
                }
                FindingKind::HeavyDependency(dep) => {
                    v["kind"] = json!("heavy-dependency");
                    v["line"] = json!(dep.line);
                    v["name"] = json!(dep.name);
                    v["importers"] = json!(dep.importers);
                }
                FindingKind::DuplicateVersions(dup) => {
                    v["kind"] = json!("duplicate-versions");
                    v["line"] = json!(dup.line);
                    v["name"] = json!(dup.name);
                    v["versions"] = json!(dup.versions);
                }
                FindingKind::Issue(issue) => {
                    v["kind"] = json!("issue");
                    v["code"] = json!(issue.code);
                    v["line"] = json!(issue.line);
                    v["related"] = issue
                        .related
                        .iter()
                        .map(|r| json!({ "file": rel(&r.file, root), "line": r.line, "message": r.message }))
                        .collect();
                    if let Some(fix) = &issue.fix {
                        v["fix"] = json!({ "file": rel(&fix.file, root), "description": fix.description });
                    }
                }
            }
            v
        })
        .collect();
    let rules: Vec<Value> = result
        .rules
        .iter()
        .map(|r| {
            let errors = r.result.error_count();
            // The rule's level outside the overrides: "off" when it runs
            // only for the files an override turns it on for. A rule that
            // couldn't run has no settings to show.
            let (severity, threshold) = match (&r.error, r.result.severity) {
                (Some(_), _) => (Value::Null, Value::Null),
                (None, Some(severity)) => (json!(severity), json!(r.result.threshold)),
                (None, None) => (json!("off"), json!(r.result.threshold)),
            };
            json!({
                "rule": r.rule,
                "severity": severity,
                "threshold": threshold,
                "files_analyzed": r.result.files_analyzed,
                "findings": r.result.findings.len(),
                "errors": errors,
                "warnings": r.result.findings.len() - errors,
                "error": r.error,
            })
        })
        .collect();
    let res = result.resolution();
    let (count, errors) = (result.finding_count(), result.error_count());
    let doc = json!({
        "tool": { "name": "oxiclean", "version": meta.version },
        "command": meta.command,
        "root": root.to_string_lossy(),
        "rules": rules,
        "summary": {
            "files_analyzed": result.files_analyzed(),
            "findings": count,
            "errors": errors,
            "warnings": count - errors,
            "elapsed_ms": meta.elapsed_ms,
            "imports": {
                "resolved": res.resolved,
                "unresolved": res.unresolved,
                "builtin": res.builtin,
            },
        },
        "savings": savings_json(result, &savings),
        "findings": findings,
        "parse_failures": result.parse_failures().iter().map(|p| json!({
            "file": rel(&p.path, root),
            "message": p.message,
        })).collect::<Vec<_>>(),
    });
    serde_json::to_writer_pretty(&mut *writer, &doc)?;
    writeln!(writer)
}

/// Writes the result as a SARIF 2.1.0 log with one run, which lists every
/// rule that ran. File locations are relative to the `%SRCROOT%` base,
/// which is the project root.
///
/// # Errors
/// Returns any error from writing to `writer`.
pub fn write_sarif<W: Write>(
    writer: &mut W,
    meta: ReportMeta<'_>,
    result: &CommandResult,
) -> io::Result<()> {
    let root = &result.root;
    let savings = Savings::of(result);
    let results: Vec<Value> = result
        .rules
        .iter()
        .enumerate()
        .flat_map(|(i, r)| r.result.findings.iter().enumerate().map(move |(n, f)| (i, n, r, f)))
        .map(|(index, n, RuleResult { rule, style, .. }, f)| {
            let mut result = json!({
                "ruleId": rule,
                "ruleIndex": index,
                "level": match f.severity {
                    Severity::Error => "error",
                    Severity::Warn => "warning",
                },
                "message": { "text": message(style, f, root) },
                // GitHub code scanning needs a region; a finding about the
                // whole file points at its first line.
                "locations": [{ "physicalLocation": physical_location(&f.file, f.line().max(1), root) }],
                "partialFingerprints": { FINGERPRINT_KEY: fingerprint(rule, f, root) },
                "properties": { "value": f.value, "threshold": f.threshold },
            });
            if let Some(e) = savings.estimate(index, n) {
                result["properties"]["savings"] = estimate_json(e);
            }
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
                FindingKind::Issue(issue) => issue
                    .related
                    .iter()
                    .map(|r| (r.file.as_path(), r.line, r.message.clone()))
                    .collect(),
                FindingKind::Import { .. }
                | FindingKind::WholeGraph
                | FindingKind::UnusedExport(_)
                | FindingKind::UnusedMember(_)
                | FindingKind::UnusedDependency(_)
                | FindingKind::UnusedCatalogEntry(_)
                | FindingKind::Barrel(_)
                | FindingKind::HeavyDependency(_)
                | FindingKind::DuplicateVersions(_) => Vec::new(),
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

    let rules: Vec<Value> = result
        .rules
        .iter()
        .map(|r| {
            json!({
                "id": r.rule,
                "shortDescription": { "text": r.style.description },
                "helpUri": format!("{INFORMATION_URI}#tools"),
            })
        })
        .collect();
    // A rule that couldn't run is a tool execution notification, so code
    // scanning shows the run as incomplete rather than clean.
    let notifications: Vec<Value> = result
        .rules
        .iter()
        .enumerate()
        .filter_map(|(index, r)| {
            let error = r.error.as_deref()?;
            Some(json!({
                "level": "error",
                "message": { "text": format!("{} couldn't run: {error}", r.rule) },
                "associatedRule": { "id": r.rule, "index": index },
            }))
        })
        .collect();
    let mut invocation = json!({ "executionSuccessful": notifications.is_empty() });
    if !notifications.is_empty() {
        invocation["toolExecutionNotifications"] = Value::Array(notifications);
    }
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
                "rules": rules,
            }},
            // Code scanning keeps one set of alerts per tool and category, so
            // each command gets its own category or its uploads replace each
            // other's.
            "automationDetails": { "id": format!("oxiclean/{}/", meta.command) },
            "invocations": [invocation],
            "properties": { "savings": savings_json(result, &savings) },
            "originalUriBaseIds": { "%SRCROOT%": { "uri": root_uri } },
            "results": results,
        }],
    });
    serde_json::to_writer_pretty(&mut *writer, &doc)?;
    writeln!(writer)
}

/// Rounds `hours` to a ten-thousandth (a third of a second), so reports
/// don't print float noise but small estimates still rank.
fn hours(hours: f64) -> f64 {
    (hours * 10_000.0).round() / 10_000.0
}

/// Rounds `seconds` to a millisecond.
fn seconds(seconds: f64) -> f64 {
    (seconds * 1000.0).round() / 1000.0
}

/// What fixing a finding saves, as JSON.
fn estimate_json(e: &Estimate) -> Value {
    let per_run: serde_json::Map<String, Value> = crate::savings::ToolSeconds::TOOLS
        .iter()
        .zip(e.per_run.by_tool())
        .filter(|(_, s)| *s > 0.0)
        .map(|(tool, s)| ((*tool).to_string(), json!(seconds(s))))
        .collect();
    json!({
        "seconds_per_run": per_run,
        "scope": e.scope.as_str(),
        "developer_hours_per_week": hours(e.developer_hours),
        "ci_hours_per_week": hours(e.ci_hours),
        "weighted_hours_per_week": hours(e.weighted_hours),
    })
}

/// The health score, the totals, each rule's share and the team settings
/// they were computed with, as JSON.
fn savings_json(result: &CommandResult, savings: &Savings) -> Value {
    let root = &result.root;
    let team = &savings.team;
    let local = &team.local_runs_per_day;
    let top: Vec<Value> = savings
        .ranked
        .iter()
        .take(10)
        .map(|&(r, i)| {
            let (rule, f) = (&result.rules[r], &result.rules[r].result.findings[i]);
            json!({
                "rule": rule.rule,
                "file": rel(&f.file, root),
                "line": f.line(),
                "message": message(&rule.style, f, root),
                "savings": savings.estimate(r, i).map(estimate_json),
            })
        })
        .collect();
    json!({
        "health": { "score": savings.health.score, "grade": savings.health.grade.to_string() },
        "developer_hours_per_week": hours(savings.total.developer_hours),
        "ci_hours_per_week": hours(savings.total.ci_hours),
        "weighted_hours_per_week": hours(savings.total.weighted_hours),
        "baseline": {
            "developer_hours_per_week": hours(savings.baseline.developer_hours),
            "ci_hours_per_week": hours(savings.baseline.ci_hours),
            "weighted_hours_per_week": hours(savings.baseline.weighted_hours),
        },
        "by_rule": savings.by_rule.iter().map(|r| json!({
            "rule": r.rule,
            "findings": r.findings,
            "developer_hours_per_week": hours(r.developer_hours),
            "ci_hours_per_week": hours(r.ci_hours),
            "weighted_hours_per_week": hours(r.weighted_hours),
        })).collect::<Vec<_>>(),
        "top": top,
        "assumptions": {
            "source_files": savings.scale.source_files,
            "packages": savings.scale.packages,
            "developers": team.developers,
            "work_days_per_week": team.work_days_per_week,
            "ci_runs_per_day": team.ci_runs_per_day,
            "ci_hours_per_developer_hour": team.ci_hours_per_developer_hour,
            "local_runs_per_day": {
                "typecheck": local.typecheck,
                "lint": local.lint,
                "test": local.test,
                "build": local.build,
                "install": local.install,
            },
        },
    })
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
        FindingKind::Barrel(_) => key.push_str("<barrel>"),
        FindingKind::HeavyDependency(dep) => key.push_str(&dep.name),
        FindingKind::DuplicateVersions(dup) => key.push_str(&dup.name),
        FindingKind::Issue(issue) => {
            key.push_str(&issue.code);
            key.push('\0');
            key.push_str(&issue.key);
        }
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
        FindingKind::UnusedDependency(dep) => {
            key.push_str(&dep.field);
            key.push('\0');
            key.push_str(&dep.name);
        }
        FindingKind::UnusedCatalogEntry(entry) => {
            key.push_str(&entry.catalog);
            key.push('\0');
            key.push_str(&entry.name);
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
    use crate::{
        exports::ExportKind,
        report::{CheckResult, UnusedExport},
    };
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

    #[test]
    fn paths_outside_the_root_keep_one_leading_slash() {
        let root = Path::new("/repo");
        assert_eq!(rel(Path::new("/repo/src/a.ts"), root), "src/a.ts");
        assert_eq!(rel(Path::new("/elsewhere/a.ts"), root), "/elsewhere/a.ts");
    }

    fn result() -> CommandResult {
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
        let bloat = CheckResult {
            findings: vec![entry, import],
            root: root.clone(),
            files_analyzed: 14,
            threshold: 10,
            severity: Some(Severity::Error),
            ..CheckResult::default()
        };
        let mut unused = Finding::new(
            root.join("pkg/src/b.ts"),
            FindingKind::UnusedExport(UnusedExport::new("b".into(), 2, ExportKind::Value)),
            1,
        );
        unused.threshold = 1;
        let unused = CheckResult {
            findings: vec![unused],
            root: root.clone(),
            files_analyzed: 9,
            threshold: 1,
            ..CheckResult::default()
        };
        let mut result = CommandResult::new(root.clone());
        result.push("bloat", STYLE, bloat);
        result.push("unused-exports", STYLE, unused);
        result.push("depth", STYLE, CheckResult { root, ..CheckResult::default() });
        result
    }

    fn meta() -> ReportMeta<'static> {
        ReportMeta { command: "mixed", version: "0.1.0", elapsed_ms: 5 }
    }

    #[test]
    fn json_report() {
        let mut out = Vec::new();
        write_json(&mut out, meta(), &result()).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["command"], "mixed");
        assert_eq!(v["summary"]["files_analyzed"], 14);
        assert_eq!(v["summary"]["errors"], 2);
        assert_eq!(v["summary"]["warnings"], 1);
        let rules: Vec<&str> =
            v["rules"].as_array().unwrap().iter().map(|r| r["rule"].as_str().unwrap()).collect();
        assert_eq!(rules, ["bloat", "unused-exports", "depth"]);
        assert_eq!(v["rules"][0]["threshold"], 10);
        assert_eq!(v["rules"][0]["severity"], "error");
        assert_eq!(v["rules"][0]["error"], Value::Null);
        // Off outside the overrides: it ran only where one turns it on.
        assert_eq!(v["rules"][1]["severity"], "off");
        assert_eq!(v["rules"][0]["findings"], 2);
        assert_eq!(v["rules"][0]["warnings"], 1);
        assert_eq!(v["rules"][1]["errors"], 1);
        assert_eq!(v["rules"][2]["findings"], 0);
        assert_eq!(v["findings"][0]["rule"], "bloat");
        assert_eq!(v["findings"][2]["rule"], "unused-exports");
        assert_eq!(v["findings"][2]["name"], "b");
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
        let ids: Vec<&str> = run["tool"]["driver"]["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["bloat", "unused-exports", "depth"]);
        assert_eq!(run["automationDetails"]["id"], "oxiclean/mixed/");
        assert_eq!(run["originalUriBaseIds"]["%SRCROOT%"]["uri"], "file:///repo/");
        let results = run["results"].as_array().unwrap();
        assert_eq!(results[0]["level"], "warning");
        assert_eq!(results[0]["locations"][0]["physicalLocation"]["region"]["startLine"], 1);
        assert_eq!(results[1]["level"], "error");
        let loc = &results[1]["locations"][0]["physicalLocation"];
        assert_eq!(loc["artifactLocation"]["uri"], "pkg/src/a.ts");
        assert_eq!(loc["region"]["startLine"], 3);
        assert_eq!(
            (&results[2]["ruleId"], &results[2]["ruleIndex"]),
            (&json!("unused-exports"), &json!(1))
        );
        assert_eq!(run["invocations"][0]["executionSuccessful"], true);
        assert!(run["invocations"][0].get("toolExecutionNotifications").is_none());
    }

    #[test]
    fn failed_rules_are_reported() {
        let mut result = result();
        result.rules.insert(
            1,
            RuleResult::failed("broken", STYLE, PathBuf::from("/repo"), "no entry files found"),
        );

        let mut out = Vec::new();
        write_json(&mut out, meta(), &result).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        let broken = &v["rules"][1];
        assert_eq!(broken["rule"], "broken");
        assert_eq!(broken["error"], "no entry files found");
        assert_eq!((&broken["severity"], &broken["threshold"]), (&Value::Null, &Value::Null));
        assert_eq!(v["rules"][0]["error"], Value::Null);

        let mut out = Vec::new();
        write_sarif(&mut out, meta(), &result).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        let run = &v["runs"][0];
        assert_eq!(run["tool"]["driver"]["rules"][1]["id"], "broken");
        let invocation = &run["invocations"][0];
        assert_eq!(invocation["executionSuccessful"], false);
        let notification = &invocation["toolExecutionNotifications"][0];
        assert_eq!(notification["level"], "error");
        assert_eq!(notification["message"]["text"], "broken couldn't run: no entry files found");
        assert_eq!(notification["associatedRule"], json!({ "id": "broken", "index": 1 }));
        // The results of the rules after it keep pointing at their own rule.
        let unused =
            run["results"].as_array().unwrap().iter().find(|r| r["ruleId"] == "unused-exports");
        assert_eq!(unused.unwrap()["ruleIndex"], 2);
    }

    #[test]
    fn file_uris() {
        assert_eq!(url_path(Path::new("/a b/c")), "file:///a%20b/c");
        assert_eq!(url_path(Path::new(r"\\?\C:\work\x")), "file:///C:/work/x");
    }
}
