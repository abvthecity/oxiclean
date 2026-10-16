//! The program map: for each package, which tsconfig programs type-check
//! its files, how each one reaches them, and how many of those checks
//! repeat work another program already does.
//!
//! A file is checked once by the program that owns it: a tsconfig in its
//! package, along with that tsconfig's variants (the package's configs in
//! the same directory, or that extend it or it extends). Unlike `overlap`,
//! a root config that a package's tsconfig extends is not a variant: when
//! its `include` takes in the package, it checks the package again. Every other
//! program that checks it repeats the work. A program reaches another
//! package's files in one of these ways ([`Cause`]):
//!
//! - its `include` matches them (a root tsconfig, a test tsconfig, a
//!   catch-all `tsconfig.eslint.json`, or build output left in `include`);
//! - it imports the package and the import resolves to source, without a
//!   project reference that would make it read `.d.ts` instead.
//!
//! The map feeds `oxiclean map`. Building it reads every file the programs
//! reach, so [`Progress`] reports how far it has got for a progress bar.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU8, AtomicUsize, Ordering},
};

use oxiclean_core::{CommonArgs, Project, Result};
use serde::Serialize;

use crate::{
    references::{covers, package_imports, reference_closure},
    tsconfig::{TsConfigFile, TsConfigs, relative},
};

/// What the program map is doing, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Phase {
    /// Finding the root, the config file and the workspace packages.
    Workspace,
    /// Reading tsconfig files and expanding what each program includes.
    Programs,
    /// Following imports from every program's files.
    Imports,
    /// Joining it all up per package.
    Packages,
    /// Finished.
    Done,
}

impl Phase {
    const ALL: [Self; 5] =
        [Self::Workspace, Self::Programs, Self::Imports, Self::Packages, Self::Done];

    /// What the phase is doing, for a progress line.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Workspace => "Reading workspace",
            Self::Programs => "Expanding tsconfig programs",
            Self::Imports => "Following imports",
            Self::Packages => "Mapping packages",
            Self::Done => "Done",
        }
    }
}

/// How far building a map has got. Shared between the thread that builds
/// it and one that draws progress; every counter is relaxed, as a
/// progress bar only needs a recent value.
#[derive(Debug, Default)]
pub struct Progress {
    phase: AtomicU8,
    done: AtomicUsize,
    total: AtomicUsize,
}

impl Progress {
    /// The current phase.
    #[must_use]
    pub fn phase(&self) -> Phase {
        let i = usize::from(self.phase.load(Ordering::Relaxed));
        Phase::ALL.get(i).copied().unwrap_or(Phase::Done)
    }

    /// Files read and files found to read so far in [`Phase::Imports`].
    /// The total grows as imports lead to more files.
    #[must_use]
    pub fn files(&self) -> (usize, usize) {
        let total = self.total.load(Ordering::Relaxed);
        (self.done.load(Ordering::Relaxed).min(total), total)
    }

    /// Moves on to `phase`.
    pub(crate) fn set_phase(&self, phase: Phase) {
        let i = Phase::ALL.iter().position(|p| *p == phase).unwrap_or(Phase::ALL.len() - 1);
        self.phase.store(u8::try_from(i).unwrap_or(u8::MAX), Ordering::Relaxed);
    }

    pub(crate) fn add_total(&self, n: usize) {
        self.total.fetch_add(n, Ordering::Relaxed);
    }

    pub(crate) fn tick(&self) {
        self.done.fetch_add(1, Ordering::Relaxed);
    }
}

/// How a program reaches a package's files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Cause {
    /// The package's own tsconfig: the one check that is needed.
    Owner,
    /// A variant of the owner, in its directory or extending it, which
    /// `overlap` treats as split on purpose.
    Variant,
    /// An import that resolves to the package's source, with no project
    /// reference to read its `.d.ts` instead.
    SourceImport,
    /// A lint tsconfig whose `include` takes in everything.
    CatchAll,
    /// A test tsconfig whose `include` takes in the source too.
    Tests,
    /// Another config's build output, checked as if it were source.
    BuildOutput,
    /// Any other `include` that reaches into the package.
    Include,
}

impl Cause {
    /// Every cause of repeated checks, in the order the map lists them.
    pub const REPEATS: [Self; 5] =
        [Self::SourceImport, Self::CatchAll, Self::Tests, Self::BuildOutput, Self::Include];

    /// A short name for a column.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Variant => "variant",
            Self::SourceImport => "source",
            Self::CatchAll => "lint",
            Self::Tests => "tests",
            Self::BuildOutput => "output",
            Self::Include => "include",
        }
    }

    /// What the cause is, in a line.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::Owner => "the package's own tsconfig",
            Self::Variant => "a variant of the package's own tsconfig",
            Self::SourceImport => "imports that reach source without a project reference",
            Self::CatchAll => "lint tsconfigs that include everything",
            Self::Tests => "test tsconfigs that include source too",
            Self::BuildOutput => "build output checked as source",
            Self::Include => "includes that reach into other packages",
        }
    }

    /// The usual fix, in a line.
    #[must_use]
    pub fn fix(self) -> &'static str {
        match self {
            Self::Owner | Self::Variant => "nothing to fix",
            Self::SourceImport => {
                "add a project reference, so tsc -b reads the package's .d.ts instead"
            }
            Self::CatchAll => {
                "switch typed linting to projectService and drop the catch-all tsconfig"
            }
            Self::Tests => "have the test tsconfig reference the package instead of including src",
            Self::BuildOutput => "add the output directory to \"exclude\"",
            Self::Include => "narrow \"include\" to stop at packages with their own tsconfig",
        }
    }

    /// Whether checks for this cause repeat work.
    #[must_use]
    pub fn repeats(self) -> bool {
        !matches!(self, Self::Owner | Self::Variant)
    }
}

/// One program that checks a package's files.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Checker {
    /// The tsconfig, relative to the root.
    pub program: String,
    /// How it reaches the files.
    pub cause: Cause,
    /// How many of the package's files it checks. For a source import,
    /// every file the package's programs check: an upper bound, as `tsc`
    /// only checks what the imports reach.
    pub files: usize,
    /// How many of those files a program listed before it already checks.
    /// Always 0 for the owner and its variants, and for a program whose
    /// files nothing else checks.
    pub repeats: usize,
    /// Where the program reaches the package: the import, as
    /// `file:line`, or one of the files its `include` matches.
    pub via: Option<String>,
}

/// One package and the programs that check it.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct PackageChecks {
    /// The package name, or `(root)` for files in no workspace package.
    pub name: String,
    /// Its directory, relative to the root.
    pub dir: String,
    /// The files any of its programs check. When only imports reach the
    /// package, the TypeScript files in its directory.
    pub files: usize,
    /// Every program that checks it: the owner, its variants, then the
    /// rest, most repeated checks first.
    pub checkers: Vec<Checker>,
    /// Checks of its files that repeat work.
    pub repeats: usize,
}

impl PackageChecks {
    /// How many programs check the package.
    #[must_use]
    pub fn checked_by(&self) -> usize {
        self.checkers.len()
    }

    /// The repeated checks of `cause`.
    #[must_use]
    pub fn repeats_of(&self, cause: Cause) -> usize {
        self.checkers.iter().filter(|c| c.cause == cause).map(|c| c.repeats).sum()
    }
}

/// The repeated checks one cause makes, across every package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct CauseRepeats {
    /// The cause.
    pub cause: Cause,
    /// The checks it repeats, across every package.
    pub repeats: usize,
}

/// Every package, with the programs that check it.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct ProgramMap {
    /// The project root.
    pub root: PathBuf,
    /// How many tsconfig programs there are.
    pub programs: usize,
    /// Every file check, across every program.
    pub checks: usize,
    /// The checks that repeat one another program already does.
    pub repeats: usize,
    /// Repeated checks by cause, in [`Cause::REPEATS`] order, without
    /// causes that have none.
    pub causes: Vec<CauseRepeats>,
    /// Every package any program checks, most repeated checks first.
    pub packages: Vec<PackageChecks>,
}

/// The name of the bucket for files in no workspace package.
pub const ROOT_PACKAGE: &str = "(root)";

/// Reads the workspace `common` names and builds its map, reporting each
/// phase to `progress`.
///
/// # Errors
/// Fails if the root can't be found or read, or the config file is invalid.
pub fn scan(common: &CommonArgs, progress: &Progress) -> Result<ProgramMap> {
    progress.set_phase(Phase::Workspace);
    let workspace = common.workspace()?;
    progress.set_phase(Phase::Programs);
    let configs = TsConfigs::load(&workspace.project);
    Ok(build(&workspace.project, &configs, progress))
}

/// Builds the map for `project`, whose tsconfig files are `configs`.
#[must_use]
pub(crate) fn build(project: &Project, configs: &TsConfigs, progress: &Progress) -> ProgramMap {
    progress.set_phase(Phase::Imports);
    let imports = package_imports(project, configs, Some(progress));
    progress.set_phase(Phase::Packages);
    let closure = reference_closure(configs);
    let programs: Vec<&TsConfigFile> = configs.programs().collect();
    let outputs: HashSet<PathBuf> =
        configs.configs.iter().flat_map(TsConfigFile::output_dirs).collect();
    let root = &project.root;
    let package_of = |path: &Path| project.package_for(path).unwrap_or(ROOT_PACKAGE);
    // Looking a package up scans every package, and programs share most
    // of their files, so lookups are kept per directory.
    let mut packages_by_dir: HashMap<&Path, &str> = HashMap::new();

    // The files each program includes, by package.
    let mut included: BTreeMap<String, Vec<(usize, Vec<&Path>)>> = BTreeMap::new();
    for (i, p) in programs.iter().enumerate() {
        let mut by_package: BTreeMap<&str, Vec<&Path>> = BTreeMap::new();
        for f in &p.root_files {
            let dir = f.parent().unwrap_or(f);
            let name = *packages_by_dir.entry(dir).or_insert_with(|| package_of(f));
            by_package.entry(name).or_default().push(f);
        }
        for (name, files) in by_package {
            included.entry(name.to_string()).or_default().push((i, files));
        }
    }

    let mut names: BTreeSet<String> = included.keys().cloned().collect();
    names.extend(imports.values().flat_map(|m| m.keys().cloned()));
    let mut packages: Vec<PackageChecks> = Vec::new();
    for name in names {
        let dir = project.workspace_package(&name).map_or_else(|| root.clone(), Path::to_path_buf);
        let reached = included.remove(&name).unwrap_or_default();
        let ctx =
            Package { project, programs: &programs, outputs: &outputs, name: &name, dir: &dir };
        let mut checkers = ctx.includers(&reached);
        let checked: HashSet<&Path> = reached.iter().flat_map(|(_, f)| f.iter().copied()).collect();
        let mut files = checked.len();
        // Programs that reach the package through imports instead, in
        // program order. Without any program that includes its files, the
        // first of them is the one check that's needed.
        let importers: Vec<(usize, String)> = programs
            .iter()
            .enumerate()
            .filter(|(i, _)| !reached.iter().any(|(r, _)| r == i))
            .filter_map(|(i, p)| {
                let site = imports.get(&p.path)?.get(&name)?;
                let referenced =
                    || closure.get(p.path.as_path()).is_some_and(|c| covers(project, c, &name));
                (site.source && !referenced())
                    .then(|| (i, format!("{}:{}", relative(root, &site.file), site.line)))
            })
            .collect();
        if files == 0 && !importers.is_empty() {
            files = typescript_files(project, &dir, &name);
        }
        for (n, (i, via)) in importers.into_iter().enumerate() {
            let first = checked.is_empty() && n == 0;
            checkers.push(Checker {
                program: relative(root, &programs[i].path),
                cause: Cause::SourceImport,
                files,
                repeats: if first { 0 } else { files },
                via: Some(via),
            });
        }
        if checkers.is_empty() {
            continue;
        }
        checkers.sort_by(|a, b| {
            rank(a.cause)
                .cmp(&rank(b.cause))
                .then_with(|| b.repeats.cmp(&a.repeats))
                .then_with(|| b.files.cmp(&a.files))
                .then_with(|| a.program.cmp(&b.program))
        });
        let repeats = checkers.iter().map(|c| c.repeats).sum();
        packages.push(PackageChecks { dir: relative(root, &dir), name, files, checkers, repeats });
    }
    packages.sort_by(|a, b| {
        b.repeats
            .cmp(&a.repeats)
            .then_with(|| b.checked_by().cmp(&a.checked_by()))
            .then_with(|| a.name.cmp(&b.name))
    });

    let checks = packages.iter().flat_map(|p| &p.checkers).map(|c| c.files).sum();
    let repeats = packages.iter().map(|p| p.repeats).sum();
    let causes = Cause::REPEATS
        .iter()
        .map(|&cause| CauseRepeats {
            cause,
            repeats: packages.iter().map(|p| p.repeats_of(cause)).sum(),
        })
        .filter(|c| c.repeats > 0)
        .collect();
    progress.set_phase(Phase::Done);
    ProgramMap { root: root.clone(), programs: programs.len(), checks, repeats, causes, packages }
}

/// Where a cause sorts among a package's checkers: the owner, then its
/// variants, then the rest.
fn rank(cause: Cause) -> u8 {
    match cause {
        Cause::Owner => 0,
        Cause::Variant => 1,
        _ => 2,
    }
}

/// One package, for working out who checks it.
struct Package<'a> {
    project: &'a Project,
    programs: &'a [&'a TsConfigFile],
    outputs: &'a HashSet<PathBuf>,
    name: &'a str,
    dir: &'a Path,
}

impl Package<'_> {
    fn owns(&self, program: &TsConfigFile) -> bool {
        self.project.package_for(&program.path).unwrap_or(ROOT_PACKAGE) == self.name
    }

    /// The programs whose `include` reaches the package's files, as
    /// checkers. A program's files count as repeats when the owner, a
    /// variant or a program with more files already checks them, so a
    /// program over files nothing else checks (an e2e tsconfig over its
    /// own folder, say) repeats nothing.
    fn includers(&self, reached: &[(usize, Vec<&Path>)]) -> Vec<Checker> {
        let programs = self.programs;
        // The owner: a program of the package itself, preferring its
        // `tsconfig.json`, then the most files.
        let owner = reached
            .iter()
            .filter(|(i, _)| self.owns(programs[*i]))
            .max_by_key(|(i, files)| {
                let p = programs[*i];
                (p.dir == self.dir && p.name() == "tsconfig.json", files.len())
            })
            .map(|(i, _)| *i);
        let mut ordered: Vec<(usize, Cause, &[&Path])> = reached
            .iter()
            .map(|(i, f)| {
                let p = programs[*i];
                let cause = match owner {
                    Some(o) if o == *i => Cause::Owner,
                    // Only the package's own configs count as variants: a
                    // root config the owner extends still checks it again.
                    Some(o) if self.owns(p) && related(programs[o], p) => Cause::Variant,
                    _ => self.include_cause(p, f),
                };
                (*i, cause, f.as_slice())
            })
            .collect();
        ordered.sort_by(|a, b| {
            rank(a.1)
                .cmp(&rank(b.1))
                .then_with(|| b.2.len().cmp(&a.2.len()))
                .then_with(|| programs[a.0].path.cmp(&programs[b.0].path))
        });
        let mut seen: HashSet<&Path> = HashSet::new();
        ordered
            .into_iter()
            .map(|(i, cause, files)| {
                let repeats = if cause.repeats() {
                    files.iter().filter(|f| seen.contains(*f)).count()
                } else {
                    0
                };
                seen.extend(files.iter().copied());
                Checker {
                    program: relative(&self.project.root, &programs[i].path),
                    cause,
                    files: files.len(),
                    repeats,
                    via: files.first().map(|f| relative(&self.project.root, f)),
                }
            })
            .collect()
    }

    /// Why `program` includes `files` of a package it doesn't own.
    fn include_cause(&self, program: &TsConfigFile, files: &[&Path]) -> Cause {
        let in_output = files
            .iter()
            .filter(|f| f.ancestors().skip(1).any(|a| self.outputs.contains(a)))
            .count();
        // Words of the file name and folders below the root, so where the
        // repo is checked out doesn't matter.
        let name = program.name().to_ascii_lowercase();
        let words: Vec<&str> = name.split(['.', '-', '_']).collect();
        let rel = program.dir.strip_prefix(&self.project.root).unwrap_or(Path::new(""));
        let folders: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
            .collect();
        let is_test = |w: &str| TEST_WORDS.contains(&w);
        if in_output * 2 > files.len() {
            Cause::BuildOutput
        } else if words.iter().any(|w| *w == "lint" || w.contains("eslint")) {
            Cause::CatchAll
        } else if words.iter().any(|w| is_test(w)) || folders.iter().any(|f| is_test(f)) {
            Cause::Tests
        } else {
            Cause::Include
        }
    }
}

/// Words in a tsconfig's name or folders that mark it as one for tests.
const TEST_WORDS: &[&str] = &[
    "test",
    "tests",
    "__tests__",
    "spec",
    "specs",
    "e2e",
    "vitest",
    "jest",
    "cypress",
    "playwright",
];

/// Whether `a` and `b` are variants of one project: in one directory, or
/// one extending the other.
fn related(a: &TsConfigFile, b: &TsConfigFile) -> bool {
    a.dir == b.dir || a.ancestors.contains(&b.path) || b.ancestors.contains(&a.path)
}

/// The TypeScript files of package `name` in `dir`, other than
/// declarations and files of packages inside it, for a package no program
/// includes.
fn typescript_files(project: &Project, dir: &Path, name: &str) -> usize {
    ignore::WalkBuilder::new(dir)
        .hidden(false)
        .filter_entry(|e| e.file_name() != "node_modules")
        .build()
        .filter_map(std::result::Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter(|e| {
            let n = e.file_name().to_string_lossy();
            [".ts", ".tsx", ".mts", ".cts"].iter().any(|x| n.ends_with(x))
                && ![".d.ts", ".d.mts", ".d.cts"].iter().any(|x| n.ends_with(x))
        })
        .filter(|e| project.package_for(e.path()) == Some(name))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(root: &Path, path: &str, text: &str) {
        let p = root.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    /// An npm workspace: `web` imports `ui` through `paths` into its
    /// source, and a catch-all lint tsconfig checks everything again.
    fn fixture() -> (TempDir, PathBuf) {
        let t = TempDir::new().unwrap();
        let root = t.path().canonicalize().unwrap();
        write(&root, "package.json", r#"{ "private": true, "workspaces": ["packages/*"] }"#);
        write(
            &root,
            "tsconfig.base.json",
            r#"{ "compilerOptions": { "baseUrl": ".", "paths": { "@acme/*": ["packages/*/src"] } } }"#,
        );
        write(
            &root,
            "tsconfig.eslint.json",
            r#"{ "extends": "./tsconfig.base.json", "include": ["packages"] }"#,
        );
        for (name, deps) in [("ui", ""), ("web", r#", "dependencies": { "@acme/ui": "*" }"#)] {
            write(
                &root,
                &format!("packages/{name}/package.json"),
                &format!(r#"{{ "name": "@acme/{name}"{deps} }}"#),
            );
            write(
                &root,
                &format!("packages/{name}/tsconfig.json"),
                r#"{ "extends": "../../tsconfig.base.json", "include": ["src"] }"#,
            );
        }
        write(&root, "packages/ui/src/index.ts", "export * from './button';\n");
        write(&root, "packages/ui/src/button.ts", "export const Button = 1;\n");
        write(
            &root,
            "packages/web/src/app.ts",
            "import { Button } from '@acme/ui';\nexport const app = Button;\n",
        );
        (t, root)
    }

    #[test]
    fn maps_owners_source_imports_and_catch_alls() {
        let (_t, root) = fixture();
        let project = Project::load(&root);
        let configs = TsConfigs::load(&project);
        let progress = Progress::default();
        let map = build(&project, &configs, &progress);
        assert_eq!(progress.phase(), Phase::Done);
        assert_eq!(progress.files().0, progress.files().1);

        let ui = map.packages.iter().find(|p| p.name == "@acme/ui").unwrap();
        assert_eq!(
            summary(ui),
            [
                ("packages/ui/tsconfig.json", Cause::Owner, 2, 0),
                ("packages/web/tsconfig.json", Cause::SourceImport, 2, 2),
                ("tsconfig.eslint.json", Cause::CatchAll, 2, 2),
            ]
        );
        assert_eq!(ui.checkers[1].via.as_deref(), Some("packages/web/src/app.ts:1"));
        assert_eq!(ui.repeats, 4);
        // ui repeats the most, so it comes first.
        assert_eq!(map.packages[0].name, "@acme/ui");
        assert_eq!(map.repeats, 5);
        assert_eq!(map.checks, 8);
        let causes: Vec<(Cause, usize)> = map.causes.iter().map(|c| (c.cause, c.repeats)).collect();
        assert_eq!(causes, [(Cause::SourceImport, 2), (Cause::CatchAll, 3)]);
    }

    #[test]
    fn a_root_config_the_owner_extends_still_repeats() {
        let (_t, root) = fixture();
        write(
            &root,
            "tsconfig.json",
            r#"{ "extends": "./tsconfig.base.json", "include": ["packages"] }"#,
        );
        write(
            &root,
            "packages/ui/tsconfig.json",
            r#"{ "extends": "../../tsconfig.json", "include": ["src"] }"#,
        );
        let project = Project::load(&root);
        let map = build(&project, &TsConfigs::load(&project), &Progress::default());
        let ui = map.packages.iter().find(|p| p.name == "@acme/ui").unwrap();
        let root_config = ui.checkers.iter().find(|c| c.program == "tsconfig.json").unwrap();
        assert_eq!((root_config.cause, root_config.repeats), (Cause::Include, 2));
    }

    fn summary(p: &PackageChecks) -> Vec<(&str, Cause, usize, usize)> {
        p.checkers.iter().map(|c| (c.program.as_str(), c.cause, c.files, c.repeats)).collect()
    }

    fn map_of(root: &Path) -> ProgramMap {
        let project = Project::load(root);
        build(&project, &TsConfigs::load(&project), &Progress::default())
    }

    fn package<'a>(map: &'a ProgramMap, name: &str) -> &'a PackageChecks {
        map.packages.iter().find(|p| p.name == name).unwrap()
    }

    #[test]
    fn programs_over_their_own_files_repeat_nothing() {
        let (_t, root) = fixture();
        // An e2e project over its own folder, a test config over source
        // and tests, and a variant beside the owner.
        write(&root, "packages/web/e2e/tsconfig.json", r#"{ "include": ["."] }"#);
        write(&root, "packages/web/e2e/app.spec.ts", "export {};\n");
        write(&root, "packages/web/tests/tsconfig.json", r#"{ "include": ["..", "."] }"#);
        write(&root, "packages/web/tsconfig.build.json", r#"{ "extends": "./tsconfig.json" }"#);
        let map = map_of(&root);
        let web = summary(package(&map, "@acme/web"));
        let find = |program: &str| *web.iter().find(|c| c.0 == program).unwrap();
        assert_eq!(find("packages/web/tsconfig.build.json").1, Cause::Variant);
        assert_eq!(find("packages/web/tsconfig.build.json").3, 0);
        // The e2e spec is checked by the e2e project, and again by the
        // test config that includes the whole package.
        let e2e = find("packages/web/e2e/tsconfig.json");
        let tests = find("packages/web/tests/tsconfig.json");
        assert_eq!(tests.1, Cause::Tests);
        assert_eq!(e2e.1, Cause::Tests);
        assert_eq!(e2e.3 + tests.3, 2, "{web:?}");
    }

    #[test]
    fn where_the_repo_is_checked_out_does_not_matter() {
        let t = TempDir::new().unwrap();
        let base = t.path().canonicalize().unwrap().join("latest");
        let (_f, fixture_root) = fixture();
        for e in ignore::WalkBuilder::new(&fixture_root).hidden(false).build().flatten() {
            let rel = e.path().strip_prefix(&fixture_root).unwrap();
            if e.file_type().is_some_and(|t| t.is_file()) {
                write(&base, &rel.to_string_lossy(), &fs::read_to_string(e.path()).unwrap());
            }
        }
        write(&base, "tsconfig.json", r#"{ "include": ["packages"] }"#);
        let map = map_of(&base);
        let ui = summary(package(&map, "@acme/ui"));
        assert!(ui.iter().any(|c| c.0 == "tsconfig.json" && c.1 == Cause::Include), "{ui:?}");
    }

    #[test]
    fn build_output_is_its_own_cause() {
        let (_t, root) = fixture();
        write(
            &root,
            "packages/ui/tsconfig.build.json",
            r#"{ "include": ["src"], "compilerOptions": { "outDir": "dist" } }"#,
        );
        write(&root, "packages/ui/dist/index.d.ts", "export {};\n");
        write(&root, "packages/ui/dist/button.d.ts", "export {};\n");
        write(&root, "tsconfig.json", r#"{ "include": ["packages/ui/dist"] }"#);
        let map = map_of(&root);
        let ui = summary(package(&map, "@acme/ui"));
        assert!(ui.iter().any(|c| c.0 == "tsconfig.json" && c.1 == Cause::BuildOutput), "{ui:?}");
    }

    #[test]
    fn a_package_only_imports_reach_is_checked_once_per_extra_importer() {
        let (_t, root) = fixture();
        fs::remove_file(root.join("packages/ui/tsconfig.json")).unwrap();
        fs::remove_file(root.join("tsconfig.eslint.json")).unwrap();
        write(&root, "packages/admin/package.json", r#"{ "name": "@acme/admin" }"#);
        write(
            &root,
            "packages/admin/tsconfig.json",
            r#"{ "extends": "../../tsconfig.base.json", "include": ["src"] }"#,
        );
        write(
            &root,
            "packages/admin/src/main.ts",
            "import { Button } from '@acme/ui';\nexport const m = Button;\n",
        );
        let map = map_of(&root);
        let ui = package(&map, "@acme/ui");
        assert_eq!(ui.files, 2);
        let repeats: Vec<(Cause, usize)> =
            ui.checkers.iter().map(|c| (c.cause, c.repeats)).collect();
        assert_eq!(repeats, [(Cause::SourceImport, 2), (Cause::SourceImport, 0)], "{ui:?}");
        assert_eq!(ui.repeats, 2);
    }

    #[test]
    fn a_reference_stops_the_source_import() {
        let (_t, root) = fixture();
        write(
            &root,
            "packages/web/tsconfig.json",
            r#"{ "extends": "../../tsconfig.base.json", "include": ["src"], "references": [{ "path": "../ui" }] }"#,
        );
        let project = Project::load(&root);
        let map = build(&project, &TsConfigs::load(&project), &Progress::default());
        let ui = map.packages.iter().find(|p| p.name == "@acme/ui").unwrap();
        assert!(ui.checkers.iter().all(|c| c.cause != Cause::SourceImport));
    }
}
