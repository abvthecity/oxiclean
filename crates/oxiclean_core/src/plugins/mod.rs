//! Plugins: what oxiclean knows about a file format or a tool, as knip's
//! plugins do. A plugin can
//!
//! - parse a file format into JS/TS and imports ([`Plugin::extract`]), as
//!   the HTML plugin reads `<script>` tags;
//! - make files entries ([`Plugin::is_entry`], [`Plugin::entries`]), as
//!   HTML pages that load scripts and Jest's setup files are;
//! - name the packages a package's config files use by string
//!   ([`Plugin::references`]), as tsconfig `extends` does;
//! - name shell commands a tool runs ([`Plugin::scripts`]), as husky's
//!   hooks.
//!
//! A tool plugin is on when a package.json in the project lists one of its
//! [`Plugin::enablers`]; a plugin with none is always on. `plugins` in
//! `.oxicleanrc.json` turns any plugin on or off.

mod babel;
mod components;
mod config;
mod css;
mod eslint;
mod expo;
mod git_hooks;
mod html;
mod jest;
mod markup;
mod mdx;
mod next;
mod postcss;
mod prettier;
mod storybook;
mod typescript;
mod vitest;

use std::{
    collections::HashSet,
    fmt,
    path::{Path, PathBuf},
};

use oxc_span::SourceType;
use serde_json::Value;

use crate::{Project, settings::PluginSettings};

/// What a format plugin makes of a file.
#[derive(Debug)]
pub(crate) struct Extracted {
    /// The file's JS/TS, with everything else blanked to spaces so that
    /// offsets and lines stay those of the file.
    pub js: String,
    pub source_type: SourceType,
    /// Imports outside the JS/TS, as `<script src>`, with the byte offset
    /// of each.
    pub imports: Vec<(String, usize)>,
}

/// A file format or tool oxiclean knows about. See the module docs.
pub(crate) trait Plugin: Sync {
    /// The plugin's key in `.oxicleanrc.json`'s `plugins`.
    fn name(&self) -> &'static str;

    /// Packages that turn the plugin on when any package.json lists one.
    /// None means always on.
    fn enablers(&self) -> &'static [&'static str] {
        &[]
    }

    /// The file extensions [`Self::extract`] reads.
    fn extensions(&self) -> &'static [&'static str] {
        &[]
    }

    /// The JS/TS and imports of `src`, the text of `file`.
    fn extract(&self, _file: &Path, _src: &str) -> Option<Extracted> {
        None
    }

    /// Whether `file`, found anywhere in the project, is an entry.
    fn is_entry(&self, _file: &Path) -> bool {
        false
    }

    /// The packages, as requests such as `@tsconfig/node20/tsconfig.json`,
    /// that the config files in package directory `dir` use.
    fn references(&self, _dir: &Path) -> Vec<String> {
        Vec::new()
    }

    /// The files the config files in package directory `dir` make entries,
    /// as Jest's `setupFiles`.
    fn entries(&self, _dir: &Path) -> Vec<PathBuf> {
        Vec::new()
    }

    /// Shell commands the tool runs in package directory `dir`, read as
    /// package.json scripts are, as husky's hooks.
    fn scripts(&self, _dir: &Path) -> Vec<String> {
        Vec::new()
    }
}

/// Every plugin, in the order they run.
const ALL: &[&dyn Plugin] = &[
    &html::Html,
    &mdx::Mdx,
    &components::Vue,
    &components::Svelte,
    &components::Astro,
    &css::Css,
    &typescript::TypeScript,
    &eslint::Eslint,
    &babel::Babel,
    &postcss::Postcss,
    &prettier::Prettier,
    &jest::Jest,
    &vitest::Vitest,
    &next::Next,
    &expo::Expo,
    &storybook::Storybook,
    &git_hooks::Husky,
    &git_hooks::LintStaged,
];

/// The names of every plugin, for the config schema and error messages.
#[must_use]
pub fn plugin_names() -> Vec<&'static str> {
    ALL.iter().map(|p| p.name()).collect()
}

/// Whether some plugin reads files with extension `ext`.
pub(crate) fn is_plugin_extension(ext: &str) -> bool {
    ALL.iter().any(|p| p.extensions().contains(&ext))
}

/// The plugins that are on for a project.
#[derive(Clone)]
pub struct Plugins {
    enabled: Vec<&'static dyn Plugin>,
}

impl fmt::Debug for Plugins {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.names()).finish()
    }
}

impl Default for Plugins {
    /// Every plugin, as for a file parsed outside a project.
    fn default() -> Self {
        Self { enabled: ALL.to_vec() }
    }
}

impl Plugins {
    /// The plugins `settings` turns on, and those it leaves alone that are
    /// always on or whose enabler a package.json of `project` lists.
    #[must_use]
    pub fn detect(project: &Project, settings: &PluginSettings) -> Self {
        let listed = listed_packages(project);
        let enabled = ALL
            .iter()
            .copied()
            .filter(|p| {
                settings.get(p.name()).unwrap_or_else(|| {
                    p.enablers().is_empty() || p.enablers().iter().any(|e| listed.contains(*e))
                })
            })
            .collect();
        Self { enabled }
    }

    /// The names of the plugins that are on.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        self.enabled.iter().map(|p| p.name()).collect()
    }

    /// The plugin that reads `file`, if it is on.
    pub(crate) fn for_file(&self, file: &Path) -> Option<&'static dyn Plugin> {
        let ext = file.extension()?.to_str()?;
        self.enabled.iter().copied().find(|p| p.extensions().contains(&ext))
    }

    /// Whether some plugin makes `file` an entry.
    #[must_use]
    pub fn is_entry(&self, file: &Path) -> bool {
        self.enabled.iter().any(|p| p.is_entry(file))
    }

    /// The packages the config files in package directory `dir` use, as
    /// requests.
    #[must_use]
    pub fn references(&self, dir: &Path) -> Vec<String> {
        self.enabled.iter().flat_map(|p| p.references(dir)).collect()
    }

    /// The files the config files in package directory `dir` make entries.
    #[must_use]
    pub fn entries(&self, dir: &Path) -> Vec<PathBuf> {
        self.enabled.iter().flat_map(|p| p.entries(dir)).collect()
    }

    /// Shell commands tools run in package directory `dir`.
    #[must_use]
    pub fn scripts(&self, dir: &Path) -> Vec<String> {
        self.enabled.iter().flat_map(|p| p.scripts(dir)).collect()
    }
}

/// Every package any package.json of the root or a workspace package lists.
fn listed_packages(project: &Project) -> HashSet<String> {
    let mut dirs = vec![project.root.as_path()];
    dirs.extend(project.workspace_packages().into_iter().map(|(_, d)| d));
    let mut names = HashSet::new();
    for pkg in dirs.into_iter().filter_map(|d| project.package_json(d)) {
        for field in ["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"]
        {
            if let Some(Value::Object(deps)) = pkg.get(field) {
                names.extend(deps.keys().cloned());
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_known_to_settings() {
        let names = plugin_names();
        let unique: HashSet<_> = names.iter().collect();
        assert_eq!(unique.len(), names.len());
        let all_off = PluginSettings::all(false);
        for name in names {
            assert_eq!(all_off.get(name), Some(false), "{name} has no setting");
        }
    }

    #[test]
    fn settings_turn_plugins_off() {
        let project = Project::new("/nowhere");
        let mut settings = PluginSettings::default();
        assert!(Plugins::detect(&project, &settings).names().contains(&"html"));
        settings.html = Some(false);
        let plugins = Plugins::detect(&project, &settings);
        assert!(!plugins.names().contains(&"html"));
        assert!(plugins.for_file(Path::new("a.html")).is_none());
        assert!(plugins.for_file(Path::new("a.mdx")).is_some());
    }
}
