//! Paths in a trace, found on this machine, and the files read for them.
//!
//! A trace names files by the absolute paths `tsc` saw where it ran, which
//! may be another machine, a CI container, a symlinked checkout or one
//! that has since moved; on case-insensitive systems `tsc` also lowercases
//! them. Each path is looked up:
//!
//! 1. as written, when it exists here: canonicalized when it isn't spelled
//!    under the root (a symlinked checkout, `/var` for `/private/var`);
//! 2. under the root, when it starts with the root's path in any case
//!    (macOS and Windows lowercase paths, `c:/` for `\\?\C:\`);
//! 3. under the root, when it starts with the directory the trace was
//!    recorded in. That is learnt once, before the trace is analysed: from
//!    the tsconfig files the programs were built from, or else from the
//!    files the trace parsed and checked, as the directory under which
//!    the most of them are found here (a weak match when only one file
//!    says so). It is then applied to every path, so
//!    `/ci/other/src/index.ts` isn't taken for the root's own
//!    `src/index.ts`;
//! 4. failing that, by the one tail of at least two components
//!    (`src/a.ts`) that names a file under the root. Such a match is weak:
//!    it places findings but never confirms a `complex-types` finding.
//!
//! Each directory's entries are matched without regard to case when the
//! exact name isn't there.
//!
//! Only regular files under the root, up to [`MAX_SOURCE_BYTES`], are
//! read, so a trace can't make oxiclean read `/etc/passwd` or hang on a
//! FIFO or `/dev/zero`.

use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
};

use tracing::debug;

/// The largest source file read for positions or `import("…")` types.
pub(crate) const MAX_SOURCE_BYTES: u64 = 64 << 20;

/// Finds the files a trace names, remembering each answer.
#[derive(Debug)]
pub(crate) struct PathMap {
    root: PathBuf,
    /// The root's path, lowercased with `/` separators.
    root_key: String,
    /// The directory the trace was recorded in, lowercased with `/`
    /// separators, once learnt, and whether more than one file says so.
    recorded: Option<(String, bool)>,
    /// Each path asked for: where it is, and whether that's a strong
    /// match.
    cache: Mutex<HashMap<String, Option<(PathBuf, bool)>>>,
}

impl PathMap {
    pub(crate) fn new(root: &Path) -> Self {
        // `root` is canonical, as the workspace's is.
        Self {
            root: root.to_path_buf(),
            root_key: key(&root.to_string_lossy()),
            recorded: None,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Learns the directory the trace was recorded in: from `configs`,
    /// the tsconfig files of the programs, by the longest tail of one that
    /// is a file under the root; else from `files`, paths the trace parsed
    /// or checked, as the directory under which the most of them are here.
    /// Nothing is learnt when the paths are this machine's own, or none
    /// is found.
    pub(crate) fn learn<'s>(
        &mut self,
        configs: impl IntoIterator<Item = &'s str>,
        files: impl IntoIterator<Item = &'s str>,
    ) {
        for raw in configs {
            let parts = components(raw);
            if let Some(skip) = (drive(&parts)..parts.len())
                .find(|&skip| under(&self.root, &parts[skip..]).is_some_and(|f| f.is_file()))
            {
                self.set_recorded(&parts[..skip]);
                return;
            }
        }
        // Each candidate directory: how many files it places, and whether
        // one of them by a tail of two components or more.
        let mut votes: HashMap<String, (usize, bool)> = HashMap::new();
        let mut voters = 0;
        for raw in files {
            if Path::new(raw).exists() || self.strip(&self.root_key, raw).is_some() {
                // Recorded on this machine.
                continue;
            }
            voters += 1;
            let parts = components(raw);
            for skip in drive(&parts)..parts.len() {
                if under(&self.root, &parts[skip..]).is_some() {
                    let vote = votes.entry(key(&parts[..skip].join("/"))).or_default();
                    vote.0 += 1;
                    vote.1 |= parts.len() - skip >= 2;
                }
            }
        }
        // The most files, then the longest tails (the shortest directory).
        let best = votes
            .into_iter()
            .filter(|(_, (n, long))| *long || *n >= 2)
            .max_by(|a, b| a.1.0.cmp(&b.1.0).then_with(|| b.0.len().cmp(&a.0.len())));
        if let Some((prefix, (n, _))) = best {
            debug!("The trace was recorded in {prefix:?} ({n} of {voters} files are here)");
            self.recorded = Some((prefix, n >= 2));
        }
    }

    fn set_recorded(&mut self, parts: &[&str]) {
        let prefix = key(&parts.join("/"));
        debug!("The trace was recorded in {prefix:?}");
        self.recorded = Some((prefix, true));
    }

    /// The file or directory `raw`, as a trace writes it, on this machine;
    /// `None` when it can't be found.
    pub(crate) fn local(&self, raw: &str) -> Option<PathBuf> {
        self.lookup(raw).map(|(p, _)| p)
    }

    /// As [`Self::local`], only when that is under the root.
    pub(crate) fn here(&self, raw: &str) -> Option<PathBuf> {
        self.local(raw).filter(|p| p.starts_with(&self.root))
    }

    /// As [`Self::here`], only when found by more than a tail of its path,
    /// so it can confirm what another rule found there.
    pub(crate) fn here_for_sure(&self, raw: &str) -> Option<PathBuf> {
        self.lookup(raw).filter(|(p, strong)| *strong && p.starts_with(&self.root)).map(|(p, _)| p)
    }

    fn lookup(&self, raw: &str) -> Option<(PathBuf, bool)> {
        if let Some(hit) = self.cache.lock().ok().and_then(|c| c.get(raw).cloned()) {
            return hit;
        }
        let found = self.find(raw);
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(raw.to_string(), found.clone());
        }
        found
    }

    fn find(&self, raw: &str) -> Option<(PathBuf, bool)> {
        if raw.is_empty() {
            return None;
        }
        let as_written = Path::new(raw);
        if as_written.is_absolute() && as_written.exists() {
            if as_written.starts_with(&self.root) {
                return Some((true_case(as_written.to_path_buf()), true));
            }
            if let Ok(real) = as_written.canonicalize()
                && real.starts_with(&self.root)
            {
                return Some((real, true));
            }
            // A file elsewhere on this machine, such as TypeScript's lib
            // files, unless it's the root spelled in another case.
            let found = self.strip(&self.root_key, raw).unwrap_or_else(|| as_written.to_path_buf());
            return Some((found, true));
        }
        if let Some(found) = self.strip(&self.root_key, raw) {
            return Some((found, true));
        }
        let parts = components(raw);
        let absolute =
            as_written.is_absolute() || raw.starts_with(['/', '\\']) || drive(&parts) > 0;
        if !absolute {
            return under(&self.root, &parts).map(|p| (p, true));
        }
        if let Some((recorded, strong)) = &self.recorded {
            return self.strip(recorded, raw).map(|p| (p, *strong));
        }
        // Only a tail of two components or more, and only the one.
        let mut hits = (drive(&parts)..parts.len().saturating_sub(1))
            .filter_map(|skip| under(&self.root, &parts[skip..]));
        let found = hits.next()?;
        if hits.next().is_some() {
            debug!("{raw} could be several files here; leaving it out");
            return None;
        }
        Some((found, false))
    }

    /// `raw` under the root, when it starts with the directory `prefix`
    /// (lowercased, with `/` separators) in any case.
    fn strip(&self, prefix: &str, raw: &str) -> Option<PathBuf> {
        let normalized = key(raw);
        let rest = normalized.strip_prefix(prefix)?;
        if !(rest.is_empty() || rest.starts_with('/')) {
            return None;
        }
        // The same components of `raw` in their own case.
        let parts = components(raw);
        let skip = parts.len().checked_sub(components(rest).len())?;
        under(&self.root, &parts[skip..])
    }

    /// The text of `path`, a source file under the root, without a byte
    /// order mark (`tsc` leaves it out of its offsets). `None` when it
    /// isn't a regular file under the root, is larger than
    /// [`MAX_SOURCE_BYTES`] or can't be read.
    pub(crate) fn read_source(&self, path: &Path) -> Option<String> {
        let real = path.canonicalize().ok()?;
        if !real.starts_with(&self.root) {
            debug!("Not reading {}: it isn't under the root", path.display());
            return None;
        }
        let text = read_capped(&real)?;
        Some(match text.strip_prefix('\u{feff}') {
            Some(rest) => rest.to_string(),
            None => text,
        })
    }
}

/// The text of the regular file `path`, when it's at most
/// [`MAX_SOURCE_BYTES`] long.
pub(crate) fn read_capped(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_SOURCE_BYTES {
        debug!("Not reading {}: not a regular file of at most 64 MiB", path.display());
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    fs::File::open(path).ok()?.take(MAX_SOURCE_BYTES).read_to_end(&mut bytes).ok()?;
    Some(match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    })
}

/// `raw`'s components, with either separator, without empty ones and `.`,
/// and without the `\\?\` of a Windows verbatim path.
fn components(raw: &str) -> Vec<&str> {
    raw.split(['/', '\\']).filter(|p| !p.is_empty() && *p != "." && *p != "?").collect()
}

/// 1 when `parts` starts with a Windows drive (`C:`), else 0.
fn drive(parts: &[&str]) -> usize {
    parts.first().is_some_and(|p| p.ends_with(':')).into()
}

/// `raw`'s components lowercased and joined with `/`, without a leading
/// or trailing one, as paths are compared regardless of case and
/// platform.
fn key(raw: &str) -> String {
    components(raw).join("/").to_lowercase()
}

/// `parts` joined under `dir`, each found exactly or else without regard
/// to case.
fn under(dir: &Path, parts: &[&str]) -> Option<PathBuf> {
    if parts.contains(&"..") {
        return None;
    }
    let exact = parts.iter().fold(dir.to_path_buf(), |p, part| p.join(part));
    if exact.exists() {
        return Some(true_case(exact));
    }
    let mut current = dir.to_path_buf();
    for part in parts {
        let next = current.join(part);
        current = if next.exists() {
            next
        } else {
            let wanted = part.to_lowercase();
            fs::read_dir(&current)
                .ok()?
                .flatten()
                .find(|e| e.file_name().to_string_lossy().to_lowercase() == wanted)?
                .path()
        };
    }
    Some(true_case(current))
}

/// `path` as spelled on disk, when it exists in another case: on a
/// case-insensitive file system (macOS, Windows) a lowercased path exists
/// as written. Symlinks are left as they are.
fn true_case(path: PathBuf) -> PathBuf {
    match path.canonicalize() {
        Ok(real)
            if real != path
                && real.to_string_lossy().to_lowercase()
                    == path.to_string_lossy().to_lowercase() =>
        {
            real
        }
        _ => path,
    }
}

/// `raw` with `/` separators, as analyze-trace's patterns expect.
pub(crate) fn slashes(raw: &str) -> std::borrow::Cow<'_, str> {
    if raw.contains('\\') { raw.replace('\\', "/").into() } else { raw.into() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn repo() -> (TempDir, PathBuf, PathBuf) {
        let t = TempDir::new().unwrap();
        let root = t.path().canonicalize().unwrap().join("repo");
        fs::create_dir_all(root.join("packages/ui/src")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("packages/ui/src/Button.tsx"), "").unwrap();
        fs::write(root.join("src/index.ts"), "").unwrap();
        fs::write(root.join("index.ts"), "").unwrap();
        fs::write(root.join("tsconfig.json"), "{}").unwrap();
        let file = root.join("packages/ui/src/Button.tsx");
        (t, root, file)
    }

    #[test]
    fn finds_files_recorded_elsewhere() {
        let (_t, root, file) = repo();
        let mut map = PathMap::new(&root);
        map.learn(
            [],
            [
                "/ci/work/repo/packages/ui/src/Button.tsx",
                "/ci/work/repo/src/index.ts",
                "/typescript/lib/lib.d.ts",
            ],
        );
        let here = file.to_string_lossy().to_string();
        assert_eq!(map.local(&here), Some(file.clone()));
        assert_eq!(map.local("packages/ui/src/Button.tsx"), Some(file.clone()));
        assert_eq!(map.local("/elsewhere/missing.ts"), None);
        assert_eq!(map.local(""), None);
        assert_eq!(map.local("/x/../../etc/passwd"), None);
        assert_eq!(map.here_for_sure("/ci/work/repo/packages/ui/src/Button.tsx"), Some(file));
        assert_eq!(map.local("/ci/work/repo/index.ts"), Some(root.join("index.ts")));
        // Not a file of the root, though its tail is.
        assert_eq!(map.local("/ci/work/other/src/index.ts"), None);
    }

    #[test]
    fn windows_paths_lowercased() {
        let (_t, root, file) = repo();
        let mut map = PathMap::new(&root);
        map.learn([], ["c:/users/me/repo/packages/ui/src/button.tsx"]);
        assert_eq!(map.local("c:/users/me/repo/packages/ui/src/button.tsx"), Some(file.clone()));
        assert_eq!(map.local("C:\\Users\\me\\repo\\packages\\ui\\src\\Button.tsx"), Some(file));
        assert_eq!(map.local("C:/Users/me/repo/index.ts"), Some(root.join("index.ts")));
    }

    #[test]
    fn without_a_recorded_directory_only_one_long_tail_will_do() {
        let (_t, root, file) = repo();
        let map = PathMap::new(&root);
        assert_eq!(map.local("/ci/other/index.ts"), None);
        // `packages/ui/src/Button.tsx` and nothing shorter: placed, weakly.
        assert_eq!(map.local("/ci/x/packages/ui/src/Button.tsx"), Some(file));
        assert_eq!(map.here_for_sure("/ci/x/packages/ui/src/Button.tsx"), None);
    }

    #[test]
    fn a_legend_tsconfig_tells_where_the_trace_was_recorded() {
        let (_t, root, _) = repo();
        let mut map = PathMap::new(&root);
        map.learn(["/ci/work/repo/tsconfig.json"], ["/ci/other/src/index.ts"]);
        assert_eq!(map.here_for_sure("/ci/work/repo/index.ts"), Some(root.join("index.ts")));
        assert_eq!(map.local("/ci/other/src/index.ts"), None);
        // One file of another checkout: placed, but weakly.
        let mut map = PathMap::new(&root);
        map.learn([], ["/ci/other/src/index.ts"]);
        assert_eq!(map.local("/ci/other/src/index.ts"), Some(root.join("src/index.ts")));
        assert_eq!(map.here_for_sure("/ci/other/src/index.ts"), None);
    }

    #[test]
    fn the_root_in_another_case_or_through_a_symlink() {
        let (t, root, file) = repo();
        let map = PathMap::new(&root);
        let upper = file.to_string_lossy().to_uppercase();
        assert_eq!(map.local(&upper), Some(file.clone()));
        #[cfg(unix)]
        {
            let link = t.path().join("link");
            std::os::unix::fs::symlink(&root, &link).unwrap();
            let through = link.join("packages/ui/src/Button.tsx");
            assert_eq!(map.here_for_sure(&through.to_string_lossy()), Some(file));
        }
        let _ = t;
    }

    #[test]
    fn reads_only_regular_files_under_the_root() {
        let (t, root, file) = repo();
        let map = PathMap::new(&root);
        fs::write(&file, "\u{feff}const a = 1;").unwrap();
        assert_eq!(map.read_source(&file).as_deref(), Some("const a = 1;"));
        let outside = t.path().join("secret.txt");
        fs::write(&outside, "secret").unwrap();
        assert_eq!(map.read_source(&outside), None);
        assert_eq!(map.read_source(&root.join("packages")), None);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, root.join("link.ts")).unwrap();
            assert_eq!(map.read_source(&root.join("link.ts")), None);
            assert_eq!(read_capped(Path::new("/dev/zero")), None);
        }
    }
}
