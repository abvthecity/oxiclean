//! Runs the `trace` command on one small repo per package manager in
//! `tests/fixtures`: pnpm, npm, Bun and Deno. Each holds a real trace of
//! itself, written by `tsc --generateTrace` and trimmed by
//! `tests/fixtures/regenerate.mjs`; the timings below are the ones
//! recorded there, and change when the traces are regenerated.
//!
//! The traces were recorded at other paths than the fixtures' (pnpm's in
//! `/home/ci/pnpm-repo`, Bun's in `/Users/dev/bun-repo`, Deno's on
//! Windows, with lowercased paths), so each test also checks that the
//! files they name are found here.

use std::{
    fs,
    path::{Path, PathBuf},
};

use oxiclean_core::{CommandResult, CommonArgs, FindingKind, Severity};
use oxiclean_trace::{Config, run_trace_command};
use tempfile::TempDir;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn run(root: &Path, dir: Option<PathBuf>) -> CommandResult {
    let cfg = Config { dir, common: CommonArgs::with_root(root) };
    run_trace_command(&cfg).expect("the trace is read")
}

/// Each finding of `rule` as `file:line code (value) severity`, with
/// `confirmed` when it confirms a complex-types finding.
fn summary(result: &CommandResult, rule: &str) -> Vec<String> {
    let r = result.rules.iter().find(|r| r.rule == rule).expect("the rule ran");
    assert!(r.error.is_none(), "{rule}: {:?}", r.error);
    r.result
        .findings
        .iter()
        .map(|f| {
            let FindingKind::Issue(issue) = &f.kind else { panic!("{f:?}") };
            let file = f.file.strip_prefix(&result.root).unwrap_or(&f.file);
            let file = file.to_string_lossy().replace('\\', "/");
            let severity = match f.severity {
                Severity::Warn => "warn",
                Severity::Error => "error",
            };
            let confirmed = if issue.message.contains("confirms the complex-types finding") {
                " confirmed"
            } else {
                ""
            };
            format!("{file}:{} {} ({}) {severity}{confirmed}", issue.line, issue.code, f.value)
        })
        .collect()
}

/// The message of the finding of `rule` whose message has `needle`, and
/// its related locations as `file:line message`.
fn finding(result: &CommandResult, rule: &str, needle: &str) -> (String, Vec<String>) {
    let r = result.rules.iter().find(|r| r.rule == rule).unwrap();
    r.result
        .findings
        .iter()
        .find_map(|f| match &f.kind {
            FindingKind::Issue(issue) if issue.message.contains(needle) => Some((
                issue.message.clone(),
                issue
                    .related
                    .iter()
                    .map(|rel| {
                        let file = rel.file.strip_prefix(&result.root).unwrap_or(&rel.file);
                        let file = file.to_string_lossy().replace('\\', "/");
                        format!("{file}:{} {}", rel.line, rel.message)
                    })
                    .collect(),
            )),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no {rule} finding mentions {needle}"))
}

/// pnpm, traced with `tsc --build` (legend.json and one trace per
/// program): an inline intersection of unions that complex-types flags
/// and the trace confirms, the declarations of a package that repeat
/// `import("@acme/ui")`, and two csstype versions in the store.
#[test]
fn pnpm() {
    let result = run(&fixture("pnpm"), None);
    assert_eq!(
        summary(&result, "hot-spots"),
        [
            "packages/app/dist/index.d.ts:0 emit-declaration-file-or-bundle (12) warn",
            "packages/ui/src/classes.ts:0 check-source-file (54) warn",
            "packages/ui/src/use.ts:0 check-source-file (89) warn",
            "packages/ui/src/use.ts:5 check-variable-declaration (24) warn",
            "packages/ui/src/use.ts:5 check-expression (22) warn",
            "packages/ui/src/variants.ts:0 check-source-file (99) warn",
            "packages/ui/src/variants.ts:5 check-variable-declaration (97) error confirmed",
            "packages/ui/src/variants.ts:5 check-expression (68) warn",
        ]
    );
    let (message, related) = finding(&result, "hot-spots", "(line 5, char 94)");
    assert_eq!(
        message.split("; ").next().unwrap(),
        "Check variable declaration from (line 5, char 14) to (line 5, char 94) took 97ms (program of packages/ui/tsconfig.json)"
    );
    assert!(
        message.contains("confirms the complex-types finding at packages/ui/src/variants.ts:5 (union-intersection, about 1200 types)"),
        "{message}"
    );
    assert_eq!(
        related[0],
        "packages/ui/src/variants.ts:0 inside Check file packages/ui/src/variants.ts"
    );
    assert!(
        related[1].starts_with(
            "packages/ui/src/variants.ts:5 complex-types: this type intersects unions"
        )
    );
    let (message, _) = finding(&result, "hot-spots", "Emit declarations");
    assert!(message.contains("it repeats import(\"@acme/ui\") 12 times"), "{message}");

    assert_eq!(
        summary(&result, "duplicate-packages"),
        [
            "node_modules/.pnpm/csstype@3.1.2/node_modules/csstype/package.json:0 duplicate-package (2) warn"
        ]
    );
    let (message, related) = finding(&result, "duplicate-packages", "csstype");
    assert_eq!(
        message,
        "tsc loads `csstype` from 2 directories: 3.1.2 at node_modules/.pnpm/csstype@3.1.2/node_modules/csstype, 3.1.3 at node_modules/.pnpm/csstype@3.1.3/node_modules/csstype; align the versions that depend on it so one copy is loaded"
    );
    assert_eq!(
        related,
        [
            "node_modules/.pnpm/csstype@3.1.3/node_modules/csstype/package.json:0 another copy, version 3.1.3"
        ]
    );
    // The legend's two programs: each file the trace checked counts once.
    assert_eq!(result.files_analyzed(), 12);
}

/// npm: `Paths<T>` over a `.json` import, which complex-types flags and
/// the trace confirms; comparisons inside `Paths` with their types from
/// types.json; and date-lib nested under another package at an older
/// version.
#[test]
fn npm() {
    let result = run(&fixture("npm"), None);
    assert_eq!(
        summary(&result, "hot-spots"),
        [
            "packages/web/src/app.ts:0 check-source-file (16) warn",
            "packages/web/src/i18n.ts:0 check-source-file (100) warn",
            "packages/web/src/i18n.ts:4 check-variable-declaration (73) error confirmed",
            "packages/web/src/i18n.ts:4 check-expression (71) warn",
            "packages/web/src/paths.ts:0 check-source-file (44) warn",
            "packages/web/src/paths.ts:0 structured-type-related-to (19) warn",
            "packages/web/src/paths.ts:0 structured-type-related-to (17) warn",
        ]
    );
    let (message, related) = finding(&result, "hot-spots", "Compare types 90 and 32");
    assert_eq!(
        message,
        "Compare types 90 and 32 took 19ms: type 90 (`K`, TypeParameter, at packages/web/src/paths.ts:2:8) and type 32 (Union of 7)"
    );
    assert_eq!(
        related,
        [
            "packages/web/src/paths.ts:0 inside Check file packages/web/src/paths.ts",
            "packages/web/src/paths.ts:2 `K` is declared here",
        ]
    );
    let (message, _) = finding(&result, "hot-spots", "Compare types 90 and 10");
    assert!(message.contains("type 10 (`undefined`, Intrinsic)"), "{message}");

    assert_eq!(
        summary(&result, "duplicate-packages"),
        ["node_modules/date-lib/package.json:0 duplicate-package (2) warn"]
    );
    let (message, related) = finding(&result, "duplicate-packages", "date-lib");
    assert!(
        message.contains(
            "3.0.0 at node_modules/date-lib, 2.30.0 at node_modules/legacy-picker/node_modules/date-lib"
        ),
        "{message}"
    );
    assert_eq!(
        related,
        [
            "node_modules/legacy-picker/node_modules/date-lib/package.json:0 another copy, version 2.30.0"
        ]
    );
}

/// Bun: `Range<400>` in a variable's annotation, which complex-types
/// flags and the trace confirms, and one version of a scoped package
/// installed twice.
#[test]
fn bun() {
    let result = run(&fixture("bun"), None);
    assert_eq!(
        summary(&result, "hot-spots"),
        [
            "packages/server/src/config.ts:0 check-source-file (309) warn",
            "packages/server/src/config.ts:3 check-variable-declaration (277) error confirmed",
            "packages/server/src/main.ts:0 check-source-file (17) warn",
            "packages/server/src/main.ts:5 check-expression (15) warn",
            "packages/server/src/main.ts:5 check-expression (10) warn",
            "packages/server/src/range.ts:0 check-source-file (26) warn",
        ]
    );
    assert_eq!(
        summary(&result, "duplicate-packages"),
        ["node_modules/@acme/shared/package.json:0 duplicate-package (2) warn"]
    );
    let (message, _) = finding(&result, "duplicate-packages", "@acme/shared");
    assert!(message.contains("the same version installed twice"), "{message}");
}

/// Deno, traced on Windows: a chain of 40 zod `.extend()` calls, which
/// complex-types flags and the outermost span confirms, nested 30 deep at
/// the fixture's settings; and two zod versions in `node_modules/.deno`.
#[test]
fn deno() {
    let result = run(&fixture("deno"), None);
    let hot = summary(&result, "hot-spots");
    assert_eq!(
        hot[..3],
        [
            "packages/api/src/main.ts:0 check-source-file (95) warn",
            "packages/api/src/main.ts:5 check-variable-declaration (88) error confirmed",
            "packages/api/src/main.ts:5 check-expression (85) warn",
        ]
    );
    assert_eq!(hot.len(), 30);
    assert!(hot[2..].iter().all(|l| l.contains("check-expression") && l.ends_with("warn")));
    let (message, _) = finding(&result, "hot-spots", "(line 46, char 31)");
    assert!(
        message
            .starts_with("Check variable declaration from (line 5, char 14) to (line 46, char 31)")
    );
    assert!(message.contains("(builder-chain, about 6200 types)"), "{message}");

    assert_eq!(
        summary(&result, "duplicate-packages"),
        [
            "node_modules/.deno/zod@3.22.4/node_modules/zod/package.json:0 duplicate-package (2) warn"
        ]
    );
}

/// Copies fixture `name` to a temporary directory.
fn copy(name: &str) -> TempDir {
    let t = TempDir::new().unwrap();
    copy_dir(&fixture(name), t.path());
    t
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            fs::copy(entry.path(), dest).unwrap();
        }
    }
}

#[test]
fn a_directory_given_wins_over_the_config_file() {
    let t = copy("bun");
    fs::rename(t.path().join("trace"), t.path().join("elsewhere")).unwrap();
    let cfg = Config { dir: None, common: CommonArgs::with_root(t.path()) };
    let e = run_trace_command(&cfg).unwrap_err().to_string();
    assert!(e.contains("could not read the trace in") && e.contains("trace"), "{e}");
    let result = run(t.path(), Some(t.path().join("elsewhere")));
    assert_eq!(summary(&result, "hot-spots").len(), 6);

    // No directory at all.
    fs::write(t.path().join(".oxicleanrc.json"), "{}").unwrap();
    let e = run_trace_command(&cfg).unwrap_err().to_string();
    assert!(e.contains("no trace directory"), "{e}");
    // A directory with no trace in it.
    let empty = TempDir::new().unwrap();
    let cfg =
        Config { dir: Some(empty.path().to_path_buf()), common: CommonArgs::with_root(t.path()) };
    let e = run_trace_command(&cfg).unwrap_err().to_string();
    assert!(e.contains("holds no trace.json"), "{e}");
}

#[test]
fn rules_can_be_turned_off_and_tuned() {
    let t = copy("bun");
    let config = |rules: &str| {
        fs::write(
            t.path().join(".oxicleanrc.json"),
            format!(r#"{{ "trace": {{ "dir": "trace" }}, "rules": {rules} }}"#),
        )
        .unwrap();
    };
    config(r#"{ "hot-spots": "off" }"#);
    let result = run(t.path(), None);
    assert_eq!(result.rules.iter().map(|r| r.rule).collect::<Vec<_>>(), ["duplicate-packages"]);

    // Without complex-types nothing is confirmed, and hot spots are warnings.
    config(
        r#"{ "complex-types": "off", "duplicate-packages": "off", "hot-spots": ["warn", { "threshold": 10, "skipMillis": 2 }] }"#,
    );
    let hot = summary(&run(t.path(), None), "hot-spots");
    assert_eq!(hot.len(), 6);
    assert!(hot.iter().all(|l| l.ends_with(" warn")), "{hot:?}");

    // analyze-trace's defaults: 500ms and 100ms find nothing here.
    config(r#"{ "duplicate-packages": "off" }"#);
    assert!(summary(&run(t.path(), None), "hot-spots").is_empty());

    // Only spans of at least skipMillis are read.
    config(r#"{ "hot-spots": ["error", { "threshold": 100, "skipMillis": 100 }] }"#);
    assert_eq!(
        summary(&run(t.path(), None), "hot-spots"),
        [
            "packages/server/src/config.ts:0 check-source-file (309) error",
            "packages/server/src/config.ts:3 check-variable-declaration (277) error confirmed",
        ]
    );
}

#[test]
fn a_trace_cut_short_still_reports_what_it_holds() {
    let t = copy("bun");
    let trace = t.path().join("trace/trace.json");
    let text = fs::read_to_string(&trace).unwrap();
    // Cut inside the checking of config.ts, as when tsc runs out of memory:
    // after the declaration it checks, before the file's end event.
    let at = text.find("checkVariableDeclaration").unwrap();
    let cut = at + text[at..].find('\n').unwrap() + 1;
    fs::write(&trace, &text[..cut]).unwrap();
    fs::write(t.path().join("trace/types.json"), "[{\"id\":1,").unwrap();
    let result = run(t.path(), None);
    let hot = summary(&result, "hot-spots");
    assert!(!hot.is_empty());
    let (message, _) = finding(&result, "hot-spots", "Check file packages/server/src/config.ts");
    assert!(message.contains("still running when the trace ended"), "{message}");
}

#[test]
fn a_missing_types_file_leaves_types_out() {
    let t = copy("npm");
    fs::remove_file(t.path().join("trace/types.json")).unwrap();
    let result = run(t.path(), None);
    let (message, related) = finding(&result, "hot-spots", "Compare types 90 and 32");
    assert_eq!(message, "Compare types 90 and 32 took 19ms");
    assert_eq!(
        related,
        ["packages/web/src/paths.ts:0 inside Check file packages/web/src/paths.ts"]
    );
}

#[test]
fn a_file_that_is_not_a_trace_fails_the_command() {
    let t = copy("bun");
    fs::write(t.path().join("trace/trace.json"), "not json").unwrap();
    let cfg = Config { dir: None, common: CommonArgs::with_root(t.path()) };
    let e = run_trace_command(&cfg).unwrap_err().to_string();
    assert!(e.contains("not a trace"), "{e}");
}

/// Each finding of `rule` as `file code key`: what its SARIF fingerprint
/// is made of.
fn fingerprints(result: &CommandResult, rule: &str) -> Vec<String> {
    let r = result.rules.iter().find(|r| r.rule == rule).unwrap();
    let mut out: Vec<String> = r
        .result
        .findings
        .iter()
        .map(|f| {
            let FindingKind::Issue(issue) = &f.kind else { panic!("{f:?}") };
            let file = f.file.strip_prefix(&result.root).unwrap_or(&f.file);
            format!("{} {} {}", file.to_string_lossy().replace('\\', "/"), issue.code, issue.key)
        })
        .collect();
    out.sort();
    out
}

/// Rewrites each event of the traces in `dir` with `f`.
fn rewrite_events(dir: &Path, f: impl Fn(&mut serde_json::Map<String, serde_json::Value>)) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !name.starts_with("trace") {
            continue;
        }
        let mut events: Vec<serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        for e in &mut events {
            f(e.as_object_mut().unwrap());
        }
        fs::write(&path, serde_json::to_string(&events).unwrap()).unwrap();
    }
}

/// Another trace of the same code, by another process at another time and
/// with its files named after that process, reports the same findings
/// under the same fingerprints.
#[test]
fn fingerprints_stay_put_from_one_trace_to_the_next() {
    for name in ["pnpm", "npm", "deno"] {
        let before = run(&fixture(name), None);
        let t = copy(name);
        let trace = t.path().join("trace");
        rewrite_events(&trace, |e| {
            e.insert("pid".into(), 4242.into());
            let ts = e["ts"].as_f64().unwrap();
            e.insert("ts".into(), (ts + 54_321.5).into());
        });
        for entry in fs::read_dir(&trace).unwrap() {
            let path = entry.unwrap().path();
            let renamed = path.to_string_lossy().replace("12347", "777");
            fs::rename(&path, renamed).unwrap();
        }
        if trace.join("legend.json").exists() {
            let legend = fs::read_to_string(trace.join("legend.json")).unwrap();
            fs::write(trace.join("legend.json"), legend.replace("12347", "777")).unwrap();
        }
        let after = run(t.path(), None);
        for rule in ["hot-spots", "duplicate-packages"] {
            let (b, a) = (fingerprints(&before, rule), fingerprints(&after, rule));
            assert!(!b.is_empty(), "{name} {rule}");
            assert_eq!(b, a, "{name} {rule}");
            let mut unique = b.clone();
            unique.dedup();
            assert_eq!(unique, b, "{name} {rule}: two findings share a fingerprint");
        }
    }
}

/// A trace recorded through a symlink to the root, as in a symlinked
/// checkout, is found under the root.
#[cfg(unix)]
#[test]
fn a_trace_recorded_through_a_symlinked_root() {
    let t = TempDir::new().unwrap();
    let real = t.path().canonicalize().unwrap().join("repo");
    copy_dir(&fixture("bun"), &real);
    let link = t.path().canonicalize().unwrap().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let path = real.join("trace/trace.json");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace("/Users/dev/bun-repo", &link.to_string_lossy())).unwrap();
    let result = run(&real, None);
    assert_eq!(summary(&result, "hot-spots"), summary(&run(&fixture("bun"), None), "hot-spots"));
}

/// The threshold of the file a hot spot is reported on applies to it; a
/// frame under it is still reported when it takes most of a frame that
/// is.
#[test]
fn each_hot_spot_has_its_own_files_threshold() {
    let t = copy("bun");
    fs::write(
        t.path().join(".oxicleanrc.json"),
        r#"{ "trace": { "dir": "trace" },
             "rules": { "hot-spots": ["warn", { "threshold": 10, "skipMillis": 2 }] },
             "overrides": [{ "files": ["packages/server/src/main.ts"], "rules": { "hot-spots": ["warn", { "threshold": 16 }] } },
                           { "files": ["packages/server/src/config.ts"], "rules": { "hot-spots": ["error", { "threshold": 1000 }] } }] }"#,
    )
    .unwrap();
    assert_eq!(
        summary(&run(t.path(), None), "hot-spots"),
        [
            // 309ms is under config.ts's 1000ms, and nothing in it is kept.
            "packages/server/src/main.ts:0 check-source-file (17) warn",
            // 15ms is under main.ts's 16ms, but most of the 17ms file check,
            // and 10ms most of those 15ms.
            "packages/server/src/main.ts:5 check-expression (15) warn",
            "packages/server/src/main.ts:5 check-expression (10) warn",
            "packages/server/src/range.ts:0 check-source-file (26) warn",
        ]
    );
}

/// A confirmed hot spot takes the severity of the complex-types finding it
/// confirms when that's higher than its own, and no higher.
#[test]
fn a_confirmed_hot_spot_is_as_severe_as_what_it_confirms() {
    let t = copy("bun");
    let config = |complex: &str| {
        fs::write(
            t.path().join(".oxicleanrc.json"),
            format!(
                r#"{{ "trace": {{ "dir": "trace" }}, "rules": {{ "complex-types": "{complex}", "hot-spots": ["warn", {{ "threshold": 100, "skipMillis": 2 }}] }} }}"#
            ),
        )
        .unwrap();
    };
    config("warn");
    assert_eq!(
        summary(&run(t.path(), None), "hot-spots"),
        [
            "packages/server/src/config.ts:0 check-source-file (309) warn",
            "packages/server/src/config.ts:3 check-variable-declaration (277) warn confirmed",
        ]
    );
    config("error");
    assert_eq!(
        summary(&run(t.path(), None), "hot-spots")[1],
        "packages/server/src/config.ts:3 check-variable-declaration (277) error confirmed"
    );
}

/// A trace of files that aren't under the root reports on the root's
/// package.json, never a path outside the root.
#[test]
fn findings_stay_under_the_root() {
    let t = copy("bun");
    let elsewhere = TempDir::new().unwrap();
    copy_dir(&fixture("bun").join("trace"), elsewhere.path());
    // The trace names files of another checkout altogether.
    let path = elsewhere.path().join("trace.json");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace("/Users/dev/bun-repo/packages/server", "/Users/dev/other"))
        .unwrap();
    let result = run(t.path(), Some(elsewhere.path().to_path_buf()));
    for rule in ["hot-spots", "duplicate-packages"] {
        let r = result.rules.iter().find(|r| r.rule == rule).unwrap();
        for f in &r.result.findings {
            assert!(f.file.starts_with(&result.root), "{rule}: {}", f.file.display());
        }
    }
    let hot = summary(&result, "hot-spots");
    assert!(!hot.is_empty() && hot.iter().all(|l| l.starts_with("package.json:0")), "{hot:?}");
    // Not confirmed: the trace isn't of this code.
    assert!(hot.iter().all(|l| !l.contains("confirmed")), "{hot:?}");
}

/// Directories that are one directory through symlinks are one copy, and
/// a package several programs load twice is one finding.
#[cfg(unix)]
#[test]
fn symlinked_copies_are_one_and_programs_merge() {
    let t = copy("pnpm");
    let root = t.path().canonicalize().unwrap();
    // As `preserveSymlinks` would load it: csstype 3.1.3 through the link
    // pnpm makes in packages/ui, besides from the store.
    let store = root.join("node_modules/.pnpm/csstype@3.1.3/node_modules/csstype");
    fs::create_dir_all(root.join("packages/ui/node_modules")).unwrap();
    std::os::unix::fs::symlink(&store, root.join("packages/ui/node_modules/csstype")).unwrap();
    let trace = root.join("trace");
    rewrite_events(&trace, |e| {
        if e["name"] == "createSourceFile"
            && let Some(path) = e["args"]["path"].as_str()
            && path.contains("csstype@3.1.3")
        {
            let linked = path.replace(
                "node_modules/.pnpm/csstype@3.1.3/node_modules/csstype",
                "packages/ui/node_modules/csstype",
            );
            e["args"]["path"] = linked.into();
        }
    });
    // The second program loads both copies too.
    let first = fs::read_to_string(trace.join("trace.12347-1.json")).unwrap();
    let mut events: Vec<serde_json::Value> = serde_json::from_str(&first).unwrap();
    events.retain(|e| e["name"] == "createSourceFile");
    let second = trace.join("trace.12347-2.json");
    let mut theirs: Vec<serde_json::Value> =
        serde_json::from_str(&fs::read_to_string(&second).unwrap()).unwrap();
    theirs.extend(events);
    fs::write(&second, serde_json::to_string(&theirs).unwrap()).unwrap();

    let result = run(&root, None);
    assert_eq!(
        summary(&result, "duplicate-packages"),
        [
            "node_modules/.pnpm/csstype@3.1.2/node_modules/csstype/package.json:0 duplicate-package (2) warn"
        ]
    );
    let (message, _) = finding(&result, "duplicate-packages", "csstype");
    assert!(
        message.contains("3.1.3 at node_modules/.pnpm/csstype@3.1.3/node_modules/csstype"),
        "{message}"
    );
    assert!(message.ends_with("(in 2 programs)"), "{message}");
}
