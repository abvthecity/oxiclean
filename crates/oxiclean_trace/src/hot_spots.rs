//! `hot-spots`: the frames of analyze-trace's hot-spot tree, one finding
//! each, joined to what `complex-types` flags.
//!
//! Each frame is reported on the file it's in, at its first line, with the
//! frame it's in as a related location, so the tree can be read back from
//! the findings. Comparisons and variances name their types, from
//! `types.json`, with where each is declared. A frame on lines a
//! `complex-types` finding covers, or comparing a type declared there,
//! says the trace confirms that finding, and is at least as severe as it.
//!
//! A frame is reported when it took at least the threshold of the file
//! it's reported on, or at least 60% of the frame it's in when that one is
//! reported. Each finding's key, which SARIF fingerprints are made from,
//! names what the frame is about (the file, a hash of the source text it
//! spans, the types it compares), never the trace's file name, process
//! ids, type ids or positions, so it stays put from one trace to the next.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    path::{Path, PathBuf},
    ptr,
};

use oxiclean_core::{
    CheckResult, Finding, FindingKind, Issue, Related, Result, Severity, Workspace,
};
use oxiclean_typecheck::{COMPLEX_TYPES_RULE, FlaggedType};
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::{
    analyze::{Frame, FrameKind, HotType, ProjectTrace, Range, StableHash, visit},
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
    let flagged = flagged_types(workspace, trace);
    let joiner = Joiner::new(&flagged);
    let mut walker = Walker { trace, joiner: &joiner, out: Vec::new() };
    for project in &trace.projects {
        walker.walk(project, &project.frames, None);
    }
    let (findings, meta): (Vec<Finding>, Vec<Meta>) = walker.out.into_iter().unzip();
    let mut meta = meta.into_iter();
    // Whether each finding so far is reported, by its index: the findings
    // come parents first.
    let mut kept: Vec<bool> = Vec::with_capacity(findings.len());
    let result = workspace.finish_with(&policy, findings, trace.checked_files(), |f, rule| {
        let m = meta.next().unwrap_or_default();
        let parent_kept = m.parent.is_none_or(|p| kept.get(p).copied().unwrap_or(false));
        let keep = rule.enabled
            && !rule.ignored
            && (f.value >= rule.threshold || (m.by_share && parent_kept));
        kept.push(keep);
        keep.then(|| m.confirmed.map_or(rule.severity, |s| s.max(rule.severity)))
    });
    info!(findings = result.findings.len(), "{HOT_SPOTS_RULE} check complete");
    Ok(result)
}

/// What `complex-types` flags, when it's on and some frame is in a file
/// under the root; nothing, with a warning, when it can't run.
fn flagged_types(workspace: &Workspace, trace: &Trace) -> Vec<FlaggedType> {
    if !workspace.enabled(COMPLEX_TYPES_RULE) {
        return Vec::new();
    }
    let here = |raw: &str| trace.paths.here_for_sure(raw).is_some();
    let mut any_here = false;
    for project in &trace.projects {
        visit(&project.frames, &mut |f| {
            any_here = any_here
                || f.file.as_deref().is_some_and(here)
                || f.types
                    .iter()
                    .flatten()
                    .any(|t| t.ty.location.as_ref().is_some_and(|l| here(&l.path)));
        });
    }
    if !any_here {
        debug!("No hot spot is in a file under the root; not running {COMPLEX_TYPES_RULE}");
        return Vec::new();
    }
    match oxiclean_typecheck::flagged_types(workspace) {
        Ok(flagged) => flagged.unwrap_or_default(),
        Err(e) => {
            warn!(
                "{HOT_SPOTS_RULE}: {COMPLEX_TYPES_RULE} couldn't run ({e}); hot spots aren't joined to its findings"
            );
            Vec::new()
        }
    }
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

/// What deciding whether a finding is reported needs beyond its file's
/// settings.
#[derive(Debug, Default)]
struct Meta {
    /// The index of the finding of the frame it's in.
    parent: Option<usize>,
    /// Whether it takes at least 60% of the span it's in.
    by_share: bool,
    /// The severity of the most severe `complex-types` finding it confirms.
    confirmed: Option<Severity>,
}

/// Turns frames into findings, depth first.
struct Walker<'a, 'f> {
    trace: &'a Trace,
    joiner: &'a Joiner<'f>,
    out: Vec<(Finding, Meta)>,
}

/// The frame a frame is in, for its related location.
struct Parent<'f> {
    /// Its finding's index.
    index: usize,
    /// Where it was reported.
    file: PathBuf,
    line: u32,
    description: String,
    /// What it is about, for the keys of the frames in it.
    anchor: u64,
    /// The `complex-types` findings it or a frame it's in confirms, which
    /// the frames in it don't repeat.
    confirmed: Vec<&'f FlaggedType>,
}

impl<'f> Walker<'_, 'f> {
    fn walk(&mut self, project: &ProjectTrace, frames: &[Frame], parent: Option<&Parent<'f>>) {
        for frame in frames {
            match self.report(project, frame, parent) {
                Some(this) => self.walk(project, &frame.children, Some(&this)),
                None => self.walk(project, &frame.children, parent),
            }
        }
    }

    /// Records the finding for `frame`, and returns it as a parent; `None`
    /// when there's nowhere under the root to report it.
    fn report(
        &mut self,
        project: &ProjectTrace,
        frame: &Frame,
        parent: Option<&Parent<'f>>,
    ) -> Option<Parent<'f>> {
        let trace = self.trace;
        let here = frame.file.as_deref().and_then(|f| trace.paths.here(f));
        let Some((file, line)) = self.location(project, frame, here.as_deref(), parent) else {
            debug!("Leaving out a {} frame: nothing it's in is under the root", frame.event);
            return None;
        };
        // A check reported on another file than its own says which.
        let elsewhere = match (&frame.kind, frame.range, frame.file.as_deref()) {
            (FrameKind::Check, Some(Range::Lines { .. }), Some(raw))
                if here.as_ref() != Some(&file) =>
            {
                Some(trace.show_raw(raw))
            }
            _ => None,
        };
        let description = self.description(frame, elsewhere.as_deref());
        let mut message = format!("{description} took {}ms", frame.time_ms);
        if frame.unterminated {
            message.push_str(", and was still running when the trace ended");
        }
        let mut related = Vec::new();
        self.details(frame, &mut message, &mut related);
        if trace.is_build()
            && let Some(config) = project.project.config_file.as_deref()
        {
            let _ = write!(message, " (program of {})", trace.show_raw(config));
        }

        let inherited = parent.map_or(&[][..], |p| p.confirmed.as_slice());
        let confirmed = self.confirmations(frame, inherited);
        self.cite(&confirmed, &mut message, &mut related);
        if let Some(p) = parent {
            related.insert(
                0,
                Related::new(p.file.clone(), p.line, format!("inside {}", p.description)),
            );
        }

        let anchor = self.anchor(project, frame, parent);
        let issue = Issue::new(kebab(&frame.event), format!("{anchor:016x}"), message, line)
            .with_related(related);
        let value = usize::try_from(frame.time_ms).unwrap_or(usize::MAX);
        let meta = Meta {
            parent: parent.map(|p| p.index),
            by_share: frame.by_share,
            confirmed: confirmed.iter().map(|f| f.finding.severity).max(),
        };
        let index = self.out.len();
        self.out.push((Finding::new(file.clone(), FindingKind::Issue(issue), value), meta));
        let mut all = inherited.to_vec();
        all.extend(confirmed);
        Some(Parent { index, file, line, description, anchor, confirmed: all })
    }

    /// Says in `message` and `related` that the frame confirms each of
    /// `confirmed`.
    fn cite(&self, confirmed: &[&FlaggedType], message: &mut String, related: &mut Vec<Related>) {
        for f in confirmed {
            let FindingKind::Issue(issue) = &f.finding.kind else { continue };
            let _ = write!(
                message,
                "; this confirms the complex-types finding at {}:{} ({}, about {} types)",
                self.trace.show(&f.finding.file),
                issue.line,
                issue.code,
                f.finding.value
            );
            related.push(Related::new(
                f.finding.file.clone(),
                issue.line,
                format!("complex-types: {}", issue.message),
            ));
        }
    }

    /// Where `frame`, whose file is `here` when that's under the root, is
    /// reported: its own file, else where the frame it's in is, else the
    /// trace file or the root's package.json, whichever is under the root,
    /// so every location names a file in the project.
    fn location(
        &self,
        project: &ProjectTrace,
        frame: &Frame,
        here: Option<&Path>,
        parent: Option<&Parent<'f>>,
    ) -> Option<(PathBuf, u32)> {
        if let Some(file) = here {
            let line = match frame.range {
                Some(Range::Lines { start, .. }) => clamp(start.0),
                // A comparison is where the expression it's in is.
                _ if matches!(
                    frame.kind,
                    FrameKind::Compare { .. } | FrameKind::Variance { .. }
                ) =>
                {
                    parent.filter(|p| p.file == file).map_or(0, |p| p.line)
                }
                _ => 0,
            };
            return Some((file.to_path_buf(), line));
        }
        // In a file that isn't here, such as TypeScript's own lib files.
        if let Some(p) = parent {
            return Some((p.file.clone(), p.line));
        }
        let trace_file = &project.project.trace;
        if trace_file.starts_with(&self.trace.root) {
            return Some((trace_file.clone(), 0));
        }
        let manifest = self.trace.root.join("package.json");
        manifest.is_file().then_some((manifest, 0))
    }

    /// What `frame` is, in analyze-trace's words, with paths relative to
    /// the root; `elsewhere` is the file of a check reported on another.
    fn description(&self, frame: &Frame, elsewhere: Option<&str>) -> String {
        let shown = || frame.file.as_deref().map(|f| self.trace.show_raw(f)).unwrap_or_default();
        match &frame.kind {
            FrameKind::CheckFile => format!("Check file {}", shown()),
            FrameKind::EmitDeclarations { .. } => format!("Emit declarations file {}", shown()),
            FrameKind::Compare { source, target } => {
                format!("Compare types {} and {}", js(source), js(target))
            }
            FrameKind::Variance { id } => format!("Determine variance of type {}", js(id)),
            FrameKind::Check => {
                let mut what = String::new();
                unmangle(&frame.event, &mut what);
                match frame.range {
                    Some(Range::Lines { start, end }) => {
                        if let Some(file) = elsewhere {
                            let _ = write!(what, " in {file}");
                        }
                        let _ = write!(
                            what,
                            " from (line {}, char {}) to (line {}, char {})",
                            start.0, start.1, end.0, end.1
                        );
                    }
                    Some(Range::Offsets { start, end }) => {
                        let _ = write!(what, " in {} from offset {start} to offset {end}", shown());
                    }
                    None => {}
                }
                what
            }
        }
    }

    /// Adds the types a comparison or variance names, or the `import("…")`
    /// types a declaration file repeats, to `message` and `related`.
    fn details(&self, frame: &Frame, message: &mut String, related: &mut Vec<Related>) {
        match &frame.kind {
            FrameKind::Compare { .. } | FrameKind::Variance { .. } if !frame.types.is_empty() => {
                message.push_str(": ");
                for (i, t) in frame.types.iter().enumerate() {
                    if i > 0 {
                        message.push_str(" and ");
                    }
                    self.type_label(t.as_ref(), message);
                }
                for t in frame.types.iter().flatten() {
                    if let Some((file, line)) = self.type_location(t, false) {
                        related.push(Related::new(
                            file,
                            line,
                            format!("{} is declared here", type_name(t)),
                        ));
                    }
                }
            }
            FrameKind::EmitDeclarations { imports } => {
                message.push_str("; it repeats ");
                for (i, (module, n)) in imports.iter().enumerate() {
                    if i > 0 {
                        message.push_str(", ");
                    }
                    let _ = write!(message, "import({module}) {n} times");
                }
                message.push_str(
                    ": import those types by name in the source so the declarations can too",
                );
            }
            _ => {}
        }
    }

    /// `type 12 (`Name`, AliasedUnion of 7680, at src/a.ts:3:1)`.
    fn type_label(&self, t: Option<&HotType>, out: &mut String) {
        let Some(t) = t else {
            out.push_str("a type types.json doesn't have");
            return;
        };
        let _ = write!(out, "type {} (", t.ty.id);
        if let Some(name) = &t.ty.name {
            let _ = write!(out, "`{name}`, ");
        }
        out.push_str(&t.ty.kind);
        if let Some(n) = t.ty.count {
            let _ = write!(out, " of {n}");
        }
        if t.ty.name.is_none()
            && let Some(display) = &t.ty.display
        {
            let _ = write!(out, ", `{}`", shorten(display));
        }
        if let Some(loc) = &t.ty.location {
            let (line, char) = t.line_char.unwrap_or((loc.line, loc.char));
            let _ = write!(out, ", at {}:{line}:{char}", self.trace.show_raw(&loc.path));
        }
        out.push(')');
    }

    /// Where `t` is declared, when that file is under the root; `sure`
    /// asks for a file found by more than a tail of its path.
    fn type_location(&self, t: &HotType, sure: bool) -> Option<(PathBuf, u32)> {
        let loc = t.ty.location.as_ref()?;
        let file = if sure {
            self.trace.paths.here_for_sure(&loc.path)?
        } else {
            self.trace.paths.here(&loc.path)?
        };
        Some((file, clamp(t.line_char.map_or(loc.line, |l| l.0))))
    }

    /// The `complex-types` findings `frame` confirms, other than
    /// `inherited`: those on its lines, or where a type it compares is
    /// declared, each once, in order of file, line and key.
    fn confirmations(&self, frame: &Frame, inherited: &[&'f FlaggedType]) -> Vec<&'f FlaggedType> {
        let mut seen: HashSet<*const FlaggedType> =
            inherited.iter().map(|f| ptr::from_ref(*f)).collect();
        let mut found: Vec<&'f FlaggedType> = Vec::new();
        let mut add = |f: &'f FlaggedType| {
            if seen.insert(ptr::from_ref(f)) {
                found.push(f);
            }
        };
        if let Some(Range::Lines { start, end }) = frame.range
            && let Some(file) =
                frame.file.as_deref().and_then(|f| self.trace.paths.here_for_sure(f))
        {
            self.joiner.overlapping(&file, start.0, end.0).for_each(&mut add);
        }
        for t in frame.types.iter().flatten() {
            if let Some((file, line)) = self.type_location(t, true) {
                self.joiner.overlapping(&file, u64::from(line), u64::from(line)).for_each(&mut add);
            }
        }
        found.sort_by(|a, b| {
            (&a.finding.file, a.finding.line(), issue_key(a)).cmp(&(
                &b.finding.file,
                b.finding.line(),
                issue_key(b),
            ))
        });
        found
    }

    /// What `frame` is about, as a hash that stays the same from one trace
    /// of the same code to the next: the file it checks or emits; the
    /// source text it spans; or the types it compares, by name and
    /// declaration, within the frame it's in.
    fn anchor(&self, project: &ProjectTrace, frame: &Frame, parent: Option<&Parent<'f>>) -> u64 {
        let trace = self.trace;
        let mut h = StableHash::new();
        h.write(frame.event.as_bytes());
        h.write(b"\0");
        // The same file checked by two programs of a build is two findings.
        if trace.is_build()
            && let Some(config) = project.project.config_file.as_deref()
        {
            h.write(trace.show_raw(config).as_bytes());
            h.write(b"\0");
        }
        let file = frame.file.as_deref().map(|f| trace.show_raw(f)).unwrap_or_default();
        match &frame.kind {
            FrameKind::CheckFile | FrameKind::EmitDeclarations { .. } => h.write(file.as_bytes()),
            FrameKind::Check => {
                h.write(file.as_bytes());
                match (frame.text_hash, frame.range) {
                    (Some(text), _) => h.write(&text.to_le_bytes()),
                    // The file isn't here: its offsets are all there is.
                    (None, Some(Range::Offsets { start, end })) => {
                        h.write(&start.to_le_bytes());
                        h.write(&end.to_le_bytes());
                    }
                    (None, _) => h.write(&parent.map_or(0, |p| p.anchor).to_le_bytes()),
                }
            }
            FrameKind::Compare { .. } | FrameKind::Variance { .. } => {
                h.write(&parent.map_or(0, |p| p.anchor).to_le_bytes());
                if frame.types.is_empty() {
                    // No types.json: the ids are all there is.
                    for id in frame_ids(&frame.kind) {
                        h.write(js(id).as_bytes());
                        h.write(b"\0");
                    }
                }
                for t in &frame.types {
                    match t {
                        Some(t) => {
                            let ty = &t.ty;
                            h.write(ty.kind.as_bytes());
                            h.write(ty.name.as_deref().unwrap_or_default().as_bytes());
                            h.write(&ty.count.unwrap_or(0).to_le_bytes());
                            h.write(ty.display.as_deref().unwrap_or_default().as_bytes());
                            if let Some(loc) = &ty.location {
                                h.write(trace.show_raw(&loc.path).as_bytes());
                                h.write(&t.line_char.map_or(loc.line, |l| l.0).to_le_bytes());
                            }
                        }
                        None => h.write(b"?"),
                    }
                    h.write(b"\0");
                }
            }
        }
        h.finish()
    }
}

/// The ids of the types a comparison or variance names.
fn frame_ids(kind: &FrameKind) -> Vec<&Value> {
    match kind {
        FrameKind::Compare { source, target } => vec![source, target],
        FrameKind::Variance { id } => vec![id],
        _ => Vec::new(),
    }
}

fn issue_key(f: &FlaggedType) -> &str {
    match &f.finding.kind {
        FindingKind::Issue(issue) => &issue.key,
        _ => "",
    }
}

fn type_name(t: &HotType) -> String {
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

/// Writes `checkVariableDeclaration` as `Check variable declaration`, as
/// analyze-trace's `unmangleCamelCase` writes it.
fn unmangle(name: &str, out: &mut String) {
    for (i, c) in name.chars().enumerate() {
        if i == 0 {
            out.extend(c.to_uppercase());
            continue;
        }
        if c.to_lowercase().ne(std::iter::once(c)) {
            out.push(' ');
        }
        out.extend(c.to_lowercase());
    }
}

/// `checkVariableDeclaration` as `check-variable-declaration`, for the
/// finding's code.
fn kebab(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, c) in name.chars().enumerate() {
        if i > 0 && c.to_lowercase().ne(std::iter::once(c)) {
            out.push('-');
        }
        out.extend(c.to_lowercase());
    }
    out
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
mod parity;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_events_as_analyze_trace() {
        let unmangled = |name: &str| {
            let mut s = String::new();
            unmangle(name, &mut s);
            s
        };
        assert_eq!(unmangled("checkVariableDeclaration"), "Check variable declaration");
        assert_eq!(unmangled("checkExpression"), "Check expression");
        assert_eq!(kebab("checkDeferredNode"), "check-deferred-node");
        assert_eq!(kebab("structuredTypeRelatedTo"), "structured-type-related-to");
        assert_eq!(js(&Value::Null), "undefined");
        assert_eq!(js(&Value::from(12)), "12");
    }
}
