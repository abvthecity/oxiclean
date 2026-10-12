//! Core utilities for oxiclean tools.
//!
//! This crate provides shared functionality for analyzing JavaScript/TypeScript
//! projects, including:
//! - Parsing the imports and exports of JS/TS files
//! - Resolving module paths (relative, tsconfig paths, node_modules, workspace packages)
//! - Collecting entry files from a project
//! - Building the module graph and analysing it ([`ModuleGraph`])
//! - Reading `oxiclean.toml` ([`settings`])
//! - Reporting findings ([`report`])

mod analysis;
mod collector;
mod config;
mod constants;
mod error;
mod exports;
mod graph;
mod jsonc;
mod parser;
mod project;
pub mod report;
mod resolver;
pub mod sequencer;
pub mod settings;
mod types;

// Re-export public API
pub use analysis::{Analysis, CommonArgs};
pub use collector::{CollectorConfig, collect_entries};
pub use config::{find_git_root, find_git_root_from};
pub use constants::{INDEX_FILES, JS_TS_EXTENSIONS, RESOLVE_EXTENSIONS};
pub use error::{Error, Result};
pub use exports::{Export, ExportForm, ExportKind};
pub use graph::{
    BuildOptions, Cycle, Edge, Module, ModuleGraph, ModuleId, ParseFailure, ParsedFile, RawImport,
};
pub use parser::{ParsedImports, imports_for, parse_imports};
pub use project::{Project, TsConfig, split_package_request};
pub use report::{
    CheckResult, CycleFinding, CycleStep, DuplicateExport, ExportName, Finding, FindingKind,
};
pub use resolver::{ResolutionStats, is_builtin, resolve};
pub use sequencer::{Sequence, sequence};
pub use settings::{ConfigFile, RulePolicy, Severity};
pub use types::{SpecKind, Specifier};
