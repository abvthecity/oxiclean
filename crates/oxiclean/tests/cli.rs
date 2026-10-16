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

/// Writes `.oxicleanrc.json` in `root`.
fn configure(root: &Path, json: &str) {
    write(root, ".oxicleanrc.json", json);
}

/// A config with these entries and one rule at `threshold`.
fn rule_config(rule: &str, threshold: usize, entries: &[&str]) -> String {
    let entries: Vec<String> = entries.iter().map(|e| format!("{e:?}")).collect();
    format!(
        r#"{{ "entries": [{}], "rules": {{ "{rule}": ["error", {{ "threshold": {threshold} }}] }} }}"#,
        entries.join(", ")
    )
}

/// A config with `rules` turned off.
fn off_config(rules: &[&str]) -> String {
    let off: Vec<String> = rules.iter().map(|r| format!(r#""{r}": "off""#)).collect();
    format!(r#"{{ "rules": {{ {} }} }}"#, off.join(", "))
}

/// A config that leaves only `rule` of the `typecheck` command on.
fn typecheck_only(rule: &str) -> String {
    let others: Vec<&str> =
        oxiclean_typecheck::rules().into_iter().filter(|r| *r != rule).collect();
    off_config(&others)
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
    configure(t.path(), &rule_config("depth", 1, &["src/e*.ts"]));
    let (code, out) = run(t.path(), &["depth"]);
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
    configure(t.path(), &rule_config("depth", 1, &[]));
    let strip_timing =
        |s: String| s.lines().filter(|l| !l.contains("Finished in")).collect::<Vec<_>>().join("\n");
    let first = strip_timing(run(t.path(), &["depth"]).1);
    for _ in 0..5 {
        assert_eq!(strip_timing(run(t.path(), &["depth"]).1), first);
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
    configure(t.path(), &rule_config("bloat", 1, &["src/main.ts"]));

    let (code, out) = run(t.path(), &["bloat"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("import './barrel' (6 modules)"), "{out}");
}

#[test]
fn entries_are_real_globs() {
    let t = TempDir::new().unwrap();
    write(t.path(), "src/pages/home.tsx", "import './nested/card';");
    write(t.path(), "src/pages/nested/card.tsx", "export const c = 1;");
    configure(t.path(), &rule_config("bloat", 1, &["src/pages/*.tsx"]));

    let (code, out) = run(t.path(), &["bloat"]);
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

    let (code, out) = run(t.path(), &["bloat"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("on 1 files"), "{out}");
}

#[test]
fn unparseable_files_are_reported() {
    let t = TempDir::new().unwrap();
    write(t.path(), "src/main.ts", "import './broken';");
    write(t.path(), "src/broken.ts", "export const = ;");
    configure(t.path(), r#"{ "entries": ["src/main.ts"] }"#);

    let (code, out) = run(t.path(), &["bloat"]);
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

    configure(t.path(), &rule_config("bloat", 2, &["src/main.ts"]));
    let (code, out) = run(t.path(), &["bloat"]);
    assert_eq!(code, 0, "{out}");

    configure(
        t.path(),
        r#"{ "entries": ["src/main.ts"], "includeDynamic": true,
             "rules": { "bloat": ["error", { "threshold": 2 }] } }"#,
    );
    let (code, out) = run(t.path(), &["bloat"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("import './heavy' (2 modules)"), "{out}");
}

#[test]
fn missing_root_is_a_clear_error() {
    let t = TempDir::new().unwrap();
    let (code, out) = run(&t.path().join("nope"), &["depth"]);
    assert_ne!(code, 0);
    assert!(out.contains("is not accessible"), "{out}");
}

#[test]
fn no_entries_is_a_clear_error() {
    let t = TempDir::new().unwrap();
    write(t.path(), "lib/a.ts", "");
    let (code, out) = run(t.path(), &["depth"]);
    assert_ne!(code, 0);
    assert!(out.contains("no entry files found"), "{out}");
}

#[test]
fn single_rule_commands_honour_off() {
    let t = cycle_fixture();
    for command in ["depth", "cycles", "duplicates"] {
        configure(t.path(), &off_config(&[command]));
        let (code, out) = run(t.path(), &[command]);
        assert_eq!(code, 0, "{command}: {out}");
        assert!(
            out.contains(&format!("The config file turns off every rule {command} runs.")),
            "{command}: {out}"
        );
        assert!(!out.contains("depth:") && !out.contains("No "), "{command}: nothing ran: {out}");

        let (code, out) = run_stdout(t.path(), &[command, "--format", "json"]);
        assert_eq!(code, 0, "{command}: {out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["rules"], serde_json::json!([]), "{command}: {out}");
    }
}

#[test]
fn bloat_runs_barrels_alone() {
    let t = TempDir::new().unwrap();
    for i in 1..=3 {
        write(t.path(), &format!("src/m{i}.ts"), &format!("export const v{i} = {i};"));
    }
    write(
        t.path(),
        "src/barrel.ts",
        "export * from './m1';\nexport * from './m2';\nexport * from './m3';",
    );
    write(t.path(), "src/main.ts", "import { v1 } from './barrel';");
    configure(
        t.path(),
        r#"{ "entries": ["src/main.ts"],
             "rules": { "bloat": "off", "barrels": ["error", { "threshold": 2 }] } }"#,
    );
    let (code, out) = run_stdout(t.path(), &["bloat", "--format", "json"]);
    assert_eq!(code, 1, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let rules: Vec<&str> =
        v["rules"].as_array().unwrap().iter().map(|r| r["rule"].as_str().unwrap()).collect();
    assert_eq!(rules, ["barrels"], "{out}");
    let findings = v["findings"].as_array().unwrap();
    assert!(!findings.is_empty() && findings.iter().all(|f| f["rule"] == "barrels"), "{out}");
    assert!(findings.iter().any(|f| f["file"] == "src/barrel.ts"), "{out}");

    let (_, out) = run(t.path(), &["bloat"]);
    assert!(!out.contains("No bloat detected") && !out.contains("Ran "), "{out}");
}

#[test]
fn rule_options_are_not_command_line_flags() {
    let t = TempDir::new().unwrap();
    write(t.path(), "src/a.ts", "");
    for flag in ["--threshold", "--entry-glob", "--include-dynamic", "--package"] {
        let (code, out) = run(t.path(), &["bloat", flag, "1"]);
        assert_eq!(code, 2, "{flag} should be rejected: {out}");
    }
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
    configure(
        t.path(),
        r#"{
            // Comments are allowed, as in .oxlintrc.json
            "entries": ["**/main.ts"],
            "rules": { "bloat": ["error", { "threshold": 2 }] },
            "overrides": [
                { "packages": ["@acme/legacy"], "rules": { "bloat": ["error", { "threshold": 50 }] } },
            ],
        }"#,
    );
    let (code, out) = run(t.path(), &["bloat"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("threshold: 2 modules"), "{out}");
    assert!(out.contains("web"), "{out}");
    assert!(!out.contains("legacy"), "the override raised legacy's threshold: {out}");
}

#[test]
fn config_flag_reads_another_file() {
    let t = workspace_fixture();
    write(t.path(), "configs/strict.json", &rule_config("bloat", 2, &["**/main.ts"]));
    let (code, out) = run(t.path(), &["bloat", "--config", "configs/strict.json"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("threshold: 2 modules"), "{out}");
}

#[test]
fn warnings_do_not_fail_the_run() {
    let t = workspace_fixture();
    configure(t.path(), r#"{ "rules": { "depth": ["warn", { "threshold": 1 }] } }"#);
    let (code, out) = run(t.path(), &["depth"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("[warn]"), "{out}");
    assert!(out.contains("Warnings (don't fail the run): 4"), "{out}");
}

#[test]
fn disabled_rules_and_ignored_files_report_nothing() {
    let t = workspace_fixture();
    configure(
        t.path(),
        r#"{
            "ignorePatterns": ["packages/legacy/**"],
            "rules": { "bloat": "off", "depth": ["error", { "threshold": 1 }] }
        }"#,
    );
    let (code, out) = run(t.path(), &["bloat"]);
    assert_eq!(code, 0, "{out}");

    // The ignore patterns also keep legacy's files from being entries.
    let (_, out) = run(t.path(), &["depth"]);
    assert!(out.contains("web") && !out.contains("legacy"), "{out}");
}

#[test]
fn invalid_config_is_a_clear_error() {
    let t = workspace_fixture();
    configure(t.path(), r#"{ "rules": { "import-blot": "warn" } }"#);
    let (code, out) = run(t.path(), &["bloat"]);
    assert_ne!(code, 0);
    assert!(out.contains(".oxicleanrc.json") && out.contains("import-blot"), "{out}");
}

#[test]
fn json_output() {
    let t = workspace_fixture();
    configure(t.path(), &rule_config("bloat", 2, &["packages/web/**"]));
    let (code, out) = run_stdout(t.path(), &["bloat", "--format", "json"]);
    assert_eq!(code, 1, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["command"], "bloat");
    assert_eq!(v["rules"][0]["rule"], "bloat");
    assert_eq!(v["rules"][0]["threshold"], 2);
    assert_eq!(v["rules"][0]["errors"], 3);
    assert_eq!(v["rules"][1]["rule"], "barrels");
    assert_eq!(v["rules"][1]["findings"], 0);
    assert_eq!(v["summary"]["errors"], 3);
    assert!(v["findings"].as_array().unwrap().iter().all(|f| f["rule"] == "bloat"), "{out}");
    let first = &v["findings"][0];
    assert_eq!(first["package"], "@acme/web");
    assert_eq!(first["file"], "packages/web/src/a.ts");
}

#[test]
fn sarif_output() {
    let t = workspace_fixture();
    configure(t.path(), &rule_config("depth", 2, &[]));
    let (code, out) = run_stdout(t.path(), &["--format", "sarif", "depth"]);
    assert_eq!(code, 1, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["version"], "2.1.0");
    let results = v["runs"][0]["results"].as_array().unwrap();
    assert!(!results.is_empty());
    assert!(results.iter().all(|r| r["ruleId"] == "depth" && r["level"] == "error"));
    let loc = &results[0]["locations"][0]["physicalLocation"];
    assert_eq!(loc["region"]["startLine"], 1);
}

#[test]
fn node_modules_are_scanned_only_when_enabled() {
    let t = TempDir::new().unwrap();
    write(t.path(), "src/main.ts", "import 'dep';");
    write(t.path(), "node_modules/dep/package.json", r#"{"name":"dep","main":"index.js"}"#);
    write(t.path(), "node_modules/dep/index.js", "require('./a'); require('./b');");
    write(t.path(), "node_modules/dep/a.js", "");
    write(t.path(), "node_modules/dep/b.js", "");

    // Off by default: the package is one leaf module.
    configure(t.path(), &rule_config("bloat", 1, &["src/main.ts"]));
    let (_, out) = run(t.path(), &["bloat"]);
    assert!(out.contains("import 'dep' (1 modules)"), "{out}");

    configure(
        t.path(),
        r#"{ "entries": ["src/main.ts"], "includeNodeModules": true,
             "rules": { "bloat": ["error", { "threshold": 1 }] } }"#,
    );
    let (_, out) = run(t.path(), &["bloat"]);
    assert!(out.contains("import 'dep' (3 modules)"), "{out}");
}

#[test]
fn installed_workspace_packages_resolve_to_their_sources() {
    let t = workspace_fixture();
    write(t.path(), "packages/web/src/main.ts", "import '@acme/legacy';");
    write(
        t.path(),
        "packages/legacy/package.json",
        r#"{"name":"@acme/legacy","main":"src/main.ts"}"#,
    );
    // What an install leaves behind: a copy (or symlink) under node_modules.
    write(
        t.path(),
        "node_modules/@acme/legacy/package.json",
        r#"{"name":"@acme/legacy","main":"src/main.ts"}"#,
    );
    write(t.path(), "node_modules/@acme/legacy/src/main.ts", "");
    configure(t.path(), &rule_config("bloat", 1, &["packages/web/src/main.ts"]));
    let (_, out) = run(t.path(), &["bloat"]);
    assert!(out.contains("import '@acme/legacy' (3 modules)"), "{out}");
}

/// Two files that import each other in a workspace package, plus one that
/// only imports a type back.
fn cycles_fixture() -> TempDir {
    let t = workspace_fixture();
    write(
        t.path(),
        "packages/web/src/b.ts",
        "import { main } from './main';\nexport const b = main;",
    );
    write(t.path(), "packages/web/src/main.ts", "import './a';\nexport const main = 1;");
    write(
        t.path(),
        "packages/legacy/src/b.ts",
        "import type { A } from './a';\nexport const b = 1;",
    );
    t
}

#[test]
fn cycles_text_output() {
    let t = cycles_fixture();
    let (code, out) = run(t.path(), &["cycles"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("Circular imports detected"), "{out}");
    assert!(out.contains("import './b' (cycle of 3 files)"), "{out}");
    for step in
        ["src/a.ts:1 imports './b'", "src/b.ts:1 imports './main'", "src/main.ts:1 imports './a'"]
    {
        assert!(out.contains(step), "{step} in {out}");
    }
    assert!(out.contains("Total violations: 1"), "{out}");
    assert!(!out.contains("legacy"), "type-only imports don't make cycles: {out}");
}

#[test]
fn cycles_json_and_sarif_list_every_import() {
    let t = cycles_fixture();
    let (code, out) = run_stdout(t.path(), &["cycles", "--format", "json"]);
    assert_eq!(code, 1, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let finding = &v["findings"][0];
    assert_eq!(finding["kind"], "cycle");
    assert_eq!(finding["package"], "@acme/web");
    assert_eq!(finding["value"], 3);
    assert_eq!(finding["group_size"], 3);
    let files: Vec<&str> =
        finding["cycle"].as_array().unwrap().iter().map(|s| s["file"].as_str().unwrap()).collect();
    assert_eq!(
        files,
        ["packages/web/src/a.ts", "packages/web/src/b.ts", "packages/web/src/main.ts"]
    );

    let (_, out) = run_stdout(t.path(), &["cycles", "--format", "sarif"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let result = &v["runs"][0]["results"][0];
    assert_eq!(result["ruleId"], "cycles");
    assert_eq!(result["relatedLocations"].as_array().unwrap().len(), 2);
}

#[test]
fn duplicates_in_every_format() {
    let t = workspace_fixture();
    write(
        t.path(),
        "packages/web/src/b.ts",
        "export const b = 1;\nexport const bee = b;\n/** @alias */\nexport default b;",
    );
    let (code, out) = run(t.path(), &["duplicates"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("Duplicate exports detected"), "{out}");
    assert!(out.contains("export 'b' (exported as 2 names)"), "{out}");
    assert!(out.contains("src/b.ts:2 also exports it as 'bee'"), "{out}");

    let (_, out) = run_stdout(t.path(), &["duplicates", "--format", "json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let finding = &v["findings"][0];
    assert_eq!(v["findings"].as_array().unwrap().len(), 1);
    assert_eq!(finding["kind"], "duplicate-export");
    assert_eq!(finding["file"], "packages/web/src/b.ts");
    assert_eq!(finding["line"], 1);
    assert_eq!(
        finding["message"],
        "'b', 'bee' export the same thing (exported as 2 names, threshold: 2 names)"
    );
    assert_eq!(finding["names"][1], serde_json::json!({ "name": "bee", "line": 2 }));

    let (_, out) = run_stdout(t.path(), &["duplicates", "--format", "sarif"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let result = &v["runs"][0]["results"][0];
    assert_eq!(result["ruleId"], "duplicates");
    assert_eq!(result["relatedLocations"][0]["physicalLocation"]["region"]["startLine"], 2);
}

/// Without `entries`, the checks of exports start from what each package
/// offers: here the `exports` of `@acme/ui`, mapped from `dist` to `src`.
#[test]
fn unused_exports_and_types_in_every_format() {
    let t = TempDir::new().unwrap();
    write(t.path(), "pnpm-workspace.yaml", "packages:\n  - 'packages/*'\n");
    write(
        t.path(),
        "packages/ui/package.json",
        r#"{"name":"@acme/ui","exports":{".":"./dist/index.js"}}"#,
    );
    write(t.path(), "packages/ui/src/index.ts", "export { Button } from './button';");
    write(
        t.path(),
        "packages/ui/src/button.ts",
        "export const Button = 1;\nexport const unusedHelper = 2;\nexport type Size = 'sm';",
    );

    let (code, out) = run(t.path(), &["unused"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("Unused exports detected"), "{out}");
    assert!(out.contains("export 'unusedHelper' (never imported)"), "{out}");
    assert!(out.contains("Unused exported types detected"), "{out}");
    assert!(out.contains("export 'Size' (never imported)"), "{out}");
    assert!(out.contains("No unused enum members detected."), "{out}");
    assert!(!out.contains("'Button'"), "{out}");
    assert!(out.contains("Ran 4 rules"), "{out}");

    let (_, out) = run_stdout(t.path(), &["unused", "--format", "json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["command"], "unused");
    assert_eq!(v["findings"].as_array().unwrap().len(), 2, "{out}");
    assert_eq!(v["findings"][0]["rule"], "unused-exports");
    let finding = &v["findings"][1];
    assert_eq!(finding["rule"], "unused-types");
    assert_eq!(finding["kind"], "unused-export");
    assert_eq!(finding["file"], "packages/ui/src/button.ts");
    assert_eq!(finding["name"], "Size");
    assert_eq!(finding["line"], 3);
    assert_eq!(finding["package"], "@acme/ui");

    let (_, out) = run_stdout(t.path(), &["unused", "--format", "sarif"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["runs"][0]["automationDetails"]["id"], "oxiclean/unused/");
    assert_eq!(v["runs"][0]["results"][1]["ruleId"], "unused-types");
    let result = &v["runs"][0]["results"][0];
    assert_eq!(result["ruleId"], "unused-exports");
    assert_eq!(result["message"]["text"], "value 'unusedHelper' is exported but never imported");
    assert_eq!(result["locations"][0]["physicalLocation"]["region"]["startLine"], 2);
}

#[test]
fn unused_members_in_every_format() {
    let t = TempDir::new().unwrap();
    write(t.path(), "package.json", r#"{"name":"app","main":"src/index.ts"}"#);
    write(
        t.path(),
        "src/index.ts",
        "import { Color, Api } from './lib';\nconsole.log(Color.Red, Api.get);",
    );
    write(
        t.path(),
        "src/lib.ts",
        "export enum Color {\n  Red,\n  Blue,\n}\nexport namespace Api {\n  export const get = 1;\n  export function put() {}\n}",
    );

    configure(t.path(), &off_config(&["unused-namespace-members"]));
    let (code, out) = run(t.path(), &["unused"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("member 'Color.Blue' (never used)"), "{out}");
    assert!(!out.contains("Red") && !out.contains("Api"), "{out}");

    configure(t.path(), &off_config(&["unused-enum-members"]));
    let (_, out) = run_stdout(t.path(), &["unused", "--format", "json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["findings"].as_array().unwrap().len(), 1, "{out}");
    let finding = &v["findings"][0];
    assert_eq!(finding["rule"], "unused-namespace-members");
    assert_eq!(finding["kind"], "unused-member");
    assert_eq!(finding["file"], "src/lib.ts");
    assert_eq!(finding["name"], "Api.put");
    assert_eq!(finding["symbol"], "function");
    assert_eq!(finding["line"], 7);

    configure(t.path(), "{}");
    let (_, out) = run_stdout(t.path(), &["unused", "--format", "sarif"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let results = v["runs"][0]["results"].as_array().unwrap();
    assert_eq!(results.len(), 2, "{out}");
    assert_eq!(results[1]["ruleId"], "unused-namespace-members");
    let result = &results[0];
    assert_eq!(result["ruleId"], "unused-enum-members");
    assert_eq!(result["message"]["text"], "enum member 'Color.Blue' is never used");
    assert_eq!(result["locations"][0]["physicalLocation"]["region"]["startLine"], 3);
}

#[test]
fn unused_dependencies_in_every_format() {
    let t = TempDir::new().unwrap();
    write(
        t.path(),
        "package.json",
        "{\n  \"name\": \"app\",\n  \"main\": \"src/index.ts\",\n  \"scripts\": { \"build\": \"tsc\" },\n  \"dependencies\": {\n    \"react\": \"*\",\n    \"lodash\": \"*\"\n  },\n  \"devDependencies\": {\n    \"typescript\": \"*\",\n    \"prettier\": \"*\"\n  }\n}\n",
    );
    write(t.path(), "src/index.ts", "import React from 'react';\nexport default React;");

    let (code, out) = run(t.path(), &["dependencies"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("package 'lodash' (never used)"), "{out}");
    assert!(!out.contains("react"), "{out}");

    let (_, out) = run_stdout(t.path(), &["dependencies", "--format", "json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["findings"].as_array().unwrap().len(), 2, "{out}");
    let finding = &v["findings"][1];
    assert_eq!(finding["kind"], "unused-dependency");
    assert_eq!(finding["file"], "package.json");
    assert_eq!(finding["name"], "prettier");
    assert_eq!(finding["field"], "devDependencies");
    assert_eq!(finding["line"], 11);

    let (_, out) = run_stdout(t.path(), &["dependencies", "--format", "sarif"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let result = &v["runs"][0]["results"][0];
    assert_eq!(result["ruleId"], "dependencies");
    assert_eq!(
        result["message"]["text"],
        "'lodash' is listed in dependencies but never imported or run"
    );
    assert_eq!(result["locations"][0]["physicalLocation"]["region"]["startLine"], 7);
}

/// Copies the fixture directory `from` into `to`.
fn copy_dir(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            fs::create_dir_all(&target).unwrap();
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn fix_excludes_build_output_and_keeps_comments() {
    let t = TempDir::new().unwrap();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../oxiclean_typecheck/tests/fixtures/pnpm");
    copy_dir(&fixture, t.path());
    configure(t.path(), &typecheck_only("program-files"));
    let (code, out) = run(t.path(), &["typecheck", "--fix"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("Fixed 2 findings"), "{out}");
    assert_eq!(
        fs::read_to_string(t.path().join("packages/app/tsconfig.json")).unwrap(),
        "{\n  \"extends\": \"../../tsconfig.base.json\",\n  // tests are checked separately\n  \"exclude\": [\"**/*.test.ts\", \"dist\"]\n}\n"
    );
    assert_eq!(
        fs::read_to_string(t.path().join("packages/app/tsconfig.build.json")).unwrap(),
        "{\n  \"extends\": \"./tsconfig.json\",\n  \"compilerOptions\": {\n    \"outDir\": \"dist\"\n  },\n  \"exclude\": [\"**/*.test.ts\", \"dist\"]\n}\n"
    );
    let (code, out) = run(t.path(), &["typecheck"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("No tsconfig program checks files it doesn't need."), "{out}");
}

#[test]
fn fix_adds_and_removes_references() {
    let t = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiclean_typecheck/tests/fixtures/references-pnpm");
    copy_dir(&fixture, t.path());
    configure(t.path(), &typecheck_only("references"));
    let (code, out) = run(t.path(), &["typecheck", "--fix"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("Fixed 3 findings"), "{out}");
    assert_eq!(
        fs::read_to_string(t.path().join("packages/app/tsconfig.json")).unwrap(),
        "{\n  \"compilerOptions\": { \"composite\": true },\n  \"include\": [\"src\"],\n  \"references\": [{ \"path\": \"../ui\" }]\n}\n"
    );
    assert!(
        fs::read_to_string(t.path().join("packages/legacy/tsconfig.json"))
            .unwrap()
            .contains("\"composite\": true")
    );
    let (_, out) = run(t.path(), &["typecheck"]);
    assert!(out.contains("Total violations: 2"), "{out}");
}

#[test]
fn fix_sets_tsconfig_options() {
    let t = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiclean_typecheck/tests/fixtures/options-pnpm");
    copy_dir(&fixture, t.path());
    configure(t.path(), &typecheck_only("tsconfig"));
    let (code, out) = run(t.path(), &["typecheck", "--fix"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("Fixed 3 findings"), "{out}");
    assert_eq!(
        fs::read_to_string(t.path().join("packages/app/tsconfig.json")).unwrap(),
        "{\n  \"extends\": \"../../tsconfig.base.json\",\n  \"compilerOptions\": {\n    \"extendedDiagnostics\": false,\n    \"incremental\": true,\n    \"tsBuildInfoFile\": \"./node_modules/.tmp/tsconfig.tsbuildinfo\"\n  },\n  \"include\": [\"src\"]\n}\n"
    );
    assert_eq!(
        fs::read_to_string(t.path().join("tsconfig.base.json")).unwrap(),
        "{\n  \"compilerOptions\": {\n    \"strict\": true,\n    \"skipLibCheck\": true\n  }\n}\n"
    );
    let (code, out) = run(t.path(), &["typecheck"]);
    assert_eq!(code, 0, "{out}");
}

/// `lint --fix` adds build output to the ignores of JSON configs and of
/// flat configs written as an array, and turns off oxlint's unused
/// type-aware mode; what needs a decision stays a warning.
#[test]
fn lint_fix_ignores_build_output() {
    let lint = Path::new(env!("CARGO_MANIFEST_DIR")).join("../oxiclean_lint/tests/fixtures");

    let t = TempDir::new().unwrap();
    copy_dir(&lint.join("lint-npm"), t.path());
    let (code, out) = run(t.path(), &["lint", "--fix"]);
    assert_eq!(code, 0, "warnings don't fail the run: {out}");
    assert!(out.contains("Fixed 2 findings"), "{out}");
    let eslintrc = fs::read_to_string(t.path().join(".eslintrc.json")).unwrap();
    assert!(
        eslintrc.contains("\"ignorePatterns\": [\"coverage\", \"packages/lib/build/**\"],"),
        "{eslintrc}"
    );
    assert!(eslintrc.contains("// Type information for every file ESLint lints."));
    assert_eq!(
        fs::read_to_string(t.path().join(".oxlintrc.json")).unwrap(),
        "{\n  \"$schema\": \"./node_modules/oxlint/configuration_schema.json\",\n  \"categories\": { \"correctness\": \"warn\" },\n  \"ignorePatterns\": [\"vendor\", \"packages/lib/build/**\"]\n}\n"
    );
    assert!(out.contains("import/no-unused-modules"), "{out}");

    let t = TempDir::new().unwrap();
    copy_dir(&lint.join("lint-pnpm"), t.path());
    let (_, out) = run(t.path(), &["lint", "--fix"]);
    assert!(out.contains("Fixed 1 findings"), "{out}");
    let flat = fs::read_to_string(t.path().join("eslint.config.mjs")).unwrap();
    assert!(
        flat.contains("export default tseslint.config(\n  { ignores: ['packages/api/dist/**', 'packages/web/build/**'] },\n  ...tseslint.configs.recommendedTypeChecked,"),
        "{flat}"
    );

    let t = TempDir::new().unwrap();
    copy_dir(&lint.join("lint-bun"), t.path());
    let (_, out) = run(t.path(), &["lint", "--fix"]);
    assert!(out.contains("Fixed 2 findings"), "{out}");
    let flat = fs::read_to_string(t.path().join("eslint.config.js")).unwrap();
    assert!(flat.contains("{ ignores: ['packages/app/dist/**', 'packages/app/.next/**'] },"));
    let oxlintrc = fs::read_to_string(t.path().join(".oxlintrc.json")).unwrap();
    assert!(oxlintrc.contains("\"options\": { \"typeAware\": false },"), "{oxlintrc}");
    // The script still passes --type-aware.
    assert!(out.contains("package.json"), "{out}");
}

#[test]
fn fix_turns_on_isolated_declarations() {
    let t = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiclean_typecheck/tests/fixtures/isolated-pnpm");
    copy_dir(&fixture, t.path());
    configure(t.path(), &typecheck_only("isolated-declarations"));
    let (code, out) = run(t.path(), &["typecheck", "--fix"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("Fixed 1 findings"), "{out}");
    assert_eq!(
        fs::read_to_string(t.path().join("packages/ready/tsconfig.json")).unwrap(),
        "{\n  \"compilerOptions\": {\n    \"composite\": true,\n    \"outDir\": \"dist\",\n    \"isolatedDeclarations\": true\n  },\n  \"include\": [\"src\"]\n}\n"
    );
}

/// `--fix` applies the fixes of every rule of the command at once; two
/// rules' fixes to one file stack.
#[test]
fn fix_applies_every_rule_of_the_command() {
    let t = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiclean_typecheck/tests/fixtures/isolated-pnpm");
    copy_dir(&fixture, t.path());
    let (_, before) = run(t.path(), &["typecheck"]);
    assert!(before.contains("set \"compilerOptions.skipLibCheck\": true"), "{before}");
    assert!(before.contains("set \"compilerOptions.isolatedDeclarations\": true"), "{before}");
    let fixable = before.matches("fixable with --fix").count();

    let (_, out) = run(t.path(), &["typecheck", "--fix"]);
    assert!(out.contains(&format!("Fixed {fixable} findings")), "{before}\n{out}");
    assert!(!out.contains("fixable with --fix"), "{out}");
    let ready = fs::read_to_string(t.path().join("packages/ready/tsconfig.json")).unwrap();
    assert!(ready.contains("\"isolatedDeclarations\": true"), "{ready}");

    let (_, out) = run(t.path(), &["typecheck", "--fix"]);
    assert!(out.contains("Fixed 0 findings"), "{out}");
}

/// One `--fix` reaches the state the fixes of each rule reach run one after
/// another: once `references` makes a program `composite`, it is a
/// candidate for `isolatedDeclarations`, which the command's next pass
/// turns on.
#[test]
fn fix_repeats_until_no_new_fix_applies() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiclean_typecheck/tests/fixtures/references-pnpm");
    let at_once = TempDir::new().unwrap();
    copy_dir(&fixture, at_once.path());
    let (_, before) = run(at_once.path(), &["typecheck"]);
    let (_, out) = run(at_once.path(), &["typecheck", "--fix"]);
    let fixed: usize = out
        .lines()
        .find_map(|l| l.strip_prefix("● Fixed ")?.strip_suffix(" findings.")?.parse().ok())
        .unwrap_or_else(|| panic!("{out}"));
    assert!(fixed > before.matches("fixable with --fix").count(), "a later pass fixed more: {out}");
    let legacy = fs::read_to_string(at_once.path().join("packages/legacy/tsconfig.json")).unwrap();
    assert!(
        legacy.contains("\"composite\": true") && legacy.contains("\"isolatedDeclarations\": true"),
        "{legacy}"
    );
    let (_, out) = run(at_once.path(), &["typecheck", "--fix"]);
    assert!(out.contains("Fixed 0 findings"), "{out}");

    let one_by_one = TempDir::new().unwrap();
    copy_dir(&fixture, one_by_one.path());
    let configs = TempDir::new().unwrap();
    for rule in oxiclean_typecheck::rules() {
        let config = configs.path().join(format!("{rule}.json"));
        fs::write(&config, typecheck_only(rule)).unwrap();
        run(one_by_one.path(), &["typecheck", "--fix", "-c", config.to_str().unwrap()]);
    }
    for package in ["app", "legacy", "ui", "x", "y"] {
        let file = format!("packages/{package}/tsconfig.json");
        assert_eq!(
            fs::read_to_string(at_once.path().join(&file)).unwrap(),
            fs::read_to_string(one_by_one.path().join(&file)).unwrap(),
            "{file}"
        );
    }
}

/// A package whose import pulls in 101 modules, listed with an unused one.
fn heavy_fixture() -> TempDir {
    let t = TempDir::new().unwrap();
    write(
        t.path(),
        "package.json",
        r#"{"name":"app","main":"src/index.ts","dependencies":{"big":"*","lodash":"*"}}"#,
    );
    write(t.path(), "src/index.ts", "import 'big';");
    write(t.path(), "node_modules/big/package.json", r#"{"name":"big","main":"index.js"}"#);
    let requires: Vec<String> = (0..100).map(|i| format!("require('./m{i}');")).collect();
    write(t.path(), "node_modules/big/index.js", &requires.join("\n"));
    for i in 0..100 {
        write(t.path(), &format!("node_modules/big/m{i}.js"), "");
    }
    t
}

/// The severity of each finding of `rule` in a JSON report.
fn severities(v: &serde_json::Value, rule: &str) -> Vec<String> {
    v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["rule"] == rule)
        .map(|f| f["severity"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn heavy_dependencies_warn_and_unused_dependencies_fail_by_default() {
    let t = heavy_fixture();
    let (code, out) = run_stdout(t.path(), &["dependencies", "--format", "json"]);
    assert_eq!(code, 1, "the unused dependency fails the run: {out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(severities(&v, "heavy-dependencies"), ["warn"], "{out}");
    assert_eq!(severities(&v, "dependencies"), ["error"], "{out}");
    assert_eq!(v["rules"][1]["rule"], "heavy-dependencies");
    assert_eq!(v["rules"][1]["severity"], "warn");
    assert_eq!(v["rules"][1]["threshold"], 100);
    assert_eq!(v["rules"][1]["warnings"], 1);

    let (code, out) = run(t.path(), &["dependencies"]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("package 'big', imported by 1 files (pulls in 101 modules) [warn]"),
        "{out}"
    );
    assert!(out.contains("package 'lodash' (never used)\n"), "{out}");
    assert!(out.contains("1 error, 1 warning"), "{out}");

    // A heavy dependency alone only warns.
    write(
        t.path(),
        "package.json",
        r#"{"name":"app","main":"src/index.ts","dependencies":{"big":"*"}}"#,
    );
    let (code, out) = run(t.path(), &["dependencies"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("[warn]") && out.contains("No unused dependencies detected."), "{out}");
}

#[test]
fn config_severity_wins_over_the_default() {
    let t = heavy_fixture();
    configure(
        t.path(),
        r#"{ "rules": { "heavy-dependencies": "error", "dependencies": "warn" } }"#,
    );
    let (code, out) = run_stdout(t.path(), &["dependencies", "--format", "json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(severities(&v, "heavy-dependencies"), ["error"], "{out}");
    assert_eq!(severities(&v, "dependencies"), ["warn"], "{out}");
    assert_eq!(code, 1, "the heavy dependency fails the run now: {out}");

    let (_, out) = run_stdout(t.path(), &["dependencies", "--format", "sarif"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let levels: Vec<(&str, &str)> = v["runs"][0]["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["ruleId"].as_str().unwrap(), r["level"].as_str().unwrap()))
        .collect();
    assert_eq!(levels, [("dependencies", "warning"), ("heavy-dependencies", "error")]);

    configure(t.path(), r#"{ "rules": { "heavy-dependencies": "error", "dependencies": "off" } }"#);
    let (code, out) = run(t.path(), &["dependencies"]);
    assert_eq!(code, 1, "{out}");
    assert!(!out.contains("lodash") && !out.contains("[warn]"), "{out}");
}

#[test]
fn commands_run_only_the_rules_config_leaves_on() {
    let t = heavy_fixture();
    configure(t.path(), &off_config(&["heavy-dependencies", "catalog"]));
    let (code, out) = run_stdout(t.path(), &["dependencies", "--format", "json"]);
    assert_eq!(code, 1, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let rules: Vec<&str> =
        v["rules"].as_array().unwrap().iter().map(|r| r["rule"].as_str().unwrap()).collect();
    assert_eq!(rules, ["dependencies", "duplicate-versions"], "{out}");
    // Without heavy-dependencies, nothing follows imports into node_modules:
    // the package is one leaf module, not 101.
    assert_eq!(v["summary"]["files_analyzed"], 2, "{out}");

    let (_, out) = run_stdout(t.path(), &["dependencies", "--format", "sarif"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let ids: Vec<&str> = v["runs"][0]["tool"]["driver"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["dependencies", "duplicate-versions"]);

    let (_, out) = run(t.path(), &["dependencies"]);
    assert!(!out.contains("Heavy dependencies") && !out.contains("catalog"), "{out}");
    assert!(out.contains("Ran 2 rules (dependencies, duplicate-versions)"), "{out}");

    // An override can turn a rule back on for some files.
    configure(
        t.path(),
        r#"{ "rules": { "dependencies": "off", "catalog": "off", "duplicate-versions": "off",
             "heavy-dependencies": "off" },
             "overrides": [{ "files": ["package.json"], "rules": { "heavy-dependencies": "warn" } }] }"#,
    );
    let (code, out) = run(t.path(), &["dependencies"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("package 'big'") && !out.contains("Ran "), "{out}");
    // Off outside the override, so the rule's own level is "off", whatever
    // the override sets.
    let (_, out) = run_stdout(t.path(), &["dependencies", "--format", "json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["rules"][0]["rule"], "heavy-dependencies", "{out}");
    assert_eq!(v["rules"][0]["severity"], "off", "{out}");
    assert_eq!(severities(&v, "heavy-dependencies"), ["warn"], "{out}");

    configure(
        t.path(),
        &off_config(&[
            "unused-exports",
            "unused-types",
            "unused-enum-members",
            "unused-namespace-members",
        ]),
    );
    let (code, out) = run(t.path(), &["unused"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("The config file turns off every rule unused runs."), "{out}");
}

/// A rule that fails doesn't stop the others: in a pnpm workspace with a
/// catalog but no source files, the rules that build a module graph find no
/// entries, and the catalog is still checked.
#[test]
fn a_rule_that_fails_does_not_stop_its_command() {
    let t = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiclean_dependencies/tests/fixtures/catalog");
    copy_dir(&fixture, t.path());

    let (code, out) = run(t.path(), &["dependencies"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("catalog entry 'lodash' (default) (never referenced)"), "{out}");
    assert!(out.contains("No duplicate package versions detected."), "{out}");
    assert!(out.contains("✗ dependencies couldn't run: no entry files found"), "{out}");
    assert!(out.contains("✗ heavy-dependencies couldn't run: no entry files found"), "{out}");
    assert!(out.contains("2 couldn't run"), "{out}");

    let (code, out) = run_stdout(t.path(), &["dependencies", "--format", "json"]);
    assert_eq!(code, 1, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let errors: Vec<(&str, bool)> = v["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["rule"].as_str().unwrap(), r["error"].is_string()))
        .collect();
    assert_eq!(
        errors,
        [
            ("dependencies", true),
            ("heavy-dependencies", true),
            ("catalog", false),
            ("duplicate-versions", false)
        ],
        "{out}"
    );
    assert!(v["findings"].as_array().unwrap().iter().all(|f| f["rule"] == "catalog"), "{out}");

    // Turning off the rules that fail, and the one with findings, passes.
    configure(t.path(), &off_config(&["dependencies", "heavy-dependencies", "catalog"]));
    let (code, out) = run(t.path(), &["dependencies"]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("couldn't run"), "{out}");
}

/// A small monorepo of `manager` (pnpm, npm, Bun or Deno): a root tsconfig
/// that also checks both packages' files, an app that imports the ui
/// package's source without a project reference, and an unused package.
fn monorepo(manager: &str) -> TempDir {
    let t = TempDir::new().unwrap();
    let root = t.path();
    let ui_manifest = r#"{ "name": "@acme/ui", "version": "1.0.0", "exports": "./src/index.ts" }"#;
    let app_manifest = r#"{ "name": "@acme/app", "version": "1.0.0", "dependencies": { "@acme/ui": "workspace:*", "left-pad": "1.3.0" } }"#;
    match manager {
        "pnpm" => {
            write(root, "package.json", r#"{ "name": "root", "private": true }"#);
            write(root, "pnpm-workspace.yaml", "packages:\n  - packages/*\n");
            write(root, "pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
        }
        "npm" | "bun" => {
            let lockfile = if manager == "npm" { "package-lock.json" } else { "bun.lock" };
            write(
                root,
                "package.json",
                r#"{ "name": "root", "private": true, "workspaces": ["packages/*"] }"#,
            );
            write(root, lockfile, "{}");
        }
        "deno" => {
            write(root, "deno.json", r#"{ "workspace": ["./packages/ui", "./packages/app"] }"#);
        }
        _ => unreachable!("{manager}"),
    }
    let (ui, app) = if manager == "deno" {
        ("packages/ui/deno.json", "packages/app/deno.json")
    } else {
        ("packages/ui/package.json", "packages/app/package.json")
    };
    write(root, ui, ui_manifest);
    write(root, app, app_manifest);
    let tsconfig = r#"{ "compilerOptions": { "skipLibCheck": true, "noEmit": true, "strict": true }, "include": ["src"] }"#;
    write(root, "packages/ui/tsconfig.json", tsconfig);
    write(root, "packages/app/tsconfig.json", tsconfig);
    write(root, "tsconfig.json", &tsconfig.replace(r#"["src"]"#, r#"["packages"]"#));
    let mut index = String::new();
    for i in 1..=8 {
        write(
            root,
            &format!("packages/ui/src/part{i}.ts"),
            &format!("export const part{i} = {i};"),
        );
        index.push_str(&format!("export {{ part{i} }} from './part{i}';\n"));
    }
    write(root, "packages/ui/src/index.ts", &index);
    write(
        root,
        "packages/app/src/index.ts",
        "import { part1 } from '@acme/ui';\nconsole.log(part1);\n",
    );
    t
}

/// `oxiclean health` runs every command and ranks what they found by the
/// time fixing it saves, in monorepos of each package manager.
#[test]
fn health_ranks_findings_by_time_saved() {
    for manager in ["pnpm", "npm", "bun", "deno"] {
        let t = monorepo(manager);
        let (code, out) = run(t.path(), &["health"]);
        assert_eq!(code, 1, "{manager}: {out}");
        assert!(out.contains("Health: "), "{manager}: {out}");
        assert!(out.contains("Biggest wins:\n    1. "), "{manager}: {out}");
        assert!(out.contains("By rule (a week):"), "{manager}: {out}");
        // The graph commands ran too.
        assert!(!out.contains("couldn't run"), "{manager}: {out}");
        assert!(out.contains("bloat, barrels, depth, cycles, duplicates"), "{manager}: {out}");

        let (_, json) = run_stdout(t.path(), &["health", "--format", "json"]);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let savings = &v["savings"];
        let score = savings["health"]["score"].as_u64().unwrap();
        assert!((1..100).contains(&score), "{manager}: {savings}");
        // Checking eight files a second time costs more than an unused
        // package costs to install.
        let top: Vec<&str> = savings["top"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["rule"].as_str().unwrap())
            .collect();
        assert_eq!(top.first(), Some(&"overlap"), "{manager}: {savings}");
        let by_rule: Vec<f64> = savings["by_rule"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["weighted_hours_per_week"].as_f64().unwrap())
            .collect();
        assert!(by_rule.windows(2).all(|w| w[0] >= w[1]), "{manager}: {savings}");
        assert!(savings["assumptions"]["source_files"].as_u64().unwrap() >= 10, "{savings}");

        // The dependencies rule reads package.json files, which this Deno
        // workspace doesn't have.
        if manager == "deno" {
            continue;
        }
        let unused = v["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["rule"] == "dependencies")
            .unwrap_or_else(|| panic!("{manager}: {json}"));
        assert_eq!(unused["savings"]["seconds_per_run"]["install"], 0.25, "{manager}: {unused}");
    }
}

/// `savings` in the config file describes the team; twice the developers
/// wait twice as long, and CI is unchanged.
#[test]
fn savings_settings_scale_the_hours() {
    let t = monorepo("pnpm");
    let hours = |root: &Path| {
        let (_, json) = run_stdout(root, &["typecheck", "--format", "json"]);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let s = &v["savings"];
        (s["developer_hours_per_week"].as_f64().unwrap(), s["ci_hours_per_week"].as_f64().unwrap())
    };
    let (dev, ci) = hours(t.path());
    assert!(dev > 0.0 && ci > 0.0, "{dev} {ci}");
    configure(t.path(), r#"{ "savings": { "developers": 20 } }"#);
    let (dev2, ci2) = hours(t.path());
    assert!((dev2 - 2.0 * dev).abs() <= 0.011, "{dev} {dev2}");
    assert!((ci2 - ci).abs() <= 0.001, "{ci} {ci2}");

    // Every command's text report ends with the score and its biggest wins.
    let (_, out) = run(t.path(), &["typecheck"]);
    assert!(out.contains("Health: "), "{out}");
    assert!(out.contains("for 20 developers and 30 CI runs a day"), "{out}");
}
