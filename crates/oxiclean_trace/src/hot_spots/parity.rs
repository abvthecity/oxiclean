//! Parity with analyze-trace, checked on every test run: each trace
//! fixture's hot-spot tree, frame by frame (what it is, its file, its
//! position and its time), against `analyze-trace.json`, what the real
//! `@typescript/analyze-trace` 0.11.1 reports on the same committed trace
//! at the fixture's settings (`tests/fixtures/regenerate.mjs --golden`
//! writes it).
//!
//! Two known divergences, both analyze-trace's, are allowed where a
//! position differs and nothing else does:
//!
//! - a file that starts with a byte order mark: analyze-trace counts the
//!   mark in its columns, which come out one or two too high, while `tsc`
//!   leaves it out of its offsets;
//! - a span that starts at the offset another span in the file ends at:
//!   analyze-trace keeps both positions in one map keyed by offset, so one
//!   of them gets the other's line and column.
//!
//! The fixtures have neither today; a regenerated trace may.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use oxiclean_core::CommonArgs;
use serde_json::Value;

use super::{js, unmangle};
use crate::{
    analyze::{Frame, FrameKind, ProjectTrace, Range},
    trace::Trace,
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

/// A frame as analyze-trace's JSON describes it, without its time:
/// `Check expression packages/a.ts 4:18-4:56`.
#[derive(Debug, PartialEq, Eq)]
struct Label {
    what: String,
    position: Option<String>,
}

/// The label of a node of analyze-trace's JSON.
fn golden_label(node: &Value) -> Label {
    let local = |s: &str| s.replace("<root>/", "");
    let mut what = local(node["description"].as_str().unwrap_or_default());
    let description_has_path = what.starts_with("Check file ");
    if let Some(path) = node["path"].as_str().filter(|_| !description_has_path) {
        let _ = write!(what, " {}", local(path));
    }
    let position = if node.get("startLine").is_some() {
        Some(format!(
            "{}:{}-{}:{}",
            node["startLine"], node["startChar"], node["endLine"], node["endChar"]
        ))
    } else {
        node.get("startOffset").map(|_| format!("@{}-{}", node["startOffset"], node["endOffset"]))
    };
    Label { what, position }
}

/// The label of one of oxiclean's frames.
fn label(trace: &Trace, frame: &Frame) -> Label {
    let file = frame.file.as_deref().map(|f| trace.show_raw(f));
    let mut what = String::new();
    match &frame.kind {
        FrameKind::CheckFile => {
            let _ = write!(what, "Check file {}", file.unwrap_or_default());
        }
        FrameKind::EmitDeclarations { .. } => {
            let _ = write!(what, "Emit declarations file {}", file.unwrap_or_default());
        }
        FrameKind::Compare { source, target } => {
            let _ = write!(what, "Compare types {} and {}", js(source), js(target));
        }
        FrameKind::Variance { id } => {
            let _ = write!(what, "Determine variance of type {}", js(id));
        }
        FrameKind::Check => {
            unmangle(&frame.event, &mut what);
            let _ = write!(what, " {}", file.unwrap_or_default());
        }
    }
    let position = frame.range.map(|r| match r {
        Range::Lines { start, end } => format!("{}:{}-{}:{}", start.0, start.1, end.0, end.1),
        Range::Offsets { start, end } => format!("@{start}-{end}"),
    });
    Label { what, position }
}

/// Where analyze-trace's positions for `frame` are known to be off.
struct Divergences {
    /// Files under the root that start with a byte order mark.
    bom: HashSet<String>,
    /// Each file's span ends, as offsets.
    ends: HashMap<String, HashSet<u64>>,
}

impl Divergences {
    fn new(trace: &Trace, frames: &[Frame]) -> Self {
        let mut ends: HashMap<String, HashSet<u64>> = HashMap::new();
        let mut bom = HashSet::new();
        crate::analyze::visit(frames, &mut |f| {
            if let (Some(file), Some((_, end))) = (&f.file, f.offsets) {
                ends.entry(file.clone()).or_default().insert(end);
                if let Some(local) = trace.paths.here(file)
                    && fs::read(&local).is_ok_and(|b| b.starts_with("\u{feff}".as_bytes()))
                {
                    bom.insert(file.clone());
                }
            }
        });
        Self { bom, ends }
    }

    /// Why analyze-trace's position for `frame` may differ, if it may.
    fn reason(&self, frame: &Frame) -> Option<&'static str> {
        let file = frame.file.as_ref()?;
        if self.bom.contains(file) {
            return Some("the file starts with a byte order mark");
        }
        let (start, _) = frame.offsets?;
        self.ends
            .get(file)
            .is_some_and(|ends| ends.contains(&start))
            .then_some("its start is another span's end")
    }
}

/// Compares `frames` with analyze-trace's `nodes`, in order, adding each
/// difference to `problems`.
fn compare(
    trace: &Trace,
    known: &Divergences,
    frames: &[Frame],
    nodes: &[Value],
    path: &str,
    problems: &mut Vec<String>,
    allowed: &mut Vec<String>,
) {
    if frames.len() != nodes.len() {
        problems.push(format!(
            "{path}: oxiclean has {} frames, analyze-trace {}",
            frames.len(),
            nodes.len()
        ));
    }
    for (frame, node) in frames.iter().zip(nodes) {
        let (ours, theirs) = (label(trace, frame), golden_label(node));
        let here = format!("{path} > {}", theirs.what);
        if ours.what != theirs.what {
            problems.push(format!("{path}: oxiclean {ours:?}, analyze-trace {theirs:?}"));
            continue;
        }
        if ours.position != theirs.position {
            match known.reason(frame) {
                Some(why) => allowed.push(format!("{here}: {why}")),
                None => problems.push(format!(
                    "{here}: oxiclean at {:?}, analyze-trace at {:?}",
                    ours.position, theirs.position
                )),
            }
        }
        if Some(frame.time_ms) != node["timeMs"].as_u64() {
            problems.push(format!(
                "{here}: oxiclean {}ms, analyze-trace {}ms",
                frame.time_ms, node["timeMs"]
            ));
        }
        let children = node["children"].as_array().map_or(&[][..], Vec::as_slice);
        compare(trace, known, &frame.children, children, &here, problems, allowed);
    }
}

/// The project of `trace` analyze-trace's `result` is about.
fn project_of<'t>(trace: &'t Trace, result: &Value) -> &'t ProjectTrace {
    let name = result["project"]["tracePath"].as_str().unwrap().replace("<trace>/", "");
    trace
        .projects
        .iter()
        .find(|p| p.project.trace.file_name().is_some_and(|n| n.to_string_lossy() == name))
        .unwrap_or_else(|| panic!("no program reads {name}"))
}

#[test]
fn hot_spots_match_analyze_trace() {
    for name in ["pnpm", "npm", "bun", "deno"] {
        let root = fixture(name);
        let workspace = CommonArgs::with_root(&root).workspace().unwrap();
        let trace = Trace::load(&workspace, &root.join("trace"), true).unwrap();
        let golden: Value =
            serde_json::from_str(&fs::read_to_string(root.join("analyze-trace.json")).unwrap())
                .unwrap();
        let results = golden["results"].as_array().unwrap();
        assert_eq!(results.len(), trace.projects.len(), "{name}");
        let (mut problems, mut allowed) = (Vec::new(), Vec::new());
        let mut frames = 0;
        for result in results {
            let project = project_of(&trace, result);
            let known = Divergences::new(&trace, &project.frames);
            let nodes = result["highlights"]["hotSpots"].as_array().map_or(&[][..], Vec::as_slice);
            crate::analyze::visit(&project.frames, &mut |_| frames += 1);
            compare(&trace, &known, &project.frames, nodes, name, &mut problems, &mut allowed);
        }
        assert!(frames > 0, "{name}: no hot spots to compare");
        assert!(problems.is_empty(), "{name} differs from analyze-trace:\n{}", problems.join("\n"));
        // None of the fixtures has a known divergence today.
        assert!(allowed.is_empty(), "{name}: {allowed:?}");
    }
}

#[test]
fn known_divergences_are_recognised() {
    let root = fixture("bun");
    let workspace = CommonArgs::with_root(&root).workspace().unwrap();
    let trace = Trace::load(&workspace, &root.join("trace"), true).unwrap();
    let frame = |file: &str, offsets| {
        Frame { offsets: Some(offsets), ..trace.projects[0].frames[0].children[0].clone() }
            .with_file(file)
    };
    let a = frame("/x/a.ts", (10, 20));
    let b = frame("/x/a.ts", (20, 30));
    let known = Divergences::new(&trace, &[a.clone(), b.clone()]);
    assert_eq!(known.reason(&b), Some("its start is another span's end"));
    assert_eq!(known.reason(&a), None);
}

impl Frame {
    fn with_file(mut self, file: &str) -> Self {
        self.file = Some(file.to_string());
        self
    }
}
