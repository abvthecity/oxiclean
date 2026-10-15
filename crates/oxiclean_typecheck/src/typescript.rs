//! Which TypeScript a directory uses.

use std::{fs, path::Path};

use oxiclean_core::Project;
use serde_json::Value;

/// The package.json fields that declare dependencies.
pub(crate) const DEPENDENCY_FIELDS: &[&str] =
    &["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"];

/// The (major, minor) version of the TypeScript `dir` uses: the one
/// installed nearest, else the lowest one a package.json from `dir` up
/// asks for.
pub(crate) fn typescript_version(project: &Project, dir: &Path) -> Option<(u64, u64)> {
    let dirs: Vec<&Path> = dir.ancestors().take_while(|a| a.starts_with(&project.root)).collect();
    let installed = dirs.iter().find_map(|d| {
        let text = fs::read_to_string(d.join("node_modules/typescript/package.json")).ok()?;
        let pkg: Value = serde_json::from_str(&text).ok()?;
        pkg.get("version")?.as_str().map(str::to_string)
    });
    let wanted = || {
        dirs.iter().find_map(|d| {
            let pkg = project.package_json(d)?;
            DEPENDENCY_FIELDS
                .iter()
                .find_map(|f| pkg.get(f)?.get("typescript")?.as_str().map(str::to_string))
        })
    };
    parse_version(&installed.or_else(wanted)?)
}

/// The major and minor of a version or range such as `^5.4.2` or `~6.0`;
/// a missing minor reads as 0.
fn parse_version(text: &str) -> Option<(u64, u64)> {
    let start = text.find(|c: char| c.is_ascii_digit())?;
    let mut parts = text[start..].split('.').map(|p| {
        p.chars().take_while(char::is_ascii_digit).collect::<String>().parse::<u64>().ok()
    });
    let major = parts.next()??;
    let minor = parts.next().flatten().unwrap_or(0);
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_versions_and_ranges() {
        assert_eq!(parse_version("5.4.5"), Some((5, 4)));
        assert_eq!(parse_version("^5.9.0"), Some((5, 9)));
        assert_eq!(parse_version(">=6"), Some((6, 0)));
        assert_eq!(parse_version("latest"), None);
    }
}
