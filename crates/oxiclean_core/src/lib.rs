//! Core utilities for oxiclean tools.
//!
//! This crate provides shared functionality for analyzing JavaScript/TypeScript
//! projects, including:
//! - Parsing import statements from JS/TS files
//! - Resolving module paths (relative, node_modules, tsconfig paths)
//! - Collecting entry files from a project
//! - Configuration utilities (git root finding, tsconfig reading)

mod collector;
mod config;
mod constants;
mod parser;
mod resolver;
mod types;

// Re-export public API
pub use collector::{CollectorConfig, collect_entries};
pub use config::{find_git_root, read_tsconfig_paths};
pub use constants::{INDEX_FILES, JS_TS_EXTENSIONS, RESOLVE_EXTENSIONS};
pub use parser::imports_for;
pub use resolver::resolve;
pub use types::{SpecKind, Specifier};
