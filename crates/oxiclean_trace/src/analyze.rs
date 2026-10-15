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

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
};

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

impl FrameKind {
    /// The ids of the types a comparison or variance names.
    fn type_ids(&self) -> Vec<&Value> {
        match self {
            Self::Compare { source, target } => vec![source, target],
            Self::Variance { id } => vec![id],
            _ => Vec::new(),
        }
    }
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
    pub event: Box<str>,
    pub time_ms: u64,
    /// The file, as the trace names it: the file being checked, or the
    /// declaration file emitted.
    pub file: Option<String>,
    pub range: Option<Range>,
    /// The UTF-16 offsets the trace gives a check's span, before they're
    /// made lines and columns.
    pub offsets: Option<(u64, u64)>,
    /// For comparisons and variances: each type, `None` where `types.json`
    /// doesn't have it. Empty when there is no `types.json`.
    pub types: Vec<Option<HotType>>,
    /// Whether the span was still open when the trace ended.
    pub unterminated: bool,
    /// Whether the span takes at least 60% of the span it's in, which
    /// keeps it whatever the threshold.
    pub by_share: bool,
    /// A hash of the source text the frame spans, when its file is here:
    /// what it is about, wherever that moves.
    pub text_hash: Option<u64>,
    /// Which of the program's types files has the types it names.
    pub types_source: Option<usize>,
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
    by_share: bool,
    children: Vec<usize>,
}

impl Node {
    fn duration(&self) -> f64 {
        self.end - self.start
    }
}

/// Reads `project`'s trace, keeping spans at least `thresholds.skip`
/// long.
///
/// # Errors
/// Fails if the trace can't be read or isn't a trace.
pub(crate) fn parse(project: &Project, thresholds: Thresholds) -> std::io::Result<Parsed> {
    info!("Reading {}", project.trace.display());
    let parsed = parse::parse_file(&project.trace, thresholds.skip)?;
    if !parsed.unclosed.is_empty() {
        warn!(
            "{} ended with {} spans still open; they run to its end",
            project.trace.display(),
            parsed.unclosed.len()
        );
    }
    if parsed.unmatched_ends > 0 {
        debug!("{} has {} unmatched end events", project.trace.display(), parsed.unmatched_ends);
    }
    Ok(parsed)
}

/// Builds `project`'s hot-spot tree from its `parsed` trace.
pub(crate) fn analyze(
    project: Project,
    mut parsed: Parsed,
    thresholds: Thresholds,
    paths: &PathMap,
) -> ProjectTrace {
    let packages = std::mem::take(&mut parsed.packages);
    let checked_files = parsed.checked_files;
    if !thresholds.force.is_finite() {
        return ProjectTrace { project, frames: Vec::new(), packages, checked_files };
    }
    let nodes = build_tree(parsed, thresholds.force);
    let mut frames = frames(&nodes, 0, None, &project.types, paths);
    add_types(&mut frames, &project.types);
    resolve_positions(&mut frames, paths);
    ProjectTrace { project, frames, packages, checked_files }
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
        by_share: false,
        children: Vec::new(),
    }];
    let mut stacks: HashMap<u32, Vec<usize>> = HashMap::new();
    let mut too_deep = 0usize;
    for (event, start, end, unterminated) in spans {
        let stack = stacks.entry(event.stack).or_insert_with(|| vec![0]);
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
        let by_share = duration >= MIN_PERCENTAGE * nodes[parent].duration();
        if duration >= force || by_share {
            if stack.len() > MAX_DEPTH {
                too_deep += 1;
                continue;
            }
            let id = nodes.len();
            nodes.push(Node {
                event: Some(event),
                start,
                end,
                unterminated,
                by_share,
                children: Vec::new(),
            });
            nodes[parent].children.push(id);
            stack.push(id);
        }
    }
    if too_deep > 0 {
        warn!("Left out {too_deep} spans nested more than {MAX_DEPTH} deep");
    }
    nodes
}

/// The types file of the checker that wrote `event`: the only one, or the
/// one with its `checkerId`.
fn types_source(sources: &[TypeSource], event: &Event) -> Option<usize> {
    match sources {
        [] => None,
        [_] => Some(0),
        sources => {
            let checker = event.arg("checkerId")?;
            sources.iter().position(|s| s.checker_id.as_ref() == Some(checker))
        }
    }
}

/// The frames of node `id` and below: analyze-trace's `getHotSpotsWorker`.
fn frames(
    nodes: &[Node],
    id: usize,
    file: Option<&str>,
    sources: &[TypeSource],
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
        order.into_iter().flat_map(|child| frames(nodes, child, file, sources, paths)).collect();
    let Some(event) = &node.event else { return children };
    match frame(event, file, paths) {
        Some((kind, frame_file, range)) => vec![Frame {
            types_source: if kind.type_ids().is_empty() {
                None
            } else {
                types_source(sources, event)
            },
            kind,
            event: event.name.clone(),
            time_ms: round_ms(node.duration()),
            file: frame_file,
            offsets: match range {
                Some(Range::Offsets { start, end }) => Some((start, end)),
                _ => None,
            },
            range,
            types: Vec::new(),
            unterminated: node.unterminated,
            by_share: node.by_share,
            text_hash: None,
            children,
        }],
        None => children,
    }
}

/// The frame `event` makes, if any: analyze-trace's `makeHotFrame`.
fn frame(
    event: &Event,
    file: Option<&str>,
    paths: &PathMap,
) -> Option<(FrameKind, Option<String>, Option<Range>)> {
    match &*event.name {
        "emitDeclarationFileOrBundle" => {
            let dts = event.arg_str("declarationFilePath")?;
            let text = paths.read_source(&paths.local(dts)?)?;
            let mut imports: Vec<(String, usize)> = trivia::count_import_types(&text)
                .into_iter()
                .filter(|(_, n)| *n >= IMPORT_EXPRESSION_THRESHOLD)
                .collect();
            imports.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            if imports.is_empty() {
                return None;
            }
            Some((FrameKind::EmitDeclarations { imports }, Some(dts.to_string()), None))
        }
        "checkSourceFile" => Some((FrameKind::CheckFile, Some(file?.to_string()), None)),
        "structuredTypeRelatedTo" => {
            event.args.as_ref()?;
            let source = event.arg("sourceId").cloned().unwrap_or(Value::Null);
            let target = event.arg("targetId").cloned().unwrap_or(Value::Null);
            Some((FrameKind::Compare { source, target }, file.map(str::to_string), None))
        }
        "getVariancesWorker" => {
            event.args.as_ref()?;
            let id = event.arg("id").cloned().unwrap_or(Value::Null);
            Some((FrameKind::Variance { id }, file.map(str::to_string), None))
        }
        _ => {
            if event.cat.as_deref() != Some("check") {
                return None;
            }
            let (start, end) = (event.arg_u64("pos")?, event.arg_u64("end")?);
            Some((FrameKind::Check, Some(file?.to_string()), Some(Range::Offsets { start, end })))
        }
    }
}

/// Names the types of comparisons and variances from `sources`, reading
/// each types file once for just the types the frames name. A frame whose
/// checker has no types file, or one with no types, names none.
fn add_types(frames: &mut [Frame], sources: &[TypeSource]) {
    let mut wanted: HashMap<usize, HashSet<u64>> = HashMap::new();
    visit(frames, &mut |f| {
        if let Some(source) = f.types_source {
            let ids = wanted.entry(source).or_default();
            ids.extend(f.kind.type_ids().iter().filter_map(|id| id.as_u64()));
        }
    });
    if wanted.is_empty() {
        return;
    }
    let loaded: HashMap<usize, TypesFile> = wanted
        .into_iter()
        .map(|(source, ids)| {
            info!("Reading {} for {} types", sources[source].path.display(), ids.len());
            (source, TypesFile::read(&sources[source].path, &ids))
        })
        .collect();
    visit_mut(frames, &mut |f| {
        let Some(file) = f.types_source.and_then(|s| loaded.get(&s)).filter(|t| !t.is_empty())
        else {
            return;
        };
        f.types = f
            .kind
            .type_ids()
            .iter()
            .map(|id| {
                let id = id.as_u64()?;
                Some(HotType { ty: simplify(id, file.get(id)?), line_char: None })
            })
            .collect();
    });
}

/// `Math.round(micros / 1000)`.
fn round_ms(micros: f64) -> u64 {
    let ms = (micros / 1000.0 + 0.5).floor();
    if ms.is_finite() && ms > 0.0 { ms as u64 } else { 0 }
}

/// The positions in one file to make readable, and the ranges of offsets
/// whose text to hash.
type Wanted = (Vec<RawPosition>, Vec<(u64, u64)>);

/// What one file's text says about the frames in it.
struct Resolved {
    positions: HashMap<RawPosition, LineChar>,
    /// The hash of the text of each range of offsets.
    hashes: HashMap<(u64, u64), u64>,
}

/// Turns the offsets of `frames` and the locations of their types into
/// lines and columns, and hashes the text each frame spans, reading each
/// file they're in once.
fn resolve_positions(frames: &mut [Frame], paths: &PathMap) {
    // Per file: the positions to resolve, and the ranges to hash.
    let mut wanted: HashMap<String, Wanted> = HashMap::new();
    visit(frames, &mut |f| {
        if let (Some(file), Some(Range::Offsets { start, end })) = (&f.file, f.range) {
            let w = wanted.entry(file.clone()).or_default();
            w.0.push(RawPosition::Start(start));
            w.0.push(RawPosition::End(end));
            w.1.push((start, end));
        }
        for t in f.types.iter().flatten() {
            if let Some(loc) = &t.ty.location {
                wanted
                    .entry(loc.path.clone())
                    .or_default()
                    .0
                    .push(RawPosition::LineChar(loc.line, loc.char));
            }
        }
    });
    let mut resolved: HashMap<String, Resolved> = HashMap::new();
    for (file, (positions, ranges)) in wanted {
        let Some(local) = paths.here(&file) else {
            debug!("{file} isn't under the root; reporting offsets");
            continue;
        };
        match paths.read_source(&local) {
            Some(text) => {
                let hashes = hash_ranges(&text, ranges);
                resolved.insert(
                    file,
                    Resolved { positions: trivia::normalize(&text, &positions), hashes },
                );
            }
            None => debug!("Can't read {}", local.display()),
        }
    }
    visit_mut(frames, &mut |f| {
        if let (Some(file), Some(Range::Offsets { start, end })) = (&f.file, f.range)
            && let Some(r) = resolved.get(file)
            && let (Some(s), Some(e)) = (
                r.positions.get(&RawPosition::Start(start)),
                r.positions.get(&RawPosition::End(end)),
            )
        {
            f.range = Some(Range::Lines { start: *s, end: *e });
            f.text_hash = r.hashes.get(&(start, end)).copied();
        }
        for t in f.types.iter_mut().flatten() {
            if let Some(loc) = &t.ty.location {
                t.line_char = resolved
                    .get(&loc.path)
                    .and_then(|r| r.positions.get(&RawPosition::LineChar(loc.line, loc.char)))
                    .copied();
            }
        }
    });
}

/// The hash of the text of each of `ranges`, UTF-16 offsets into `text`,
/// with its whitespace collapsed.
fn hash_ranges(text: &str, ranges: Vec<(u64, u64)>) -> HashMap<(u64, u64), u64> {
    if ranges.is_empty() {
        return HashMap::new();
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    ranges
        .into_iter()
        .map(|(start, end)| {
            let at = |o: u64| usize::try_from(o).unwrap_or(usize::MAX).min(units.len());
            let slice = String::from_utf16_lossy(&units[at(start)..at(end).max(at(start))]);
            let mut hash = StableHash::new();
            for word in slice.split_whitespace() {
                hash.write(word.as_bytes());
                hash.write(b" ");
            }
            ((start, end), hash.finish())
        })
        .collect()
}

/// FNV-1a: a hash that is the same on every machine and Rust version, for
/// identities that must stay put across runs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StableHash(u64);

impl StableHash {
    pub(crate) fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    pub(crate) fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= u64::from(*b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    pub(crate) fn finish(self) -> u64 {
        self.0
    }
}

pub(crate) fn visit(frames: &[Frame], f: &mut impl FnMut(&Frame)) {
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
    use std::{fs, path::Path};
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

    fn run(p: Project, thresholds: Thresholds, root: &Path) -> ProjectTrace {
        let parsed = parse(&p, thresholds).unwrap();
        analyze(p, parsed, thresholds, &PathMap::new(root))
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
        let result = run(p, thresholds, &root);
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
        let root = t.path().canonicalize().unwrap();
        let trace = r#"[
            {"ph":"B","name":"checkSourceFile","cat":"check","ts":0,"args":{"path":"/gone/a.ts"}},
            {"ph":"X","name":"checkExpression","cat":"check","ts":1000,"dur":900000,"args":{"pos":5,"end":9}},
            {"ph":"B","name":"checkExpression","cat":"check","ts":950000,"args":{"pos":12,"end":30}},
            {"ph":"X","name":"emit","cat":"emit","ts":960000,"dur":1000}
        "#;
        let p = project(&root, trace, None);
        let thresholds = Thresholds { force: 500_000.0, skip: 0.0 };
        let result = run(p, thresholds, &root);
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
        let root = t.path().canonicalize().unwrap();
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
        let result = run(p, thresholds, &root);
        let mut depth = 0;
        let mut level = &result.frames;
        while let Some(f) = level.first() {
            depth += 1;
            level = &f.children;
        }
        assert_eq!(depth, MAX_DEPTH);
    }
}
