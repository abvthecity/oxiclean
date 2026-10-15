//! SARIF conformance: every rule's `--format sarif` output must validate
//! against the OASIS SARIF 2.1.0 schema and meet what GitHub code scanning
//! requires on top of it.
//!
//! The schema is vendored in `tests/fixtures/sarif-schema`; the fixture
//! projects are in `tests/fixtures/sarif`. See
//! <https://docs.github.com/en/code-security/code-scanning/integrating-with-code-scanning/sarif-support-for-code-scanning>
//! for GitHub's requirements.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

/// The rules with SARIF output. Add new rules here with a fixture under
/// `tests/fixtures/sarif/<rule>` that produces at least one finding.
const RULES: &[&str] = &[
    "bloat",
    "depth",
    "cycles",
    "duplicates",
    "unused-exports",
    "unused-types",
    "unused-enum-members",
    "unused-namespace-members",
    "dependencies",
    "catalog",
    "barrels",
    "heavy-dependencies",
    "duplicate-versions",
    "program-files",
    "overlap",
    "references",
    "tsconfig",
    "ambient-types",
    "isolated-declarations",
];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Runs `oxiclean <rule> --format sarif` on `root` and parses stdout.
fn sarif(rule: &str, root: &Path) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_oxiclean"))
        .args([rule, "--format", "sarif", "--root"])
        .arg(root)
        .current_dir(root)
        .env("NO_COLOR", "1")
        .env_remove("RUST_LOG")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{rule}: stdout is not JSON ({e}); stderr: {stderr}"))
}

fn validator() -> jsonschema::Validator {
    let path = fixtures().join("sarif-schema/sarif-schema-2.1.0.json");
    let schema: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    jsonschema::draft4::options().should_validate_formats(true).build(&schema).unwrap()
}

/// Panics with every schema violation in `doc`.
fn assert_schema_valid(validator: &jsonschema::Validator, rule: &str, doc: &Value) {
    let errors: Vec<String> =
        validator.iter_errors(doc).map(|e| format!("  {}: {e}", e.instance_path())).collect();
    assert!(
        errors.is_empty(),
        "{rule}: SARIF does not match the 2.1.0 schema:\n{}",
        errors.join("\n")
    );
}

/// Decodes `%XX` escapes.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap();
            out.push(u8::from_str_radix(hex, 16).unwrap());
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

/// Checks a `physicalLocation` the way code scanning reads it: a URI relative
/// to the checkout that names a real file, and a region on one of its lines.
fn check_physical_location(rule: &str, root: &Path, loc: &Value) {
    let artifact = &loc["artifactLocation"];
    let uri = artifact["uri"].as_str().unwrap_or_else(|| panic!("{rule}: no uri in {loc}"));
    assert_eq!(artifact["uriBaseId"], "%SRCROOT%", "{rule}: {loc}");
    assert!(
        !uri.starts_with('/') && !uri.contains('\\') && !uri.split('/').any(|s| s == ".."),
        "{rule}: uri must be relative to the root: {uri}"
    );
    let first = uri.split('/').next().unwrap();
    assert!(!first.contains(':'), "{rule}: uri's first segment reads as a scheme: {uri}");
    let file = root.join(percent_decode(uri));
    let source = fs::read_to_string(&file)
        .unwrap_or_else(|e| panic!("{rule}: uri {uri} does not name a file ({e})"));

    let line = loc["region"]["startLine"]
        .as_u64()
        .unwrap_or_else(|| panic!("{rule}: code scanning needs region.startLine: {loc}"));
    let lines = source.lines().count() as u64;
    assert!((1..=lines.max(1)).contains(&line), "{rule}: line {line} of {uri} ({lines} lines)");
}

/// What GitHub code scanning requires beyond the schema.
fn check_code_scanning(rule: &str, root: &Path, doc: &Value) {
    let runs = doc["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1, "{rule}: one run per invocation");
    let run = &runs[0];

    let driver = &run["tool"]["driver"];
    assert_eq!(driver["name"], "oxiclean");
    let rules = driver["rules"].as_array().unwrap();
    let ids: Vec<&str> = rules.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [rule], "{rule}: rule metadata");
    for r in rules {
        let text = r["shortDescription"]["text"].as_str().unwrap_or_default();
        assert!(!text.is_empty(), "{rule}: rule {} has no shortDescription", r["id"]);
    }

    // Uploads with the same tool and category replace each other's alerts.
    assert_eq!(run["automationDetails"]["id"], format!("oxiclean/{rule}/"));

    let results = run["results"].as_array().unwrap();
    let mut fingerprints = HashSet::new();
    for result in results {
        let id = result["ruleId"].as_str().unwrap_or_else(|| panic!("{rule}: no ruleId"));
        assert!(ids.contains(&id), "{rule}: ruleId {id} is not in tool.driver.rules");
        if let Some(index) = result["ruleIndex"].as_u64() {
            assert_eq!(ids[usize::try_from(index).unwrap()], id, "{rule}: ruleIndex");
        }
        assert!(
            !result["message"]["text"].as_str().unwrap_or_default().is_empty(),
            "{rule}: result without message.text"
        );

        let locations = result["locations"].as_array().unwrap();
        assert_eq!(locations.len(), 1, "{rule}: one primary location: {result}");
        check_physical_location(rule, root, &locations[0]["physicalLocation"]);

        if let Some(related) = result["relatedLocations"].as_array() {
            let mut related_ids = HashSet::new();
            for loc in related {
                assert!(related_ids.insert(loc["id"].as_u64().unwrap()), "{rule}: duplicate id");
                check_physical_location(rule, root, &loc["physicalLocation"]);
            }
        }

        // Without partialFingerprints, code scanning computes its own from
        // the source, which only works when the upload can read the file.
        let fps = result["partialFingerprints"].as_object().unwrap_or_else(|| {
            panic!("{rule}: result without partialFingerprints: {result}");
        });
        assert!(!fps.is_empty() && fps.values().all(|v| v.as_str().is_some_and(|s| !s.is_empty())));
        assert!(
            fingerprints.insert((id.to_owned(), Value::Object(fps.clone()))),
            "{rule}: two results share a fingerprint, so code scanning merges them: {result}"
        );
    }
}

#[test]
fn every_rule_emits_conformant_sarif() {
    let validator = validator();
    for rule in RULES {
        let root = fixtures().join("sarif").join(rule);
        let doc = sarif(rule, &root);
        assert!(
            !doc["runs"][0]["results"].as_array().unwrap().is_empty(),
            "{rule}: the fixture must produce findings"
        );
        assert_schema_valid(&validator, rule, &doc);
        check_code_scanning(rule, &root, &doc);
    }
}

#[test]
fn sarif_without_findings_is_conformant() {
    let validator = validator();
    let root = fixtures().join("sarif/clean");
    for rule in RULES {
        let doc = sarif(rule, &root);
        assert_eq!(doc["runs"][0]["results"], Value::Array(vec![]), "{rule}");
        assert_schema_valid(&validator, rule, &doc);
        check_code_scanning(rule, &root, &doc);
    }
}

#[test]
fn file_names_are_percent_encoded() {
    let root = fixtures().join("sarif/bloat");
    let doc = sarif("bloat", &root);
    let uri = &doc["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["artifactLocation"]
        ["uri"];
    assert_eq!(uri, "src/main%20entry.ts");
}

#[test]
fn fingerprints_ignore_line_moves() {
    let fixture = fixtures().join("sarif/depth");
    let before = sarif("depth", &fixture);

    // The same project with a blank line above the import.
    let t = tempfile::TempDir::new().unwrap();
    for entry in ["src/one.ts", "src/two.ts", "src/three.ts", "src/four.ts", ".oxicleanrc.json"] {
        fs::create_dir_all(t.path().join("src")).unwrap();
        fs::copy(fixture.join(entry), t.path().join(entry)).unwrap();
    }
    let main = fs::read_to_string(fixture.join("src/main.ts")).unwrap();
    fs::write(t.path().join("src/main.ts"), format!("\n{main}")).unwrap();
    let after = sarif("depth", t.path());

    let result = |doc: &Value| doc["runs"][0]["results"][0].clone();
    let line = |doc: &Value| {
        result(doc)["locations"][0]["physicalLocation"]["region"]["startLine"].as_u64().unwrap()
    };
    assert_eq!(line(&after), line(&before) + 1);
    assert_eq!(result(&after)["partialFingerprints"], result(&before)["partialFingerprints"]);
}
