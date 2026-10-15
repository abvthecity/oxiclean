//! Build output a linter may walk: which directories are generated, and
//! whether a linter's ignores, `.gitignore` or `.eslintignore` files skip
//! them.

use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    rc::Rc,
};

use globset::{GlobBuilder, GlobMatcher};
use ignore::{
    WalkBuilder,
    gitignore::{Gitignore, GitignoreBuilder},
};
use oxiclean_typecheck::TsConfigs;

/// Directories frameworks and tools generate, wherever they are: they
/// hold nothing but output.
const GENERATED_DIRS: &[&str] = &[
    ".next",
    ".nuxt",
    ".output",
    ".svelte-kit",
    ".docusaurus",
    ".vercel",
    ".netlify",
    ".turbo",
    ".astro",
    ".angular",
    ".parcel-cache",
    ".wrangler",
    "storybook-static",
];

/// Usual names of build output directories. A directory of one of these
/// names counts as output only when git ignores it, as some projects keep
/// source in `build/` or `out/`.
const BUILD_DIRS: &[&str] = &["dist", "build", "out", "coverage", ".nyc_output"];

/// Extensions of the files ESLint and oxlint parse.
const LINTED_EXTENSIONS: &[&str] =
    &["js", "mjs", "cjs", "jsx", "ts", "mts", "cts", "tsx", "vue", "svelte", "astro"];

/// How many files of a directory are counted.
const MAX_COUNTED: usize = 100_000;

/// Why a directory is build output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Why {
    /// A tool's own output directory, as `.next`.
    Generated,
    /// A tsconfig's `outDir` or `declarationDir`: the tsconfig and the
    /// line that sets it.
    OutDir(PathBuf, u32),
    /// A usual output name that git ignores.
    Named,
}

/// A directory of build output.
#[derive(Debug, Clone)]
pub(crate) struct OutputDir {
    pub path: PathBuf,
    pub why: Why,
    /// Files in it linters parse, when it exists.
    pub files: usize,
    pub gitignored: bool,
}

/// Ignore files read so far, by directory: `None` where there is none.
type IgnoreFiles = HashMap<PathBuf, Option<Gitignore>>;

/// The `.gitignore` files of a project, read as needed.
pub(crate) struct GitIgnores {
    root: PathBuf,
    gitignores: IgnoreFiles,
    eslintignores: IgnoreFiles,
}

impl GitIgnores {
    pub(crate) fn new(root: &Path) -> Self {
        Self { root: root.to_path_buf(), gitignores: HashMap::new(), eslintignores: HashMap::new() }
    }

    /// Whether a `.gitignore` file from the root down to `path` ignores
    /// it or a directory it is in.
    pub(crate) fn ignored(&mut self, path: &Path, is_dir: bool) -> bool {
        let Ok(rel) = path.strip_prefix(&self.root) else { return false };
        let mut dir = self.root.clone();
        let mut ignored =
            matches(read(&mut self.gitignores, &dir, ".gitignore"), path, is_dir).unwrap_or(false);
        for c in rel.parent().into_iter().flat_map(Path::components) {
            dir.push(c);
            ignored = matches(read(&mut self.gitignores, &dir, ".gitignore"), path, is_dir)
                .unwrap_or(ignored);
        }
        ignored
    }

    /// Whether the root's `.eslintignore` ignores `path`: ESLint 8 reads
    /// only the one in the directory it runs in.
    pub(crate) fn eslintignored(&mut self, path: &Path, is_dir: bool) -> bool {
        let gi = read(&mut self.eslintignores, &self.root, ".eslintignore");
        matches(gi, path, is_dir).unwrap_or(false)
    }
}

/// The ignore file `file_name` in `dir`, read once into `cache`.
fn read<'c>(cache: &'c mut IgnoreFiles, dir: &Path, file_name: &str) -> Option<&'c Gitignore> {
    if !cache.contains_key(dir) {
        let file = dir.join(file_name);
        let gi = file.is_file().then(|| Gitignore::new(&file).0);
        cache.insert(dir.to_path_buf(), gi);
    }
    cache.get(dir)?.as_ref()
}

/// Whether ignore file `gi` ignores `path` (`Some(true)`), brings it back
/// with a negated pattern (`Some(false)`), or says nothing of it.
fn matches(gi: Option<&Gitignore>, path: &Path, is_dir: bool) -> Option<bool> {
    let m = gi?.matched_path_or_any_parents(path, is_dir);
    if m.is_ignore() {
        Some(true)
    } else if m.is_whitelist() {
        Some(false)
    } else {
        None
    }
}

/// The build output directories under `root`: generated directories and
/// usual output names in each package directory, and the output
/// directories of tsconfig programs that emit.
pub(crate) fn output_dirs(
    root: &Path,
    package_dirs: &[PathBuf],
    tsconfigs: &TsConfigs,
    gitignores: &mut GitIgnores,
) -> Vec<OutputDir> {
    let mut found: BTreeMap<PathBuf, Why> = BTreeMap::new();
    let mut dirs: Vec<&Path> = package_dirs.iter().map(PathBuf::as_path).collect();
    dirs.push(root);
    for dir in dirs {
        for name in GENERATED_DIRS {
            let p = dir.join(name);
            if p.is_dir() {
                found.entry(p).or_insert(Why::Generated);
            }
        }
        for name in BUILD_DIRS {
            let p = dir.join(name);
            if p.is_dir() {
                found.entry(p).or_insert(Why::Named);
            }
        }
    }
    for c in tsconfigs.programs() {
        if c.flag("noEmit") == Some(true) || c.partial {
            continue;
        }
        for (option, out) in
            ["outDir", "declarationDir"].into_iter().filter_map(|o| Some((o, c.path_option(o)?)))
        {
            // tsc writes each file under `out` at its path from `rootDir`,
            // or from the directory its files share.
            let sources: Vec<&PathBuf> = c
                .root_files
                .iter()
                .filter(|f| !f.file_name().is_some_and(|n| n.to_string_lossy().contains(".d.")))
                .collect();
            let Some(common) = common_dir(&sources) else { continue };
            let base = c.path_option("rootDir").unwrap_or_else(|| common.clone());
            let out = match common.strip_prefix(&base) {
                Ok(sub) => out.join(sub),
                Err(_) => out,
            };
            // Without a trailing slash, which matchers would read as part
            // of the name.
            let out: PathBuf = out.components().collect();
            let inside = |p: &Path| p.starts_with(&out);
            let holds_source = c.dir.starts_with(&out) || c.root_files.iter().any(|f| inside(f));
            let in_deps = out.components().any(|c| c.as_os_str() == "node_modules");
            if holds_source || in_deps || !out.starts_with(root) || out == root {
                continue;
            }
            let line = c.line_of(&["compilerOptions", option]);
            found.insert(out, Why::OutDir(c.path.clone(), line));
        }
    }
    found
        .into_iter()
        .filter_map(|(path, why)| {
            let files = if path.is_dir() { count_linted(&path) } else { 0 };
            let gitignored = gitignores.ignored(&path, true);
            let generated = match why {
                Why::Named => gitignored,
                Why::Generated | Why::OutDir(..) => true,
            };
            generated.then_some(OutputDir { path, why, files, gitignored })
        })
        .collect()
}

/// The deepest directory that holds every one of `files`.
fn common_dir(files: &[&PathBuf]) -> Option<PathBuf> {
    let mut common = files.first()?.parent()?.to_path_buf();
    for f in &files[1..] {
        while !f.starts_with(&common) {
            if !common.pop() {
                return None;
            }
        }
    }
    Some(common)
}

/// The files under `dir` linters parse, up to [`MAX_COUNTED`].
fn count_linted(dir: &Path) -> usize {
    WalkBuilder::new(dir)
        .standard_filters(false)
        .filter_entry(|e| e.file_name() != "node_modules")
        .build()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_type().is_some_and(|t| t.is_file())
                && e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| LINTED_EXTENSIONS.contains(&x))
        })
        .take(MAX_COUNTED)
        .count()
}

/// `path` relative to `dir`, with `/`, or `None` outside it.
pub(crate) fn relative(dir: &Path, path: &Path) -> Option<String> {
    let mut out = String::new();
    for c in path.strip_prefix(dir).ok()?.components() {
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(&c.as_os_str().to_string_lossy());
    }
    Some(out)
}

/// Built gitignore matchers, by directory and then patterns, so that a
/// lookup borrows both.
type GitignoreCache = HashMap<PathBuf, HashMap<Vec<String>, Option<Rc<Gitignore>>>>;

/// Compiled globs and gitignore matchers, kept for one run: the same
/// patterns are matched against every file.
#[derive(Default)]
pub(crate) struct Globs {
    globs: RefCell<HashMap<String, Option<Rc<GlobMatcher>>>>,
    gitignores: RefCell<GitignoreCache>,
}

impl Globs {
    /// `pattern` compiled the way minimatch reads it, where `*` stays
    /// within a directory, or `None` when it isn't a valid glob.
    pub(crate) fn glob(&self, pattern: &str) -> Option<Rc<GlobMatcher>> {
        if let Some(m) = self.globs.borrow().get(pattern) {
            return m.clone();
        }
        let m = GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .ok()
            .map(|g| Rc::new(g.compile_matcher()));
        self.globs.borrow_mut().insert(pattern.to_string(), m.clone());
        m
    }

    /// Whether flat config `ignores` patterns, relative to the config's
    /// directory, ignore directory `rel` (relative to it too). Patterns
    /// are matched as ESLint does, with minimatch: `dist` is only the
    /// top-level `dist`. A negated pattern may bring files back, so with
    /// one, anything counts as ignored.
    pub(crate) fn flat_ignores(&self, patterns: &[String], rel: &str) -> bool {
        if patterns.iter().any(|p| p.starts_with('!')) {
            return true;
        }
        let candidates = ancestors_and_child(rel);
        patterns.iter().any(|p| {
            let p = p.trim_start_matches("./").trim_end_matches('/');
            self.glob(p).is_some_and(|m| candidates.iter().any(|c| m.is_match(c)))
        })
    }

    /// Whether gitignore-style `patterns`, rooted at `dir`, ignore
    /// directory `path` or every file in it.
    pub(crate) fn gitignore_style_ignores(
        &self,
        dir: &Path,
        patterns: &[String],
        path: &Path,
    ) -> bool {
        if !path.starts_with(dir) {
            return false;
        }
        let cached = self.gitignores.borrow().get(dir).and_then(|m| m.get(patterns)).cloned();
        let gi = cached.unwrap_or_else(|| {
            let mut b = GitignoreBuilder::new(dir);
            let ok = patterns.iter().all(|p| b.add_line(None, p).is_ok());
            let gi = if ok { b.build().ok().map(Rc::new) } else { None };
            self.gitignores
                .borrow_mut()
                .entry(dir.to_path_buf())
                .or_default()
                .insert(patterns.to_vec(), gi.clone());
            gi
        });
        // Patterns that don't build might ignore anything.
        let Some(gi) = gi else { return true };
        gi.matched_path_or_any_parents(path, true).is_ignore()
            || gi.matched_path_or_any_parents(path.join("x.js"), false).is_ignore()
    }

    /// Whether file `rel`, relative to a config's directory, matches flat
    /// config `files` or `ignores` patterns.
    pub(crate) fn flat_matches(&self, patterns: &[String], rel: &str) -> bool {
        patterns.iter().any(|p| self.flat_pattern_matches(p, rel))
    }

    /// Whether file `rel` matches flat config pattern `p`.
    pub(crate) fn flat_pattern_matches(&self, p: &str, rel: &str) -> bool {
        let p = p.trim_start_matches("./");
        let p: Cow<'_, str> = if p.ends_with('/') { format!("{p}**").into() } else { p.into() };
        self.glob(&p).is_some_and(|m| m.is_match(rel))
    }

    /// Whether file `rel`, relative to a config's directory, matches
    /// eslintrc `overrides` patterns: those without a slash match the
    /// file name.
    pub(crate) fn eslintrc_matches(&self, patterns: &[String], rel: &str) -> bool {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        patterns.iter().any(|p| {
            let p = p.trim_start_matches("./");
            let target = if p.contains('/') { rel } else { name };
            self.glob(p).is_some_and(|m| m.is_match(target))
        })
    }
}

/// `rel`, each directory above it, and a file in it.
fn ancestors_and_child(rel: &str) -> Vec<String> {
    let mut out = vec![format!("{rel}/x.js")];
    let mut cur = String::new();
    for part in rel.split('/') {
        if !cur.is_empty() {
            cur.push('/');
        }
        cur.push_str(part);
        out.push(cur.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn flat_patterns_are_rooted() {
        let g = Globs::default();
        assert!(g.flat_ignores(&s(&["dist"]), "dist"));
        assert!(!g.flat_ignores(&s(&["dist"]), "pkg/dist"));
        assert!(g.flat_ignores(&s(&["**/dist/**"]), "pkg/dist"));
        assert!(g.flat_ignores(&s(&["**/dist"]), "pkg/dist"));
        assert!(g.flat_ignores(&s(&["pkg/"]), "pkg/dist"));
        assert!(g.flat_ignores(&s(&["pkg/*/dist/"]), "pkg/a/dist"));
        assert!(!g.flat_ignores(&s(&["*.snap"]), "dist"));
        assert!(g.flat_ignores(&s(&["!keep"]), "dist"));
    }

    #[test]
    fn gitignore_patterns_match_anywhere() {
        let g = Globs::default();
        let dir = Path::new("/r");
        assert!(g.gitignore_style_ignores(dir, &s(&["dist"]), Path::new("/r/pkg/dist")));
        assert!(g.gitignore_style_ignores(dir, &s(&["pkg/dist/**"]), Path::new("/r/pkg/dist")));
        assert!(!g.gitignore_style_ignores(dir, &s(&["/dist"]), Path::new("/r/pkg/dist")));
        assert!(!g.gitignore_style_ignores(
            dir,
            &s(&["dist", "!pkg/dist"]),
            Path::new("/r/pkg/dist")
        ));
    }

    #[test]
    fn matches_files_patterns() {
        let g = Globs::default();
        assert!(g.flat_matches(&s(&["**/*.js"]), "a/b.js"));
        assert!(!g.flat_matches(&s(&["*.js"]), "a/b.js"));
        assert!(g.flat_matches(&s(&["**/*.{js,mjs}"]), "b.mjs"));
        assert!(g.eslintrc_matches(&s(&["*.js"]), "a/b.js"));
        assert!(!g.eslintrc_matches(&s(&["src/*.js"]), "a/b.js"));
    }
}
