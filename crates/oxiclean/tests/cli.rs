//! End-to-end tests: run the built binary against small fixture projects.
//! Each one reproduces a bug from the rust-skills audit.

use std::{fs, path::Path, process::Command};

use tempfile::TempDir;

fn write(root: &Path, path: &str, content: &str) {
    let p = root.join(path);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

/// Runs `oxiclean <args> --root <root>` and returns (exit code, stdout).
fn run(root: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_oxiclean"))
        .args(args)
        .arg("--root")
        .arg(root)
        .current_dir(if root.exists() { root } else { root.parent().unwrap() })
        .env("NO_COLOR", "1")
        .env_remove("RUST_LOG")
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    (out.status.code().unwrap_or(-1), format!("{stdout}{stderr}"))
}

fn cycle_fixture() -> TempDir {
    let t = TempDir::new().unwrap();
    write(t.path(), "src/e.ts", "import './a'; import './b';");
    write(t.path(), "src/e2.ts", "import './b';");
    write(t.path(), "src/a.ts", "import './b'; import './x';");
    write(t.path(), "src/b.ts", "import './a';");
    write(t.path(), "src/x.ts", "import './y';");
    write(t.path(), "src/y.ts", "import './z';");
    write(t.path(), "src/z.ts", "export const z = 1;");
    t
}

#[test]
fn depth_is_the_same_for_a_cycle_from_every_entry() {
    let t = cycle_fixture();
    let (code, out) =
        run(t.path(), &["import-depth", "--threshold", "1", "--entry-glob", "src/e*.ts"]);
    assert_eq!(code, 1, "{out}");
    // Before the fix, './b' was reported as depth 2 from one entry and 5 from another.
    let b_lines: Vec<&str> = out.lines().filter(|l| l.contains("import './b'")).collect();
    assert!(b_lines.len() >= 2, "{out}");
    assert!(b_lines.iter().all(|l| l.contains("(depth: 4)")), "{out}");
    assert!(!out.contains("(depth: 2)") && !out.contains("(depth: 5)"), "{out}");
}

#[test]
fn output_is_identical_across_runs() {
    let t = cycle_fixture();
    let strip_timing =
        |s: String| s.lines().filter(|l| !l.contains("Finished in")).collect::<Vec<_>>().join("\n");
    let first = strip_timing(run(t.path(), &["import-depth", "--threshold", "1"]).1);
    for _ in 0..5 {
        assert_eq!(strip_timing(run(t.path(), &["import-depth", "--threshold", "1"]).1), first);
    }
}

#[test]
fn bloat_follows_barrel_re_exports() {
    let t = TempDir::new().unwrap();
    for i in 1..=5 {
        write(t.path(), &format!("src/m{i}.ts"), &format!("export const v{i} = {i};"));
    }
    write(
        t.path(),
        "src/barrel.ts",
        "export * from './m1';\nexport * from './m2';\nexport { v3 } from './m3';\nexport * from './m4';\nexport * from './m5';",
    );
    write(t.path(), "src/main.ts", "import { v1 } from './barrel';");

    let (code, out) =
        run(t.path(), &["import-bloat", "--threshold", "1", "--entry-glob", "src/main.ts"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("import './barrel' (6 modules)"), "{out}");
}

#[test]
fn entry_glob_is_a_real_glob() {
    let t = TempDir::new().unwrap();
    write(t.path(), "src/pages/home.tsx", "import './nested/card';");
    write(t.path(), "src/pages/nested/card.tsx", "export const c = 1;");

    let (code, out) =
        run(t.path(), &["import-bloat", "--threshold", "1", "--entry-glob", "src/pages/*.tsx"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("home.tsx (2 modules)"), "{out}");
    assert!(!out.contains("card.tsx ("), "nested file must not match `*`: {out}");
}

#[test]
fn gitignored_dirs_are_skipped_without_git() {
    let t = TempDir::new().unwrap();
    write(t.path(), ".gitignore", "dist/\n");
    write(t.path(), "src/a.ts", "export const a = 1;");
    for i in 0..10 {
        write(t.path(), &format!("dist/src/f{i}.js"), "export const b = 1;");
        write(t.path(), &format!("node_modules/pkg/src/f{i}.js"), "export const b = 1;");
    }

    let (code, out) = run(t.path(), &["import-bloat"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("on 1 files"), "{out}");
}

#[test]
fn unparseable_files_are_reported() {
    let t = TempDir::new().unwrap();
    write(t.path(), "src/main.ts", "import './broken';");
    write(t.path(), "src/broken.ts", "export const = ;");

    let (code, out) = run(t.path(), &["import-bloat", "--entry-glob", "src/main.ts"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("1 file(s) could not be parsed"), "{out}");
    assert!(out.contains("broken.ts"), "{out}");
}

#[test]
fn dynamic_imports_are_opt_in() {
    let t = TempDir::new().unwrap();
    write(t.path(), "src/main.ts", "export const load = () => import('./heavy');");
    write(t.path(), "src/heavy.ts", "import './dep';");
    write(t.path(), "src/dep.ts", "export const d = 1;");

    let args = ["import-bloat", "--threshold", "2", "--entry-glob", "src/main.ts"];
    let (code, out) = run(t.path(), &args);
    assert_eq!(code, 0, "{out}");

    let (code, out) = run(t.path(), &[&args[..], &["--include-dynamic"]].concat());
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("import './heavy' (2 modules)"), "{out}");
}

#[test]
fn missing_root_is_a_clear_error() {
    let t = TempDir::new().unwrap();
    let (code, out) = run(&t.path().join("nope"), &["import-depth"]);
    assert_ne!(code, 0);
    assert!(out.contains("is not accessible"), "{out}");
}

#[test]
fn no_entries_is_a_clear_error() {
    let t = TempDir::new().unwrap();
    write(t.path(), "lib/a.ts", "");
    let (code, out) = run(t.path(), &["import-depth"]);
    assert_ne!(code, 0);
    assert!(out.contains("no entry files found"), "{out}");
}

/// Like [`run`], but returns stdout alone, for machine-readable output.
fn run_stdout(root: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_oxiclean"))
        .args(args)
        .arg("--root")
        .arg(root)
        .current_dir(root)
        .env("NO_COLOR", "1")
        .env_remove("RUST_LOG")
        .output()
        .unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8(out.stdout).unwrap())
}

/// A pnpm workspace with two packages whose entries each pull in 3 modules.
fn workspace_fixture() -> TempDir {
    let t = TempDir::new().unwrap();
    write(t.path(), "pnpm-workspace.yaml", "packages:\n  - 'packages/*'\n");
    for pkg in ["web", "legacy"] {
        write(
            t.path(),
            &format!("packages/{pkg}/package.json"),
            &format!(r#"{{"name":"@acme/{pkg}"}}"#),
        );
        write(t.path(), &format!("packages/{pkg}/src/main.ts"), "import './a';");
        write(t.path(), &format!("packages/{pkg}/src/a.ts"), "import './b';");
        write(t.path(), &format!("packages/{pkg}/src/b.ts"), "export const b = 1;");
    }
    t
}

#[test]
fn config_file_sets_thresholds_per_package() {
    let t = workspace_fixture();
    write(
        t.path(),
        "oxiclean.toml",
        r#"
        entry-globs = ["**/main.ts"]

        [rules.import-bloat]
        threshold = 2

        [[overrides]]
        packages = ["@acme/legacy"]
        [overrides.rules.import-bloat]
        threshold = 50
        "#,
    );
    let (code, out) = run(t.path(), &["import-bloat"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("threshold: 2 modules"), "{out}");
    assert!(out.contains("web"), "{out}");
    assert!(!out.contains("legacy"), "the override raised legacy's threshold: {out}");

    // --threshold replaces every threshold in the file.
    let (_, out) = run(t.path(), &["import-bloat", "--threshold", "2"]);
    assert!(out.contains("legacy"), "{out}");
}

#[test]
fn warnings_do_not_fail_the_run() {
    let t = workspace_fixture();
    write(t.path(), "oxiclean.toml", "[rules.import-depth]\nthreshold = 1\nseverity = \"warn\"\n");
    let (code, out) = run(t.path(), &["import-depth"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("[warn]"), "{out}");
    assert!(out.contains("Warnings (don't fail the run): 4"), "{out}");
}

#[test]
fn disabled_rules_and_ignored_files_report_nothing() {
    let t = workspace_fixture();
    write(
        t.path(),
        "oxiclean.toml",
        "ignore = [\"packages/legacy/**\"]\n[rules.import-bloat]\nenabled = false\n",
    );
    let (code, out) = run(t.path(), &["import-bloat", "--threshold", "1"]);
    assert_eq!(code, 0, "{out}");

    // The global ignore also keeps legacy's files from being entries.
    let (_, out) = run(t.path(), &["import-depth", "--threshold", "1"]);
    assert!(out.contains("web") && !out.contains("legacy"), "{out}");
}

#[test]
fn invalid_config_is_a_clear_error() {
    let t = workspace_fixture();
    write(t.path(), "oxiclean.toml", "[rules.import-blot]\nthreshold = 1\n");
    let (code, out) = run(t.path(), &["import-bloat"]);
    assert_ne!(code, 0);
    assert!(out.contains("oxiclean.toml") && out.contains("unknown rule 'import-blot'"), "{out}");
}

#[test]
fn package_flag_limits_entries_to_a_workspace_package() {
    let t = workspace_fixture();
    let (code, out) =
        run(t.path(), &["import-depth", "--threshold", "1", "--package", "@acme/web"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("web") && !out.contains("legacy"), "{out}");

    let (code, out) = run(t.path(), &["import-depth", "--package", "@acme/nope"]);
    assert_ne!(code, 0);
    assert!(out.contains("no workspace package matches @acme/nope"), "{out}");
}

#[test]
fn json_output() {
    let t = workspace_fixture();
    let (code, out) = run_stdout(
        t.path(),
        &["import-bloat", "--threshold", "2", "--format", "json", "--package", "@acme/web"],
    );
    assert_eq!(code, 1, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["rule"], "import-bloat");
    assert_eq!(v["summary"]["errors"], 3);
    let first = &v["findings"][0];
    assert_eq!(first["package"], "@acme/web");
    assert_eq!(first["file"], "packages/web/src/a.ts");
}

#[test]
fn sarif_output() {
    let t = workspace_fixture();
    let (code, out) =
        run_stdout(t.path(), &["--format", "sarif", "import-depth", "--threshold", "2"]);
    assert_eq!(code, 1, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["version"], "2.1.0");
    let results = v["runs"][0]["results"].as_array().unwrap();
    assert!(!results.is_empty());
    assert!(results.iter().all(|r| r["ruleId"] == "import-depth" && r["level"] == "error"));
    let loc = &results[0]["locations"][0]["physicalLocation"];
    assert_eq!(loc["region"]["startLine"], 1);
}
