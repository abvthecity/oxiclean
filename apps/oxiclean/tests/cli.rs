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
