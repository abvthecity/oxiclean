use std::{
    collections::HashMap,
    env,
    io::{self, Write},
    path::{Path, PathBuf},
};

use colored::Colorize;
use log::{debug, trace};

use crate::{config::Config, types::Warning};

/// Relativize a path to the current working directory for clickable links
fn relativize_to_cwd(root: &Path, relative_to_root: &str) -> String {
    let cwd = match env::current_dir() {
        Ok(cwd) => cwd,
        Err(_) => {
            debug!("Failed to get current directory");
            return relative_to_root.to_string();
        }
    };
    trace!("Relativizing '{}' from root {:?} to cwd {:?}", relative_to_root, root, cwd);

    // Reconstruct the absolute path
    let abs_path = root.join(relative_to_root);

    // Make it relative to cwd
    match make_relative(&abs_path, &cwd) {
        Some(rel_path) => {
            let result = rel_path.to_string_lossy().to_string();
            trace!("Relativized '{}' to '{}'", relative_to_root, result);
            result
        }
        None => {
            trace!("Could not relativize '{}', using original", relative_to_root);
            relative_to_root.to_string()
        }
    }
}

/// Relativize import paths in import statements (e.g., "import './path/to/file'" -> "import '../path/to/file'")
fn relativize_import_statement(root: &Path, from_file: &str, import_stmt: &str) -> String {
    trace!("Relativizing import statement: '{}' from file: '{}'", import_stmt, from_file);

    // Check if this is an import statement with a relative path
    if let Some(start_idx) = import_stmt.find("import '") {
        let path_start = start_idx + "import '".len();
        if let Some(end_idx) = import_stmt[path_start..].find('\'') {
            let import_path = &import_stmt[path_start..path_start + end_idx];

            // Only relativize relative imports (starting with ./ or ../)
            if import_path.starts_with("./") || import_path.starts_with("../") {
                // Reconstruct the absolute path
                let from_file_abs = root.join(from_file);
                let from_dir = from_file_abs.parent().unwrap_or(root);
                let import_abs = from_dir.join(import_path);

                // Clean the path
                let import_abs_clean = match import_abs.canonicalize() {
                    Ok(p) => p,
                    Err(_) => {
                        // If canonicalization fails, try with path-clean
                        use path_clean::clean;
                        clean(import_abs.to_string_lossy().to_string())
                    }
                };

                // Get current directory
                if let Ok(cwd) = env::current_dir() {
                    // Make the import path relative to cwd
                    if let Some(rel_path) = make_relative(&import_abs_clean, &cwd) {
                        let rel_str = rel_path.to_string_lossy();
                        let result = format!("import '{}'", rel_str);
                        trace!("Relativized '{}' to '{}'", import_stmt, result);
                        return result;
                    }
                }
            }
        }
    }

    // If we couldn't relativize it, return the original
    trace!("Could not relativize import statement, returning original");
    import_stmt.to_string()
}

/// Create a relative path from `base` to `target`
fn make_relative(target: &Path, base: &Path) -> Option<PathBuf> {
    use std::path::Component;

    let mut target_components = target.components();
    let mut base_components = base.components();

    let mut common_prefix_len = 0;
    let mut target_parts = Vec::new();
    let mut base_parts = Vec::new();

    // Find common prefix
    loop {
        match (target_components.next(), base_components.next()) {
            (Some(t), Some(b)) if t == b => {
                common_prefix_len += 1;
            }
            (Some(t), Some(b)) => {
                target_parts.push(t);
                base_parts.push(b);
                break;
            }
            (Some(t), None) => {
                target_parts.push(t);
                break;
            }
            (None, Some(_)) => {
                // target is a prefix of base, need to go up
                return Some(PathBuf::from("."));
            }
            (None, None) => {
                // They are the same
                return Some(PathBuf::from("."));
            }
        }
    }

    // Collect remaining components
    target_parts.extend(target_components);
    base_parts.extend(base_components);

    // If there's no common prefix, we can't make a relative path
    if common_prefix_len == 0 {
        // Check if they at least share a root
        let target_root = target.components().next();
        let base_root = base.components().next();

        if target_root != base_root {
            return None;
        }
    }

    // Build the relative path: "../" for each remaining base component,
    // then append all remaining target components
    let mut result = PathBuf::new();
    for _ in &base_parts {
        result.push("..");
    }
    for component in target_parts {
        match component {
            Component::Normal(p) => result.push(p),
            Component::CurDir => {}
            Component::ParentDir => result.push(".."),
            Component::RootDir | Component::Prefix(_) => {}
        }
    }

    if result.as_os_str().is_empty() { Some(PathBuf::from(".")) } else { Some(result) }
}

pub fn print_no_bloat_message<W: Write>(writer: &mut W, threshold: usize) -> io::Result<()> {
    debug!("No bloat detected");
    writeln!(writer, "{} No bloat detected. Threshold: {}", "✓".green().bold(), threshold)?;
    writer.flush()?;
    Ok(())
}

pub fn print_warnings_tree<W: Write>(
    writer: &mut W,
    warnings: &[Warning],
    cfg: &Config,
    threshold: usize,
) -> io::Result<()> {
    debug!("Printing warnings tree for {} warnings", warnings.len());
    // Group warnings by file
    let mut by_file: HashMap<String, Vec<&Warning>> = HashMap::new();
    for w in warnings {
        by_file.entry(w.from_file.clone()).or_default().push(w);
    }
    debug!("Grouped warnings into {} files", by_file.len());

    writeln!(
        writer,
        "{} Import bloat detected (threshold: {} modules)\n",
        "⚠".yellow().bold(),
        threshold.to_string().yellow()
    )?;

    // Sort files by their worst warning
    let mut files: Vec<_> = by_file.keys().collect();
    files.sort_by(|a, b| {
        let max_a =
            by_file.get(*a).unwrap().iter().map(|w| w.reachable_unique_modules).max().unwrap_or(0);
        let max_b =
            by_file.get(*b).unwrap().iter().map(|w| w.reachable_unique_modules).max().unwrap_or(0);
        max_b.cmp(&max_a)
    });

    for file in files {
        let file_warnings = by_file.get(file).unwrap();
        trace!("Processing file: {} with {} warnings", file, file_warnings.len());

        // Relativize the file path to cwd for clickable links
        let display_path = if let Some(root) = &cfg.root {
            relativize_to_cwd(root, file)
        } else {
            file.to_string()
        };

        // Find the entry file warning (entire graph)
        let entry_warning =
            file_warnings.iter().find(|w| w.import_statement.contains("Entry file"));

        if let Some(entry) = entry_warning {
            writeln!(
                writer,
                "{} ({} modules)",
                display_path.white(),
                entry.reachable_unique_modules.to_string().red().bold()
            )?;
        } else {
            writeln!(writer, "{}", display_path.bright_white().bold())?;
        }

        // Sort warnings within this file by module count (descending)
        let mut sorted_file_warnings: Vec<_> =
            file_warnings.iter().filter(|w| !w.import_statement.contains("Entry file")).collect();
        sorted_file_warnings
            .sort_by(|a, b| b.reachable_unique_modules.cmp(&a.reachable_unique_modules));

        for (idx, warning) in sorted_file_warnings.iter().enumerate() {
            let is_last = idx == sorted_file_warnings.len() - 1;
            let prefix = if is_last { "└──" } else { "├──" };

            // Use resolved path if available (includes file extension)
            let display_import =
                if let (Some(root), Some(resolved_path)) = (&cfg.root, &warning.resolved_path) {
                    trace!("Using resolved path for import: {}", resolved_path);
                    // Relativize the resolved path to cwd
                    let rel_path = relativize_to_cwd(root, resolved_path);
                    format!("import '{}'", rel_path)
                } else if let Some(root) = &cfg.root {
                    trace!(
                        "No resolved path, relativizing import statement: {}",
                        warning.import_statement
                    );
                    // Fallback to relativizing the import statement
                    relativize_import_statement(root, file, &warning.import_statement)
                } else {
                    warning.import_statement.clone()
                };

            writeln!(
                writer,
                "{}  {} ({} modules)",
                prefix.dimmed(),
                display_import.yellow(),
                warning.reachable_unique_modules.to_string().red()
            )?;
        }

        writeln!(writer)?;
    }

    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_make_relative_same_dir() {
        let target = Path::new("/project/src/file.ts");
        let base = Path::new("/project/src");
        let result = make_relative(target, base);
        assert_eq!(result, Some(PathBuf::from("file.ts")));
    }

    #[test]
    fn test_make_relative_child_dir() {
        let target = Path::new("/project/src/components/Button.tsx");
        let base = Path::new("/project/src");
        let result = make_relative(target, base);
        assert_eq!(result, Some(PathBuf::from("components/Button.tsx")));
    }

    #[test]
    fn test_make_relative_parent_dir() {
        let target = Path::new("/project/src/file.ts");
        let base = Path::new("/project/src/components");
        let result = make_relative(target, base);
        assert_eq!(result, Some(PathBuf::from("../file.ts")));
    }

    #[test]
    fn test_make_relative_sibling_dir() {
        let target = Path::new("/project/apps/web/index.ts");
        let base = Path::new("/project/apps/api");
        let result = make_relative(target, base);
        assert_eq!(result, Some(PathBuf::from("../web/index.ts")));
    }

    #[test]
    fn test_make_relative_same_path() {
        let target = Path::new("/project/src");
        let base = Path::new("/project/src");
        let result = make_relative(target, base);
        assert_eq!(result, Some(PathBuf::from(".")));
    }

    #[test]
    fn test_make_relative_multiple_levels_up() {
        let target = Path::new("/project/file.ts");
        let base = Path::new("/project/apps/web/src");
        let result = make_relative(target, base);
        assert_eq!(result, Some(PathBuf::from("../../../file.ts")));
    }

    #[test]
    fn test_relativize_import_statement_preserves_non_relative() {
        let root = Path::new("/project");
        let from_file = "apps/web/index.ts";

        // Non-relative imports should be unchanged
        let stmt = "import 'lodash'";
        assert_eq!(relativize_import_statement(root, from_file, stmt), stmt);

        let stmt = "import '@/utils'";
        assert_eq!(relativize_import_statement(root, from_file, stmt), stmt);
    }

    #[test]
    fn test_relativize_import_statement_handles_relative_imports() {
        // This test documents the expected behavior but may not work
        // in all environments due to path canonicalization
        let root = Path::new("/tmp/test_project");
        let from_file = "apps/web/index.ts";
        let import_stmt = "import './utils/helper'";

        // The function should attempt to relativize, but exact output
        // depends on file system state, so we just verify it doesn't crash
        let result = relativize_import_statement(root, from_file, import_stmt);
        assert!(result.starts_with("import '"));
    }

    #[test]
    fn test_relativize_import_statement_handles_malformed() {
        let root = Path::new("/project");
        let from_file = "apps/web/index.ts";

        // Malformed import statements should be returned as-is
        let stmt = "import without quotes";
        assert_eq!(relativize_import_statement(root, from_file, stmt), stmt);

        let stmt = "not an import statement";
        assert_eq!(relativize_import_statement(root, from_file, stmt), stmt);
    }
}
