//! `hot-spots`: the frames of analyze-trace's hot-spot tree, one finding
//! each, joined to what `complex-types` flags.
//!
//! Each frame is reported on the file it's in, at its first line, with the
//! frame it's in as a related location, so the tree can be read back from
//! the findings. Comparisons and variances name their types, from
//! `types.json`, with where each is declared. A frame on lines a
//! `complex-types` finding covers, or comparing a type declared there,
//! says the trace confirms that finding, and is an error even when the
//! rule's severity is `warn`.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use oxiclean_core::{
    CheckResult, Finding, FindingKind, Issue, Related, Result, Severity, Workspace,
    report::sort_findings,
};
use oxiclean_typecheck::FlaggedType;
use serde_json::Value;
use tracing::info;

use crate::{
    analyze::{Frame, FrameKind, HotType, ProjectTrace, Range},
    config::{HOT_SPOTS_DEFAULT_THRESHOLD, HOT_SPOTS_RULE},
    trace::Trace,
};

/// Runs `hot-spots` on `trace`.
///
/// # Errors
/// Fails if the config file has an invalid glob.
pub(crate) fn run_on(workspace: &Workspace, trace: &Trace) -> Result<CheckResult> {
    info!("Starting {HOT_SPOTS_RULE} check");
    let policy = workspace.policy(HOT_SPOTS_RULE, HOT_SPOTS_DEFAULT_THRESHOLD, Severity::Warn)?;
    let flagged = if workspace.enabled(oxiclean_typecheck::COMPLEX_TYPES_RULE) {
        oxiclean_typecheck::flagged_types(workspace)?.unwrap_or_default()
    } else {
        Vec::new()
    };
    let joiner = Joiner::new(&flagged);
    let mut findings = Vec::new();
    let mut keys: HashMap<String, usize> = HashMap::new();
    for project in &trace.projects {
        let mut walker =
            Walker { trace, project, joiner: &joiner, keys: &mut keys, out: &mut findings };
        walker.walk(&project.frames, None);
    }

    let mut result = workspace.finish(&policy, Vec::new(), trace.checked_files());
    // Every frame of the tree is reported, including those under the
    // threshold that take most of the frame they're in, so the policy
    // decides only where the rule is on and how severe it is.
    let mut kept: Vec<Finding> = findings
        .into_iter()
        .filter_map(|(mut f, confirmed)| {
            let package = workspace.project.package_for(&f.file);
            let rel = f.file.strip_prefix(&workspace.root).unwrap_or(&f.file);
            let rule = policy.resolve(rel, package);
            if !rule.enabled || rule.ignored {
                return None;
            }
            f.threshold = rule.threshold;
            f.severity = if confirmed { Severity::Error } else { rule.severity };
            f.package = package.map(str::to_string);
            Some(f)
        })
        .collect();
    sort_findings(&mut kept);
    info!(findings = kept.len(), "{HOT_SPOTS_RULE} check complete");
    result.findings = kept;
    Ok(result)
}

/// What `complex-types` flags, by file.
struct Joiner<'f> {
    by_file: HashMap<&'f Path, Vec<&'f FlaggedType>>,
}

impl<'f> Joiner<'f> {
    fn new(flagged: &'f [FlaggedType]) -> Self {
        let mut by_file: HashMap<&Path, Vec<&FlaggedType>> = HashMap::new();
        for f in flagged {
            by_file.entry(f.finding.file.as_path()).or_default().push(f);
        }
        Self { by_file }
    }

    /// The findings in `file` whose lines overlap `first..=last`.
    fn overlapping(
        &self,
        file: &Path,
        first: u64,
        last: u64,
    ) -> impl Iterator<Item = &'f FlaggedType> {
        self.by_file
            .get(file)
            .into_iter()
            .flatten()
            .copied()
            .filter(move |f| u64::from(f.finding.line()) <= last && first <= u64::from(f.end_line))
    }
}

/// Turns frames into findings, depth first.
struct Walker<'a, 'f> {
    trace: &'a Trace,
    project: &'a ProjectTrace,
    joiner: &'a Joiner<'f>,
    /// How many findings have had each key, so each gets its own.
    keys: &'a mut HashMap<String, usize>,
    /// Each finding, and whether the trace confirms a `complex-types` one.
    out: &'a mut Vec<(Finding, bool)>,
}

/// The frame a frame is in, for its related location.
struct Parent<'f> {
    /// Where it was reported.
    file: PathBuf,
    line: u32,
    description: String,
    /// The `complex-types` findings it or a frame it's in confirms, which
    /// the frames in it don't repeat.
    confirmed: Vec<&'f FlaggedType>,
}

impl<'f> Walker<'_, 'f> {
    fn walk(&mut self, frames: &[Frame], parent: Option<&Parent<'f>>) {
        for frame in frames {
            let this = self.report(frame, parent);
            self.walk(&frame.children, Some(&this));
        }
    }

    /// Records the finding for `frame`, and returns it as a parent.
    fn report(&mut self, frame: &Frame, parent: Option<&Parent<'f>>) -> Parent<'f> {
        let trace = self.trace;
        let local = frame.file.as_deref().and_then(|f| trace.paths.local(f));
        let here = local.as_ref().filter(|f| f.starts_with(&trace.root));
        let parent_file = parent.map(|p| &p.file);
        // A frame in a file that isn't here, such as TypeScript's own lib
        // files, is reported where the frame it's in is.
        let (file, line) = match (here, parent_file) {
            (Some(f), _) => {
                let line = match frame.range {
                    Some(Range::Lines { start, .. }) => clamp(start.0),
                    // A comparison is where the expression it's in is.
                    _ if matches!(
                        frame.kind,
                        FrameKind::Compare { .. } | FrameKind::Variance { .. }
                    ) =>
                    {
                        parent.filter(|p| &p.file == f).map_or(0, |p| p.line)
                    }
                    _ => 0,
                };
                (f.clone(), line)
            }
            (None, Some(pf)) => (pf.clone(), parent.map_or(0, |p| p.line)),
            // A frame outside the root with nothing around it, such as
            // checking one of TypeScript's lib files, is reported on the
            // trace file when that's here, so every location names a file
            // in the project.
            (None, None) => {
                let trace_file = &self.project.project.trace;
                let file = if trace_file.starts_with(&trace.root) {
                    trace_file.clone()
                } else {
                    local
                        .clone()
                        .or_else(|| frame.file.as_ref().map(PathBuf::from))
                        .unwrap_or_else(|| trace_file.clone())
                };
                (file, 0)
            }
        };
        let mut description = self.description(frame);
        if matches!(frame.kind, FrameKind::Check)
            && let Some(raw) = frame.file.as_deref()
            && local.as_ref() != Some(&file)
            && matches!(frame.range, Some(Range::Lines { .. }))
        {
            description =
                description.replacen(" from ", &format!(" in {} from ", trace.show_raw(raw)), 1);
        }
        let mut message = format!("{description} took {}ms", frame.time_ms);
        let mut related = Vec::new();
        if frame.unterminated {
            message.push_str(", and was still running when the trace ended");
        }
        match &frame.kind {
            FrameKind::Compare { .. } | FrameKind::Variance { .. } if !frame.types.is_empty() => {
                let names: Vec<String> =
                    frame.types.iter().map(|t| self.type_label(t.as_ref())).collect();
                message.push_str(&format!(": {}", names.join(" and ")));
                for t in frame.types.iter().flatten() {
                    if let Some((file, line)) = self.type_location(t) {
                        related.push(Related::new(
                            file,
                            line,
                            format!("{} is declared here", t_name(t)),
                        ));
                    }
                }
            }
            FrameKind::EmitDeclarations { imports } => {
                let repeated: Vec<String> =
                    imports.iter().map(|(m, n)| format!("import({m}) {n} times")).collect();
                message.push_str(&format!(
                    "; it repeats {}: import those types by name in the source so the declarations can too",
                    repeated.join(", ")
                ));
            }
            _ => {}
        }
        if let Some(config) =
            self.project.project.config_file.as_deref().filter(|_| trace.is_build())
        {
            message.push_str(&format!(" (program of {})", trace.show_raw(config)));
        }

        let inherited = parent.map_or(&[][..], |p| p.confirmed.as_slice());
        let confirmed: Vec<&'f FlaggedType> = self
            .confirmations(frame, local.as_deref())
            .into_iter()
            .filter(|f| !inherited.iter().any(|i| std::ptr::eq(*i, *f)))
            .collect();
        for f in &confirmed {
            let FindingKind::Issue(issue) = &f.finding.kind else { continue };
            message.push_str(&format!(
                "; this confirms the complex-types finding at {}:{} ({}, about {} types)",
                trace.show(&f.finding.file),
                issue.line,
                issue.code,
                f.finding.value
            ));
            related.push(Related::new(
                f.finding.file.clone(),
                issue.line,
                format!("complex-types: {}", issue.message),
            ));
        }
        if let Some(p) = parent {
            related.insert(
                0,
                Related::new(p.file.clone(), p.line, format!("inside {}", p.description)),
            );
        }

        let trace_name =
            self.project.project.trace.file_name().map(|n| n.to_string_lossy().to_string());
        let base = format!("{}:{}:{description}", trace_name.unwrap_or_default(), frame.event);
        let n = self.keys.entry(base.clone()).or_default();
        *n += 1;
        let key = if *n == 1 { base } else { format!("{base}#{n}") };
        let issue = Issue::new(kebab(&frame.event), key, message, line).with_related(related);
        let value = usize::try_from(frame.time_ms).unwrap_or(usize::MAX);
        self.out.push((
            Finding::new(file.clone(), FindingKind::Issue(issue), value),
            !confirmed.is_empty(),
        ));
        let mut all = inherited.to_vec();
        all.extend(confirmed);
        Parent { file, line, description, confirmed: all }
    }

    /// What `frame` is, in analyze-trace's words, with paths relative to
    /// the root.
    fn description(&self, frame: &Frame) -> String {
        let shown = frame.file.as_deref().map(|f| self.trace.show_raw(f)).unwrap_or_default();
        match &frame.kind {
            FrameKind::CheckFile => format!("Check file {shown}"),
            FrameKind::EmitDeclarations { .. } => format!("Emit declarations file {shown}"),
            FrameKind::Compare { source, target } => {
                format!("Compare types {} and {}", js(source), js(target))
            }
            FrameKind::Variance { id } => format!("Determine variance of type {}", js(id)),
            FrameKind::Check => {
                let what = unmangle(&frame.event);
                match frame.range {
                    Some(Range::Lines { start, end }) => format!(
                        "{what} from (line {}, char {}) to (line {}, char {})",
                        start.0, start.1, end.0, end.1
                    ),
                    Some(Range::Offsets { start, end }) => {
                        format!("{what} in {shown} from offset {start} to offset {end}")
                    }
                    None => what,
                }
            }
        }
    }

    /// `type 12 (`Name`, AliasedUnion of 7680, at src/a.ts:3:1)`.
    fn type_label(&self, t: Option<&HotType>) -> String {
        let Some(t) = t else { return "a type types.json doesn't have".into() };
        let mut parts = Vec::new();
        if let Some(name) = &t.ty.name {
            parts.push(format!("`{name}`"));
        }
        match t.ty.count {
            Some(n) => parts.push(format!("{} of {n}", t.ty.kind)),
            None => parts.push(t.ty.kind.clone()),
        }
        if t.ty.name.is_none()
            && let Some(display) = &t.ty.display
        {
            parts.push(format!("`{}`", shorten(display)));
        }
        if let Some(loc) = &t.ty.location {
            let (line, char) = t.line_char.unwrap_or((loc.line, loc.char));
            parts.push(format!("at {}:{line}:{char}", self.trace.show_raw(&loc.path)));
        }
        format!("type {} ({})", t.ty.id, parts.join(", "))
    }

    /// Where `t` is declared, when that file is under the root.
    fn type_location(&self, t: &HotType) -> Option<(PathBuf, u32)> {
        let loc = t.ty.location.as_ref()?;
        let file = self.trace.paths.local(&loc.path)?;
        file.starts_with(&self.trace.root)
            .then(|| (file, clamp(t.line_char.map_or(loc.line, |l| l.0))))
    }

    /// The `complex-types` findings `frame` confirms: those on its lines,
    /// or where a type it compares is declared.
    fn confirmations(&self, frame: &Frame, local: Option<&Path>) -> Vec<&'f FlaggedType> {
        let mut found: Vec<&'f FlaggedType> = Vec::new();
        if let (Some(file), Some(Range::Lines { start, end })) = (local, frame.range) {
            found.extend(self.joiner.overlapping(file, start.0, end.0));
        }
        for t in frame.types.iter().flatten() {
            if let Some((file, line)) = self.type_location(t) {
                found.extend(self.joiner.overlapping(&file, u64::from(line), u64::from(line)));
            }
        }
        found.sort_by_key(|f| (&f.finding.file, f.finding.line()));
        found.dedup_by(|a, b| std::ptr::eq(*a, *b));
        found
    }
}

fn t_name(t: &HotType) -> String {
    t.ty.name.as_ref().map_or_else(|| format!("type {}", t.ty.id), |n| format!("`{n}`"))
}

/// A value as JavaScript's template strings print it.
fn js(v: &Value) -> String {
    match v {
        Value::Null => "undefined".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `checkVariableDeclaration` as `Check variable declaration`, as
/// analyze-trace's `unmangleCamelCase` writes it.
pub(crate) fn unmangle(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if out.is_empty() {
            out.extend(c.to_uppercase());
            continue;
        }
        let lower: String = c.to_lowercase().collect();
        if lower != c.to_string() {
            out.push(' ');
        }
        out.push_str(&lower);
    }
    out
}

/// `checkVariableDeclaration` as `check-variable-declaration`, for the
/// finding's code.
fn kebab(name: &str) -> String {
    unmangle(name).to_lowercase().replace(' ', "-")
}

/// At most 80 characters of how `tsc` prints a type.
fn shorten(display: &str) -> String {
    let mut chars = display.chars();
    let short: String = chars.by_ref().take(80).collect();
    if chars.next().is_some() { format!("{short}…") } else { short }
}

fn clamp(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_events_as_analyze_trace() {
        assert_eq!(unmangle("checkVariableDeclaration"), "Check variable declaration");
        assert_eq!(unmangle("checkExpression"), "Check expression");
        assert_eq!(kebab("checkDeferredNode"), "check-deferred-node");
        assert_eq!(kebab("structuredTypeRelatedTo"), "structured-type-related-to");
        assert_eq!(js(&Value::Null), "undefined");
        assert_eq!(js(&Value::from(12)), "12");
    }
}
