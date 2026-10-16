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
    collections::{BTreeMap, BTreeSet, HashMap},
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
        (self.done.load(Ordering::Relaxed), self.total.load(Ordering::Relaxed))
    }

    /// Moves on to `phase`.
    pub fn set_phase(&self, phase: Phase) {
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
    /// the files the owner checks: an upper bound, as `tsc` only checks
    /// what the imports reach.
    pub files: usize,
    /// Whether this check repeats one another program already does.
    pub repeat: bool,
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
    /// The files its owner checks.
    pub files: usize,
    /// Every program that checks it, the owner first, then by files.
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
        self.checkers.iter().filter(|c| c.repeat && c.cause == cause).map(|c| c.files).sum()
    }
}

/// The repeated checks one cause makes, across every package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct CauseRepeats {
    pub cause: Cause,
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
pub fn build(project: &Project, configs: &TsConfigs, progress: &Progress) -> ProgramMap {
    progress.set_phase(Phase::Imports);
    let imports = package_imports(project, configs, Some(progress));
    progress.set_phase(Phase::Packages);
    let closure = reference_closure(configs);
    let programs: Vec<&TsConfigFile> = configs.programs().collect();
    let outputs: Vec<PathBuf> =
        configs.configs.iter().flat_map(TsConfigFile::output_dirs).collect();
    let root = &project.root;
    let package_of = |path: &Path| project.package_for(path).unwrap_or(ROOT_PACKAGE);
    // Looking a package up scans every package, and programs share most
    // of their files, so lookups are kept per directory.
    let mut packages_by_dir: HashMap<&Path, &str> = HashMap::new();
    let dir_of = |name: &str| {
        project.workspace_package(name).map_or_else(|| root.clone(), Path::to_path_buf)
    };

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

    let mut packages: Vec<PackageChecks> = Vec::new();
    let mut names: BTreeSet<String> = included.keys().cloned().collect();
    names.extend(imports.values().flat_map(|m| m.keys().cloned()));
    for name in names {
        let dir = dir_of(&name);
        let reached = included.remove(&name).unwrap_or_default();
        // The owner: a program of the package itself that includes its
        // files, preferring its `tsconfig.json`, then the most files.
        let owner = reached
            .iter()
            .filter(|(i, _)| package_of(&programs[*i].path) == name)
            .max_by_key(|(i, files)| {
                (programs[*i].dir == dir && programs[*i].name() == "tsconfig.json", files.len())
            })
            .map(|(i, _)| *i);
        let files = owner
            .and_then(|o| reached.iter().find(|(i, _)| *i == o))
            .or_else(|| reached.iter().max_by_key(|(_, f)| f.len()))
            .map_or(0, |(_, f)| f.len());
        // Without an owner, the program with the most files is the one
        // check that counts.
        let baseline =
            owner.or_else(|| reached.iter().max_by_key(|(_, f)| f.len()).map(|(i, _)| *i));

        let mut checkers: Vec<Checker> = reached
            .iter()
            .map(|(i, f)| {
                let p = programs[*i];
                let cause = match owner {
                    Some(o) if o == *i => Cause::Owner,
                    // Only the package's own configs count as variants: a
                    // root config the owner extends still checks it again.
                    Some(o) if package_of(&p.path) == name && related(programs[o], p) => {
                        Cause::Variant
                    }
                    _ => include_cause(p, f, &outputs),
                };
                Checker {
                    program: relative(root, &p.path),
                    cause,
                    files: f.len(),
                    repeat: cause.repeats() && baseline != Some(*i),
                    via: f.first().map(|f| relative(root, f)),
                }
            })
            .collect();
        for (i, p) in programs.iter().enumerate() {
            let Some(site) = imports.get(&p.path).and_then(|m| m.get(&name)) else { continue };
            let referenced =
                closure.get(p.path.as_path()).is_some_and(|c| covers(project, c, &name));
            if !site.source || referenced || reached.iter().any(|(r, _)| *r == i) {
                continue;
            }
            checkers.push(Checker {
                program: relative(root, &p.path),
                cause: Cause::SourceImport,
                files,
                repeat: true,
                via: Some(format!("{}:{}", relative(root, &site.file), site.line)),
            });
        }
        if checkers.is_empty() {
            continue;
        }
        checkers.sort_by(|a, b| {
            (a.repeat, std::cmp::Reverse(a.files), &a.program).cmp(&(
                b.repeat,
                std::cmp::Reverse(b.files),
                &b.program,
            ))
        });
        let repeats = checkers.iter().filter(|c| c.repeat).map(|c| c.files).sum();
        packages.push(PackageChecks { dir: relative(root, &dir), name, files, checkers, repeats });
    }
    packages.sort_by(|a, b| {
        b.repeats
            .cmp(&a.repeats)
            .then(b.checked_by().cmp(&a.checked_by()))
            .then(a.name.cmp(&b.name))
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

/// Whether `a` and `b` are variants of one project: in one directory, or
/// one extending the other.
fn related(a: &TsConfigFile, b: &TsConfigFile) -> bool {
    a.dir == b.dir || a.ancestors.contains(&b.path) || b.ancestors.contains(&a.path)
}

/// Why `program` includes `files` of a package it doesn't own.
fn include_cause(program: &TsConfigFile, files: &[&Path], outputs: &[PathBuf]) -> Cause {
    let in_output = files.iter().filter(|f| outputs.iter().any(|o| f.starts_with(o))).count();
    let name = program.name().to_ascii_lowercase();
    let dir = program.dir.to_string_lossy().to_ascii_lowercase();
    let tests = ["test", "spec", "vitest", "jest", "e2e"];
    if in_output * 2 > files.len() {
        Cause::BuildOutput
    } else if name.contains("lint") {
        Cause::CatchAll
    } else if tests
        .iter()
        .any(|t| name.contains(t) || dir.ends_with(t) || dir.ends_with(&format!("{t}s")))
    {
        Cause::Tests
    } else {
        Cause::Include
    }
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
        let summary: Vec<(&str, Cause, usize, bool)> =
            ui.checkers.iter().map(|c| (c.program.as_str(), c.cause, c.files, c.repeat)).collect();
        assert_eq!(
            summary,
            [
                ("packages/ui/tsconfig.json", Cause::Owner, 2, false),
                ("packages/web/tsconfig.json", Cause::SourceImport, 2, true),
                ("tsconfig.eslint.json", Cause::CatchAll, 2, true),
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
        assert_eq!((root_config.cause, root_config.repeat), (Cause::Include, true));
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
