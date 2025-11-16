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
                display_path.blue(),
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

            // Use the original import statement, handling newlines by replacing with spaces
            let display_import = warning
                .import_statement
                .replace('\n', " ")
                .replace('\r', "")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");

            writeln!(
                writer,
                "{}  {} ({} modules)",
                prefix.dimmed(),
                display_import,
                warning.reachable_unique_modules.to_string().red()
            )?;
        }

        writeln!(writer)?;
    }

    // Print summary
    print_summary(writer, warnings, cfg)?;

    writer.flush()?;
    Ok(())
}

fn print_summary<W: Write>(writer: &mut W, warnings: &[Warning], cfg: &Config) -> io::Result<()> {
    // Filter out "Entry file" warnings for violation count
    let violations: Vec<_> =
        warnings.iter().filter(|w| !w.import_statement.contains("Entry file")).collect();

    if violations.is_empty() {
        return Ok(());
    }

    let total_violations = violations.len();
    let max_bloat = violations.iter().map(|w| w.reachable_unique_modules).max().unwrap_or(0);

    // Get top 5 offenders (sorted by module count, descending)
    let mut top_offenders: Vec<_> = violations.iter().collect();
    top_offenders.sort_by(|a, b| b.reachable_unique_modules.cmp(&a.reachable_unique_modules));
    top_offenders.truncate(5);

    writeln!(writer, "{}", "─".repeat(60).dimmed())?;
    writeln!(writer, "{}", "Summary".bold())?;
    writeln!(writer, "  Total violations: {}", total_violations.to_string().yellow().bold())?;
    writeln!(writer, "  Maximum bloat: {} modules", max_bloat.to_string().red().bold())?;

    if !top_offenders.is_empty() {
        writeln!(writer, "  Top {} offenders:", top_offenders.len().min(5))?;
        for (idx, warning) in top_offenders.iter().enumerate() {
            let display_import = warning
                .import_statement
                .replace('\n', " ")
                .replace('\r', "")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");

            let file_path = if let Some(root) = &cfg.root {
                relativize_to_cwd(root, &warning.from_file)
            } else {
                warning.from_file.clone()
            };

            writeln!(
                writer,
                "    {}. {} ({} modules) - {}",
                idx + 1,
                display_import,
                warning.reachable_unique_modules.to_string().red(),
                file_path.blue()
            )?;
        }
    }

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
}
