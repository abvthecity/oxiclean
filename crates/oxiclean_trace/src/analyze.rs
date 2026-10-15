//! The hot-spot tree of one program's trace, as analyze-trace builds it
//! (`buildHotPathsTree` and `getHotSpots` in its JSON mode).
//!
//! Spans at least `skipMillis` long are nested by time, per thread. A span
//! is kept when it lasts at least `forceMillis`, or at least 60% of the
//! span it's in, so a slow file shows the expression that made it slow.
//! Kept spans become frames: files checked, expressions and declarations
//! checked (with their source positions), types compared and variances
//! measured (with the types, from `types.json`), and declaration files
//! that repeat `import("…")` types. Other kept spans pass their frames up
//! to the frame they're in.

use std::{cmp::Ordering, collections::HashMap, fs, path::Path};

use serde_json::Value;
use tracing::{debug, info, warn};

use crate::{
    layout::{Project, TypeSource},
    parse::{self, Event, Parsed},
    paths::PathMap,
    trivia::{self, LineChar, RawPosition},
    types::{SimpleType, TypesFile, simplify},
};

/// A span is kept when it lasts at least this share of the span it's in.
const MIN_PERCENTAGE: f64 = 0.6;

/// How deep the tree goes. Spans nested deeper are left out, so that a
/// trace nesting thousands of spans (tsc stops relating types at 100
/// levels, but a crafted or corrupt trace needn't) can't overflow the
/// stack of the walks over it.
const MAX_DEPTH: usize = 500;

/// A declaration file is reported when it repeats an `import("…")` type at
/// least this often.
const IMPORT_EXPRESSION_THRESHOLD: usize = 10;

/// What a frame measured.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FrameKind {
    /// `checkSourceFile`.
    CheckFile,
    /// Another span of the `check` category with a source range, such as
    /// `checkExpression`, `checkVariableDeclaration` or `checkDeferredNode`.
    Check,
    /// `structuredTypeRelatedTo`: relating two types.
    Compare { source: Value, target: Value },
    /// `getVariancesWorker`: measuring a generic type's variance.
    Variance { id: Value },
    /// `emitDeclarationFileOrBundle`, for a `.d.ts` that repeats
    /// `import("…")` types: each module and how often.
    EmitDeclarations { imports: Vec<(String, usize)> },
}

/// Where a frame is in its file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Range {
    /// 1-based lines and UTF-16 columns.
    Lines { start: LineChar, end: LineChar },
    /// UTF-16 offsets, when the file isn't on this machine.
    Offsets { start: u64, end: u64 },
}

/// A type a frame involves, with its location made readable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HotType {
    pub ty: SimpleType,
    /// Where it is declared, when its file is on this machine.
    pub line_char: Option<LineChar>,
}

/// One frame of the hot-spot tree.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Frame {
    pub kind: FrameKind,
    /// The trace event, e.g. `checkExpression`.
    pub event: String,
    pub time_ms: u64,
    /// The file, as the trace names it: the file being checked, or the
    /// declaration file emitted.
    pub file: Option<String>,
    pub range: Option<Range>,
    /// For comparisons and variances: each type, `None` where `types.json`
    /// doesn't have it. Empty when there is no `types.json`.
    pub types: Vec<Option<HotType>>,
    /// Whether the span was still open when the trace ended.
    pub unterminated: bool,
    pub children: Vec<Frame>,
}

/// What one program's trace shows.
#[derive(Debug)]
pub(crate) struct ProjectTrace {
    pub project: Project,
    /// Top-level frames, slowest first.
    pub frames: Vec<Frame>,
    /// Package name to each directory it was loaded from.
    pub packages: Vec<(String, Vec<String>)>,
    pub checked_files: usize,
}

/// Settings of the hot-spot tree, in microseconds.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Thresholds {
    /// Spans at least this long are kept: analyze-trace's `forceMillis`.
    pub force: f64,
    /// Spans shorter than this are dropped: its `skipMillis`.
    pub skip: f64,
}

impl Thresholds {
    /// No hot spots at all, for when the rule is off.
    pub(crate) fn none() -> Self {
        Self { force: f64::INFINITY, skip: f64::INFINITY }
    }
}

/// A span in the tree.
struct Node {
    event: Option<Event>,
    start: f64,
    end: f64,
    unterminated: bool,
    children: Vec<usize>,
}

impl Node {
    fn duration(&self) -> f64 {
        self.end - self.start
    }
}

/// Analyses `project`'s trace.
///
/// # Errors
/// Fails if the trace can't be read or isn't a trace.
pub(crate) fn analyze(
    project: Project,
    thresholds: Thresholds,
    paths: &PathMap,
) -> std::io::Result<ProjectTrace> {
    info!("Reading {}", project.trace.display());
    let parsed = parse::parse_file(&project.trace, thresholds.skip)?;
    if !parsed.unclosed.is_empty() {
        warn!(
            "{} ended with {} spans still open; they run to its end",
            project.trace.display(),
            parsed.unclosed.len()
        );
    }
    if !parsed.unmatched_ends.is_empty() {
        debug!(
            "{} has {} unmatched end events",
            project.trace.display(),
            parsed.unmatched_ends.len()
        );
    }
    let Parsed { packages, checked_files, .. } = &parsed;
    let (packages, checked_files) = (packages.clone(), *checked_files);
    if !thresholds.force.is_finite() {
        return Ok(ProjectTrace { project, frames: Vec::new(), packages, checked_files });
    }
    let nodes = build_tree(parsed, thresholds.force);
    let mut types = TypeCache::new(&project.types);
    let mut frames = frames(&nodes, 0, None, &mut types, paths);
    resolve_positions(&mut frames, paths);
    Ok(ProjectTrace { project, frames, packages, checked_files })
}

/// The tree of spans: analyze-trace's `buildHotPathsTree`. Node 0 is the
/// whole trace.
fn build_tree(parsed: Parsed, force: f64) -> Vec<Node> {
    let Parsed { min_time, max_time, spans, unclosed, .. } = parsed;
    let mut spans: Vec<(Event, f64, f64, bool)> =
        spans.into_iter().map(|s| (s.event, s.start, s.end, false)).collect();
    for event in unclosed.into_iter().rev() {
        let start = event.ts;
        spans.push((event, start, max_time.max(start), true));
    }
    spans.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(Ordering::Equal)
            .then(b.2.partial_cmp(&a.2).unwrap_or(Ordering::Equal))
    });
    let mut nodes = vec![Node {
        event: None,
        start: min_time,
        end: max_time,
        unterminated: false,
        children: Vec::new(),
    }];
    let mut stacks: HashMap<String, Vec<usize>> = HashMap::new();
    let mut too_deep = 0usize;
    for (event, start, end, unterminated) in spans {
        let stack = stacks.entry(event.stack_key.clone()).or_insert_with(|| vec![0]);
        let mut i = stack.len() - 1;
        while i > 0 {
            let n = &nodes[stack[i]];
            if n.start <= start && end <= n.end {
                break;
            }
            i -= 1;
        }
        stack.truncate(i + 1);
        let parent = stack[i];
        let duration = end - start;
        if duration >= force || duration >= MIN_PERCENTAGE * nodes[parent].duration() {
            if stack.len() > MAX_DEPTH {
                too_deep += 1;
                continue;
            }
            let id = nodes.len();
            nodes.push(Node { event: Some(event), start, end, unterminated, children: Vec::new() });
            nodes[parent].children.push(id);
            stack.push(id);
        }
    }
    if too_deep > 0 {
        warn!("Left out {too_deep} spans nested more than {MAX_DEPTH} deep");
    }
    nodes
}

/// The `types.json` files of a program, read when a frame first needs one.
struct TypeCache<'p> {
    sources: &'p [TypeSource],
    loaded: HashMap<usize, TypesFile>,
}

impl<'p> TypeCache<'p> {
    fn new(sources: &'p [TypeSource]) -> Self {
        Self { sources, loaded: HashMap::new() }
    }

    /// The types file of the checker that wrote `event`: the only one, or
    /// the one with its `checkerId`. `None` when there's none, or it holds
    /// no types.
    fn for_event(&mut self, event: &Event) -> Option<&TypesFile> {
        let index = match self.sources {
            [] => return None,
            [_] => 0,
            sources => {
                let checker = event.arg("checkerId")?;
                sources.iter().position(|s| s.checker_id.as_ref() == Some(checker))?
            }
        };
        let types = self.loaded.entry(index).or_insert_with(|| {
            info!("Reading {}", self.sources[index].path.display());
            TypesFile::read(&self.sources[index].path)
        });
        (!types.is_empty()).then_some(&*types)
    }
}

/// The frames of node `id` and below: analyze-trace's `getHotSpotsWorker`.
fn frames(
    nodes: &[Node],
    id: usize,
    file: Option<&str>,
    types: &mut TypeCache<'_>,
    paths: &PathMap,
) -> Vec<Frame> {
    let node = &nodes[id];
    let mut file = file;
    if let Some(event) = &node.event
        && event.cat.as_deref() == Some("check")
        && let Some(path) = event.arg_str("path").filter(|p| !p.is_empty())
    {
        file = Some(path);
    }
    let mut order = node.children.clone();
    // Slowest first; the sort is stable, as JavaScript's is.
    order.sort_by(|a, b| {
        nodes[*b].duration().partial_cmp(&nodes[*a].duration()).unwrap_or(Ordering::Equal)
    });
    let children: Vec<Frame> =
        order.into_iter().flat_map(|child| frames(nodes, child, file, types, paths)).collect();
    let Some(event) = &node.event else { return children };
    match frame(event, file, types, paths) {
        Some((kind, frame_file, range, hot_types)) => vec![Frame {
            kind,
            event: event.name.clone(),
            time_ms: round_ms(node.duration()),
            file: frame_file,
            range,
            types: hot_types,
            unterminated: node.unterminated,
            children,
        }],
        None => children,
    }
}

type Made = (FrameKind, Option<String>, Option<Range>, Vec<Option<HotType>>);

/// The frame `event` makes, if any: analyze-trace's `makeHotFrame`.
fn frame(
    event: &Event,
    file: Option<&str>,
    types: &mut TypeCache<'_>,
    paths: &PathMap,
) -> Option<Made> {
    match event.name.as_str() {
        "emitDeclarationFileOrBundle" => {
            let dts = event.arg_str("declarationFilePath")?;
            let text = read_source(&paths.local(dts)?)?;
            let mut imports: Vec<(String, usize)> = trivia::count_import_types(&text)
                .into_iter()
                .filter(|(_, n)| *n >= IMPORT_EXPRESSION_THRESHOLD)
                .collect();
            imports.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            if imports.is_empty() {
                return None;
            }
            Some((FrameKind::EmitDeclarations { imports }, Some(dts.to_string()), None, Vec::new()))
        }
        "checkSourceFile" => {
            Some((FrameKind::CheckFile, Some(file?.to_string()), None, Vec::new()))
        }
        "structuredTypeRelatedTo" => {
            event.args.as_ref()?;
            let source = event.arg("sourceId").cloned().unwrap_or(Value::Null);
            let target = event.arg("targetId").cloned().unwrap_or(Value::Null);
            let hot = hot_types(types, event, &[&source, &target]);
            Some((FrameKind::Compare { source, target }, file.map(str::to_string), None, hot))
        }
        "getVariancesWorker" => {
            event.args.as_ref()?;
            let id = event.arg("id").cloned().unwrap_or(Value::Null);
            let hot = hot_types(types, event, &[&id]);
            Some((FrameKind::Variance { id }, file.map(str::to_string), None, hot))
        }
        _ => {
            if event.cat.as_deref() != Some("check") {
                return None;
            }
            let (start, end) = (event.arg_u64("pos")?, event.arg_u64("end")?);
            Some((
                FrameKind::Check,
                Some(file?.to_string()),
                Some(Range::Offsets { start, end }),
                Vec::new(),
            ))
        }
    }
}

/// The types `ids` name, from the types file of `event`'s checker; empty
/// when there's none.
fn hot_types(types: &mut TypeCache<'_>, event: &Event, ids: &[&Value]) -> Vec<Option<HotType>> {
    let Some(file) = types.for_event(event) else { return Vec::new() };
    ids.iter()
        .map(|id| {
            let id = id.as_u64()?;
            let record = file.get(id)?;
            Some(HotType { ty: simplify(id, &record), line_char: None })
        })
        .collect()
}

/// `Math.round(micros / 1000)`.
fn round_ms(micros: f64) -> u64 {
    let ms = (micros / 1000.0 + 0.5).floor();
    if ms.is_finite() && ms > 0.0 { ms as u64 } else { 0 }
}

/// Turns the offsets of `frames` and the locations of their types into
/// lines and columns, reading each file they're in once.
fn resolve_positions(frames: &mut [Frame], paths: &PathMap) {
    let mut wanted: HashMap<String, Vec<RawPosition>> = HashMap::new();
    visit(frames, &mut |f| {
        if let (Some(file), Some(Range::Offsets { start, end })) = (&f.file, f.range) {
            let w = wanted.entry(file.clone()).or_default();
            w.push(RawPosition::Start(start));
            w.push(RawPosition::End(end));
        }
        for t in f.types.iter().flatten() {
            if let Some(loc) = &t.ty.location {
                wanted
                    .entry(loc.path.clone())
                    .or_default()
                    .push(RawPosition::LineChar(loc.line, loc.char));
            }
        }
    });
    let mut resolved: HashMap<String, HashMap<RawPosition, LineChar>> = HashMap::new();
    for (file, positions) in wanted {
        let Some(local) = paths.local(&file) else {
            debug!("{file} isn't on this machine; reporting offsets");
            continue;
        };
        match read_source(&local) {
            Some(text) => {
                resolved.insert(file, trivia::normalize(&text, &positions));
            }
            None => debug!("Can't read {}", local.display()),
        }
    }
    visit_mut(frames, &mut |f| {
        if let (Some(file), Some(Range::Offsets { start, end })) = (&f.file, f.range)
            && let Some(map) = resolved.get(file)
            && let (Some(s), Some(e)) =
                (map.get(&RawPosition::Start(start)), map.get(&RawPosition::End(end)))
        {
            f.range = Some(Range::Lines { start: *s, end: *e });
        }
        for t in f.types.iter_mut().flatten() {
            if let Some(loc) = &t.ty.location {
                t.line_char = resolved
                    .get(&loc.path)
                    .and_then(|m| m.get(&RawPosition::LineChar(loc.line, loc.char)))
                    .copied();
            }
        }
    });
}

/// A source file's text, without a byte order mark, which `tsc` leaves
/// out of its offsets.
fn read_source(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    Some(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string())
}

fn visit(frames: &[Frame], f: &mut impl FnMut(&Frame)) {
    for frame in frames {
        f(frame);
        visit(&frame.children, f);
    }
}

fn visit_mut(frames: &mut [Frame], f: &mut impl FnMut(&mut Frame)) {
    for frame in frames {
        f(frame);
        visit_mut(&mut frame.children, f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn project(dir: &Path, trace: &str, types: Option<&str>) -> Project {
        fs::write(dir.join("trace.json"), trace).unwrap();
        let mut sources = Vec::new();
        if let Some(types) = types {
            fs::write(dir.join("types.json"), types).unwrap();
            sources.push(TypeSource { path: dir.join("types.json"), checker_id: None });
        }
        Project { config_file: None, trace: dir.join("trace.json"), types: sources }
    }

    /// `name (ms) [children]`, one line per frame.
    fn outline(frames: &[Frame], depth: usize, out: &mut Vec<String>) {
        for f in frames {
            out.push(format!("{}{} {}ms {:?}", "  ".repeat(depth), f.event, f.time_ms, f.range));
            outline(&f.children, depth + 1, out);
        }
    }

    #[test]
    fn nests_and_keeps_spans_as_analyze_trace() {
        let t = TempDir::new().unwrap();
        let root = t.path().canonicalize().unwrap();
        fs::write(root.join("a.ts"), "// lead\nconst x = f();\n").unwrap();
        let file = root.join("a.ts").to_string_lossy().replace('\\', "/");
        let trace = format!(
            r#"[
            {{"ph":"B","name":"checkSourceFile","cat":"check","ts":0,"args":{{"path":"{file}"}}}},
            {{"ph":"X","name":"checkVariableDeclaration","cat":"check","ts":1000,"dur":700000,"args":{{"kind":1,"pos":7,"end":21,"path":"{file}"}}}},
            {{"ph":"X","name":"checkExpression","cat":"check","ts":2000,"dur":650000,"args":{{"kind":2,"pos":17,"end":20}}}},
            {{"ph":"X","name":"structuredTypeRelatedTo","cat":"checkTypes","ts":3000,"dur":640000,"args":{{"sourceId":2,"targetId":9}}}},
            {{"ph":"X","name":"checkExpression","cat":"check","ts":800000,"dur":50000,"args":{{"kind":2,"pos":0,"end":3}}}},
            {{"ph":"E","name":"checkSourceFile","cat":"check","ts":1000000}}
        ]"#
        );
        let types = format!(
            r#"[{{"id":1,"intrinsicName":"any","flags":["Any"]}},{{"id":2,"symbolName":"X","flags":["Object"],"firstDeclaration":{{"path":"{file}","start":{{"line":1,"character":8}},"end":{{"line":2,"character":15}}}}}}]"#
        );
        let p = project(&root, &trace, Some(&types));
        let thresholds = Thresholds { force: 500_000.0, skip: 100_000.0 };
        let result = analyze(p, thresholds, &PathMap::new(&root)).unwrap();
        let mut lines = Vec::new();
        outline(&result.frames, 0, &mut lines);
        assert_eq!(
            lines,
            [
                "checkSourceFile 1000ms None",
                "  checkVariableDeclaration 700ms Some(Lines { start: (2, 1), end: (2, 14) })",
                "    checkExpression 650ms Some(Lines { start: (2, 11), end: (2, 13) })",
                "      structuredTypeRelatedTo 640ms None",
            ]
        );
        let compare = &result.frames[0].children[0].children[0].children[0];
        assert_eq!(compare.kind, FrameKind::Compare { source: 2.into(), target: 9.into() });
        let source = compare.types[0].as_ref().unwrap();
        assert_eq!((source.ty.name.as_deref(), source.line_char), (Some("X"), Some((2, 1))));
        assert!(compare.types[1].is_none(), "type 9 isn't in types.json");
        assert_eq!(result.checked_files, 1);
    }

    #[test]
    fn spans_left_open_run_to_the_end() {
        let t = TempDir::new().unwrap();
        let root = t.path().to_path_buf();
        let trace = r#"[
            {"ph":"B","name":"checkSourceFile","cat":"check","ts":0,"args":{"path":"/gone/a.ts"}},
            {"ph":"X","name":"checkExpression","cat":"check","ts":1000,"dur":900000,"args":{"pos":5,"end":9}},
            {"ph":"B","name":"checkExpression","cat":"check","ts":950000,"args":{"pos":12,"end":30}},
            {"ph":"X","name":"emit","cat":"emit","ts":960000,"dur":1000}
        "#;
        let p = project(&root, trace, None);
        let thresholds = Thresholds { force: 500_000.0, skip: 0.0 };
        let result = analyze(p, thresholds, &PathMap::new(&root)).unwrap();
        let mut lines = Vec::new();
        outline(&result.frames, 0, &mut lines);
        assert_eq!(
            lines,
            [
                "checkSourceFile 961ms None",
                "  checkExpression 900ms Some(Offsets { start: 5, end: 9 })",
            ]
        );
        assert!(result.frames[0].unterminated);
    }

    #[test]
    fn spans_nested_too_deep_are_left_out() {
        let t = TempDir::new().unwrap();
        let root = t.path().to_path_buf();
        let n = 5000;
        let mut events = Vec::new();
        for i in 0..n {
            events.push(format!(
                r#"{{"ph":"B","name":"checkExpression","cat":"check","ts":{i},"args":{{"pos":1,"end":2,"path":"/a.ts"}}}}"#
            ));
        }
        for i in 0..n {
            events.push(format!(
                r#"{{"ph":"E","name":"checkExpression","cat":"check","ts":{}}}"#,
                1_000_000 + i
            ));
        }
        let p = project(&root, &format!("[{}]", events.join(",")), None);
        let thresholds = Thresholds { force: 1000.0, skip: 0.0 };
        let result = analyze(p, thresholds, &PathMap::new(&root)).unwrap();
        let mut depth = 0;
        let mut level = &result.frames;
        while let Some(f) = level.first() {
            depth += 1;
            level = &f.children;
        }
        assert_eq!(depth, MAX_DEPTH);
    }
}
