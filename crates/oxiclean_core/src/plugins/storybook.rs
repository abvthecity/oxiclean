//! Storybook: the addons and framework its config names are used, and
//! stories and the files in `.storybook` are entries.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{
    Plugin,
    config::{self, is_path},
};

const EXTENSIONS: &[&str] = &["js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts"];

/// The files in `.storybook` Storybook loads.
const CONFIG_FILES: &[&str] = &["main", "preview", "manager"];

pub(super) struct Storybook;

impl Plugin for Storybook {
    fn name(&self) -> &'static str {
        "storybook"
    }

    fn enablers(&self) -> &'static [&'static str] {
        &["storybook", "@storybook/react", "@storybook/vue3", "@storybook/core"]
    }

    /// `addons`, `framework`, `core.builder` and `core.renderer` of
    /// `.storybook/main.*`, each a name or an object with a `name`.
    fn references(&self, dir: &Path) -> Vec<String> {
        let Some(main) = config_files(dir, &["main"]).into_iter().find_map(|f| config::load(&f))
        else {
            return Vec::new();
        };
        let name = |v: &Value| match v {
            Value::String(s) => Some(s.clone()),
            Value::Object(o) => o.get("name")?.as_str().map(str::to_owned),
            _ => None,
        };
        let mut out = Vec::new();
        if let Some(Value::Array(addons)) = main.get("addons") {
            out.extend(addons.iter().filter_map(name));
        }
        let core = main.get("core");
        for v in [
            main.get("framework"),
            core.and_then(|c| c.get("builder")),
            core.and_then(|c| c.get("renderer")),
        ]
        .into_iter()
        .flatten()
        {
            out.extend(name(v));
        }
        out.retain(|r| !is_path(r));
        out
    }

    fn entries(&self, dir: &Path) -> Vec<PathBuf> {
        config_files(dir, CONFIG_FILES)
    }

    /// Story files: `*.stories.*`.
    fn is_entry(&self, file: &Path) -> bool {
        let Some(name) = file.file_name().and_then(|n| n.to_str()) else { return false };
        name.rsplit_once('.').is_some_and(|(stem, ext)| {
            (EXTENSIONS.contains(&ext) || ext == "mdx")
                && (stem.ends_with(".stories") || stem.ends_with(".story"))
        })
    }
}

/// The files in `dir/.storybook` named `stem.<ext>` for each of `stems`.
fn config_files(dir: &Path, stems: &[&str]) -> Vec<PathBuf> {
    let sb = dir.join(".storybook");
    stems
        .iter()
        .flat_map(|stem| {
            EXTENSIONS.iter().map(|ext| sb.join(format!("{stem}.{ext}"))).collect::<Vec<_>>()
        })
        .filter(|p| p.is_file())
        .map(|p| p.canonicalize().unwrap_or(p))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn storybook_config() {
        let t = TempDir::new().unwrap();
        let dir = t.path();
        fs::create_dir(dir.join(".storybook")).unwrap();
        fs::write(
            dir.join(".storybook/main.ts"),
            "import type { StorybookConfig } from '@storybook/react-vite';
            const config: StorybookConfig = {
              stories: ['../src/**/*.stories.tsx'],
              addons: ['@storybook/addon-essentials', { name: '@storybook/addon-a11y', options: {} }],
              framework: { name: '@storybook/react-vite', options: {} },
            };
            export default config;",
        )
        .unwrap();
        fs::write(dir.join(".storybook/preview.tsx"), "").unwrap();
        assert_eq!(
            Storybook.references(dir),
            ["@storybook/addon-essentials", "@storybook/addon-a11y", "@storybook/react-vite"]
        );
        let sb = dir.canonicalize().unwrap().join(".storybook");
        assert_eq!(Storybook.entries(dir), [sb.join("main.ts"), sb.join("preview.tsx")]);
        assert!(Storybook.is_entry(Path::new("src/Button.stories.tsx")));
        assert!(Storybook.is_entry(Path::new("src/Intro.stories.mdx")));
        assert!(!Storybook.is_entry(Path::new("src/Button.tsx")));
    }
}
