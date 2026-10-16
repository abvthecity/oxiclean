//! Parity with analyze-trace, checked on every test run, against
//! `analyze-trace.json`, what the real `@typescript/analyze-trace` 0.11.1
//! reports on each trace fixture's committed trace at the fixture's
//! settings (`tests/fixtures/regenerate.mjs --golden` writes it):
//!
//! - the hot-spot tree, frame by frame: what it is, its file, its
//!   position, its time, and the types a comparison or variance names
//!   (their ids, kinds, names, sizes and where they're declared);
//! - what the rules report: a `hot-spots` finding for every frame, with
//!   its time, and a `duplicate-packages` finding for every duplicate
//!   package, with each copy's version.
//!
//! The fixtures are traced by TypeScript 5 (pnpm, npm, Bun and Deno
//! layouts) and by TypeScript 7 (`tsgo`: one trace for every checker, a
//! types file per checker, and lowercased paths, as on macOS).
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

use oxiclean_core::{CommonArgs, FindingKind, Workspace};
use serde_json::Value;

use super::{js, unmangle};
use crate::{
    analyze::{Frame, FrameKind, HotType, ProjectTrace, Range},
    duplicates,
    trace::Trace,
};

/// Every trace fixture.
const FIXTURES: [&str; 5] = ["pnpm", "npm", "bun", "deno", "tsgo"];

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
        if let Some(theirs) = node["types"].as_array() {
            let ours: Vec<Option<String>> =
                frame.types.iter().map(|t| t.as_ref().map(|t| type_label(trace, t))).collect();
            let theirs: Vec<Option<String>> = theirs.iter().map(golden_type_label).collect();
            if ours != theirs {
                problems.push(format!("{here}: oxiclean types {ours:?}, analyze-trace {theirs:?}"));
            }
        }
        let children = node["children"].as_array().map_or(&[][..], Vec::as_slice);
        compare(trace, known, &frame.children, children, &here, problems, allowed);
    }
}

/// A type of a comparison or variance as analyze-trace's JSON gives it:
/// `90 TypeParameter K packages/web/src/paths.ts:2:8`.
fn golden_type_label(node: &Value) -> Option<String> {
    let t = node.get("type").filter(|t| !t.is_null())?;
    let mut out = format!("{} {}", t["id"], t["kind"].as_str().unwrap_or_default());
    if let Some(name) = t["name"].as_str() {
        let _ = write!(out, " {name}");
    }
    if let Some(n) = t["count"].as_u64() {
        let _ = write!(out, " of {n}");
    }
    if let Some(path) = t["location"]["path"].as_str() {
        let path = path.replace("<root>/", "");
        let _ = write!(out, " {path}:{}:{}", t["location"]["line"], t["location"]["char"]);
    }
    Some(out)
}

/// One of oxiclean's types, labelled as [`golden_type_label`] does.
fn type_label(trace: &Trace, t: &HotType) -> String {
    let ty = &t.ty;
    let mut out = format!("{} {}", ty.id, ty.kind);
    if let Some(name) = &ty.name {
        let _ = write!(out, " {name}");
    }
    if let Some(n) = ty.count {
        let _ = write!(out, " of {n}");
    }
    if let Some(loc) = &ty.location {
        let (line, char) = t.line_char.unwrap_or((loc.line, loc.char));
        let _ = write!(out, " {}:{line}:{char}", trace.show_raw(&loc.path));
    }
    out
}

/// What a frame is, as the first two words of how both tools describe
/// it, and its time: `Check expression 71ms`.
fn kind_and_time(description: &str, ms: u64) -> String {
    let words: Vec<&str> = description.split_whitespace().take(2).collect();
    format!("{} {ms}ms", words.join(" "))
}

/// Each frame of analyze-trace's `nodes` and below, as [`kind_and_time`].
fn golden_frames(nodes: &[Value], out: &mut Vec<String>) {
    for node in nodes {
        let description = node["description"].as_str().unwrap_or_default();
        out.push(kind_and_time(description, node["timeMs"].as_u64().unwrap_or_default()));
        golden_frames(node["children"].as_array().map_or(&[][..], Vec::as_slice), out);
    }
}

fn load(name: &str) -> (Workspace, Trace, Value) {
    let root = fixture(name);
    let workspace = CommonArgs::with_root(&root).workspace().unwrap();
    let trace = Trace::load(&workspace, &root.join("trace"), true).unwrap();
    let golden: Value =
        serde_json::from_str(&fs::read_to_string(root.join("analyze-trace.json")).unwrap())
            .unwrap();
    (workspace, trace, golden)
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
    let mut compared_types = 0;
    for name in FIXTURES {
        let (_, trace, golden) = load(name);
        for p in &trace.projects {
            crate::analyze::visit(&p.frames, &mut |f| {
                compared_types += f.types.iter().flatten().count();
            });
        }
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
    assert!(compared_types > 0, "no fixture compares types");
}

/// What the rules report matches what analyze-trace finds: a `hot-spots`
/// finding for each of its frames, and a `duplicate-packages` finding for
/// each of its duplicate packages, so no frame of the tree is lost on the
/// way to the report.
#[test]
fn reported_findings_match_analyze_trace() {
    for name in FIXTURES {
        let (workspace, trace, golden) = load(name);
        let results = golden["results"].as_array().unwrap();

        let mut theirs = Vec::new();
        for result in results {
            let nodes = result["highlights"]["hotSpots"].as_array().map_or(&[][..], Vec::as_slice);
            golden_frames(nodes, &mut theirs);
        }
        let hot = super::run_on(&workspace, &trace).unwrap();
        let mut ours: Vec<String> = hot
            .findings
            .iter()
            .map(|f| {
                let FindingKind::Issue(issue) = &f.kind else { panic!("{f:?}") };
                // `line 4: Check expression …` names the line first.
                let message =
                    issue.message.split_once(": ").map_or(&*issue.message, |(lead, rest)| {
                        if lead.starts_with("line ") { rest } else { &issue.message }
                    });
                kind_and_time(message, f.value as u64)
            })
            .collect();
        theirs.sort();
        ours.sort();
        assert!(!theirs.is_empty(), "{name}: analyze-trace finds no hot spots");
        assert_eq!(ours, theirs, "{name}: the hot spots reported differ from analyze-trace's");
        assert!(hot.notes.is_empty(), "{name}: {:?}", hot.notes);

        let dups = duplicates::run_on(&workspace, &trace).unwrap();
        for result in results {
            for package in
                result["highlights"]["duplicatePackages"].as_array().into_iter().flatten()
            {
                let package_name = package["name"].as_str().unwrap();
                let issue = dups
                    .findings
                    .iter()
                    .find_map(|f| match &f.kind {
                        FindingKind::Issue(i) if i.key == package_name => Some(i),
                        _ => None,
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "{name}: analyze-trace finds {package_name} loaded twice, oxiclean doesn't"
                        )
                    });
                for instance in package["instances"].as_array().into_iter().flatten() {
                    if let Some(version) = instance["version"].as_str() {
                        assert!(
                            issue.message.contains(&format!("{version} at ")),
                            "{name}: {package_name} {version} isn't in {:?}",
                            issue.message
                        );
                    }
                }
            }
        }
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
