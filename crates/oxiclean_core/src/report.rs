//! Findings and the terminal reporter shared by every checker.

use std::{
    collections::BTreeMap,
    io::{self, Write},
    path::{Component, Path, PathBuf},
};

use colored::Colorize;

use crate::{graph::ParseFailure, resolver::ResolutionStats};

/// What a finding points at.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FindingKind {
    /// A single import in the file.
    Import {
        /// The specifier as written, e.g. `./utils`.
        request: String,
        /// The file the import resolved to.
        resolved: PathBuf,
    },
    /// The file's whole import graph.
    WholeGraph,
}

/// One threshold violation reported by a checker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Absolute path of the file the finding is about.
    pub file: PathBuf,
    pub kind: FindingKind,
    /// The measured value (module count, depth, ...).
    pub value: usize,
}

/// The output of a checker run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CheckResult {
    /// Findings, sorted by file then by descending value.
    pub findings: Vec<Finding>,
    /// Canonical project root the analysis ran on.
    pub root: PathBuf,
    /// Number of modules in the analysed graph.
    pub files_analyzed: usize,
    /// Files that could not be read or parsed.
    pub parse_failures: Vec<ParseFailure>,
    /// How many imports resolved.
    pub resolution: ResolutionStats,
}

/// Sorts by file, whole-graph findings first, then by descending value and
/// request, so output never depends on thread scheduling.
pub fn sort_findings(findings: &mut [Finding]) {
    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| is_import(a).cmp(&is_import(b)))
            .then_with(|| b.value.cmp(&a.value))
            .then_with(|| request(a).cmp(request(b)))
    });
}

fn is_import(f: &Finding) -> bool {
    matches!(f.kind, FindingKind::Import { .. })
}

fn request(f: &Finding) -> &str {
    match &f.kind {
        FindingKind::Import { request, .. } => request,
        FindingKind::WholeGraph => "",
    }
}

/// Wording that differs between checkers.
#[derive(Debug, Clone, Copy)]
pub struct ReportStyle {
    /// e.g. "Import bloat detected".
    pub heading: &'static str,
    /// Unit of the threshold in the heading, e.g. "modules".
    pub threshold_unit: &'static str,
    /// Renders a value next to an import, e.g. "123 modules" or "depth: 4".
    pub value_label: fn(usize) -> String,
    /// e.g. "Maximum bloat".
    pub max_label: &'static str,
    /// Unit after the maximum, e.g. "modules".
    pub max_unit: &'static str,
    /// Printed when nothing crossed the threshold.
    pub ok_message: &'static str,
}

/// Paths the reporter displays relative to.
#[derive(Debug, Clone, Copy)]
pub struct ReportContext<'a> {
    /// Project root. Shown when a path can't be made relative to `cwd`.
    pub root: &'a Path,
    /// Directory paths are shown relative to, so they are clickable.
    pub cwd: &'a Path,
}

impl ReportContext<'_> {
    fn display(&self, path: &Path) -> String {
        make_relative(path, self.cwd)
            .or_else(|| path.strip_prefix(self.root).ok().map(Path::to_path_buf))
            .unwrap_or_else(|| path.to_path_buf())
            .to_string_lossy()
            .into_owned()
    }
}

/// Prints the success line.
///
/// # Errors
/// Returns any error from writing to `writer`.
pub fn print_ok<W: Write>(writer: &mut W, style: &ReportStyle, threshold: usize) -> io::Result<()> {
    writeln!(writer, "{} {} Threshold: {threshold}", "✓".green().bold(), style.ok_message)
}

/// Prints findings grouped by file, worst file first, followed by a summary.
///
/// # Errors
/// Returns any error from writing to `writer`.
pub fn print_findings<W: Write>(
    writer: &mut W,
    findings: &[Finding],
    style: &ReportStyle,
    threshold: usize,
    ctx: ReportContext<'_>,
) -> io::Result<()> {
    writeln!(
        writer,
        "{} {} (threshold: {} {})\n",
        "⚠".yellow().bold(),
        style.heading,
        threshold.to_string().yellow(),
        style.threshold_unit,
    )?;

    let mut by_file: BTreeMap<&Path, Vec<&Finding>> = BTreeMap::new();
    for f in findings {
        by_file.entry(&f.file).or_default().push(f);
    }
    let mut files: Vec<(&Path, Vec<&Finding>)> = by_file.into_iter().collect();
    files.sort_by_key(|(path, fs)| (std::cmp::Reverse(fs.iter().map(|f| f.value).max()), *path));

    for (path, mut file_findings) in files {
        let display_path = ctx.display(path);
        match file_findings.iter().find(|f| f.kind == FindingKind::WholeGraph) {
            Some(whole) => writeln!(
                writer,
                "{} ({})",
                display_path.blue(),
                (style.value_label)(whole.value).red().bold()
            )?,
            None => writeln!(writer, "{}", display_path.blue().bold())?,
        }

        file_findings.retain(|f| is_import(f));
        file_findings.sort_by_key(|f| (std::cmp::Reverse(f.value), request(f)));
        let last = file_findings.len().saturating_sub(1);
        for (idx, f) in file_findings.iter().enumerate() {
            let prefix = if idx == last { "└──" } else { "├──" };
            writeln!(
                writer,
                "{}  import '{}' ({})",
                prefix.dimmed(),
                request(f),
                (style.value_label)(f.value).red()
            )?;
        }
        writeln!(writer)?;
    }

    print_summary(writer, findings, style, ctx)
}

fn print_summary<W: Write>(
    writer: &mut W,
    findings: &[Finding],
    style: &ReportStyle,
    ctx: ReportContext<'_>,
) -> io::Result<()> {
    let mut violations: Vec<&Finding> = findings.iter().filter(|f| is_import(f)).collect();
    if violations.is_empty() {
        return Ok(());
    }
    violations.sort_by_key(|f| (std::cmp::Reverse(f.value), &f.file, request(f)));
    let max = violations[0].value;

    writeln!(writer, "{}", "─".repeat(60).dimmed())?;
    writeln!(writer, "{}", "Summary".bold())?;
    writeln!(writer, "  Total violations: {}", violations.len().to_string().yellow().bold())?;
    writeln!(writer, "  {}: {} {}", style.max_label, max.to_string().red().bold(), style.max_unit)?;

    let top = &violations[..violations.len().min(5)];
    writeln!(writer, "  Top {} offenders:", top.len())?;
    for (idx, f) in top.iter().enumerate() {
        writeln!(
            writer,
            "    {}. import '{}' ({}) - {}",
            idx + 1,
            request(f),
            (style.value_label)(f.value).red(),
            ctx.display(&f.file).blue()
        )?;
    }
    Ok(())
}

/// Prints the closing line, plus a note about files that could not be parsed.
///
/// # Errors
/// Returns any error from writing to `writer`.
pub fn print_finished<W: Write>(
    writer: &mut W,
    result: &CheckResult,
    elapsed_ms: u128,
    threads: usize,
    ctx: ReportContext<'_>,
) -> io::Result<()> {
    if !result.parse_failures.is_empty() {
        writeln!(
            writer,
            "\n{} {} file(s) could not be parsed; their imports were skipped:",
            "⚠".yellow(),
            result.parse_failures.len().to_string().yellow()
        )?;
        for failure in result.parse_failures.iter().take(5) {
            writeln!(writer, "    {}", ctx.display(&failure.path).dimmed())?;
        }
        if result.parse_failures.len() > 5 {
            writeln!(writer, "    ... (set RUST_LOG=warn for the full list)")?;
        }
    }
    writeln!(
        writer,
        "\n{} Finished in {}ms on {} files (using {} threads).",
        "●".bright_blue(),
        elapsed_ms.to_string().cyan(),
        result.files_analyzed.to_string().cyan(),
        threads.to_string().cyan()
    )
}

/// Builds a relative path from `base` to `target`, or `None` when they share
/// no root (e.g. different Windows drives).
#[must_use]
pub fn make_relative(target: &Path, base: &Path) -> Option<PathBuf> {
    let mut target_components = target.components().peekable();
    let mut base_components = base.components().peekable();

    if target_components.peek() != base_components.peek() {
        return None;
    }
    while let (Some(t), Some(b)) = (target_components.peek(), base_components.peek()) {
        if t != b {
            break;
        }
        target_components.next();
        base_components.next();
    }

    let mut result = PathBuf::new();
    for _ in base_components {
        result.push("..");
    }
    for component in target_components {
        match component {
            Component::Normal(p) => result.push(p),
            Component::ParentDir => result.push(".."),
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    if result.as_os_str().is_empty() { Some(PathBuf::from(".")) } else { Some(result) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_relative_same_dir() {
        let r = make_relative(Path::new("/project/src/file.ts"), Path::new("/project/src"));
        assert_eq!(r, Some(PathBuf::from("file.ts")));
    }

    #[test]
    fn make_relative_child_dir() {
        let r = make_relative(
            Path::new("/project/src/components/Button.tsx"),
            Path::new("/project/src"),
        );
        assert_eq!(r, Some(PathBuf::from("components/Button.tsx")));
    }

    #[test]
    fn make_relative_parent_dir() {
        let r =
            make_relative(Path::new("/project/src/file.ts"), Path::new("/project/src/components"));
        assert_eq!(r, Some(PathBuf::from("../file.ts")));
    }

    #[test]
    fn make_relative_sibling_dir() {
        let r =
            make_relative(Path::new("/project/apps/web/index.ts"), Path::new("/project/apps/api"));
        assert_eq!(r, Some(PathBuf::from("../web/index.ts")));
    }

    #[test]
    fn make_relative_same_path() {
        let r = make_relative(Path::new("/project/src"), Path::new("/project/src"));
        assert_eq!(r, Some(PathBuf::from(".")));
    }

    #[test]
    fn make_relative_target_is_ancestor() {
        let r = make_relative(Path::new("/project"), Path::new("/project/src/a"));
        assert_eq!(r, Some(PathBuf::from("../..")));
    }

    #[test]
    fn make_relative_multiple_levels_up() {
        let r = make_relative(Path::new("/project/file.ts"), Path::new("/project/apps/web/src"));
        assert_eq!(r, Some(PathBuf::from("../../../file.ts")));
    }

    #[test]
    fn make_relative_needs_shared_root() {
        assert_eq!(make_relative(Path::new("a/b"), Path::new("/a")), None);
    }

    fn import(file: &str, request: &str, value: usize) -> Finding {
        Finding {
            file: PathBuf::from(file),
            kind: FindingKind::Import { request: request.into(), resolved: PathBuf::from(request) },
            value,
        }
    }

    const STYLE: ReportStyle = ReportStyle {
        heading: "Import bloat detected",
        threshold_unit: "modules",
        value_label: |n| format!("{n} modules"),
        max_label: "Maximum bloat",
        max_unit: "modules",
        ok_message: "No bloat detected.",
    };

    #[test]
    fn findings_are_sorted_deterministically() {
        let mut fs = vec![
            import("/p/b.ts", "./x", 5),
            Finding { file: "/p/a.ts".into(), kind: FindingKind::WholeGraph, value: 9 },
            import("/p/a.ts", "./y", 3),
            import("/p/a.ts", "./z", 7),
        ];
        sort_findings(&mut fs);
        let order: Vec<(String, usize)> =
            fs.iter().map(|f| (f.file.display().to_string(), f.value)).collect();
        assert_eq!(
            order,
            vec![
                ("/p/a.ts".into(), 9),
                ("/p/a.ts".into(), 7),
                ("/p/a.ts".into(), 3),
                ("/p/b.ts".into(), 5)
            ]
        );
    }

    #[test]
    fn report_lists_worst_file_first_with_summary() {
        colored::control::set_override(false);
        let fs = vec![import("/p/src/a.ts", "./small", 3), import("/p/src/b.ts", "./big", 10)];
        let mut out = Vec::new();
        let ctx = ReportContext { root: Path::new("/p"), cwd: Path::new("/p") };
        print_findings(&mut out, &fs, &STYLE, 2, ctx).unwrap();
        let out = String::from_utf8(out).unwrap();
        // Paths are printed with the platform's separator.
        let rel = |name: &str| ["src", name].iter().collect::<PathBuf>().display().to_string();
        let b = out.find(&rel("b.ts")).unwrap();
        let a = out.find(&rel("a.ts")).unwrap();
        assert!(b < a, "{out}");
        assert!(out.contains("Total violations: 2"), "{out}");
        assert!(out.contains("Maximum bloat: 10 modules"), "{out}");
        assert!(
            out.contains(&format!("1. import './big' (10 modules) - {}", rel("b.ts"))),
            "{out}"
        );
    }
}
