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
pub mod command;
mod config;
mod constants;
mod entry_points;
mod error;
mod exports;
pub mod fix;
mod globs;
mod graph;
mod jsonc;
pub mod jsonc_tree;
mod members;
mod parser;
mod plugins;
mod project;
pub mod report;
mod resolver;
pub mod sequencer;
pub mod settings;
mod types;
mod uses;

// Re-export public API
pub use analysis::{Analysis, CommonArgs, Workspace};
pub use collector::{CollectorConfig, collect_entries};
pub use command::{CommandRule, Rules, rule_names, run_command};
pub use config::{find_git_root, find_git_root_from};
pub use constants::{INDEX_FILES, JS_TS_EXTENSIONS, RESOLVE_EXTENSIONS};
pub use error::{Error, Result};
pub use exports::{Export, ExportForm, ExportKind, Forwarded};
pub use graph::{
    BuildOptions, Cycle, Edge, Module, ModuleGraph, ModuleId, ParseFailure, ParsedFile, RawImport,
};
pub use jsonc::parse_jsonc;
pub use members::ExportMember;
pub use parser::{ParsedImports, imports_for, parse_imports};
pub use plugins::{Plugins, plugin_names};
pub use project::{Project, TsConfig, split_package_request};
pub use report::{
    Barrel, CheckResult, CommandResult, CycleFinding, CycleStep, DuplicateExport,
    DuplicateVersions, ExportName, Finding, FindingKind, HeavyDependency, Issue, Related,
    RuleResult, UnusedCatalogEntry, UnusedDependency, UnusedExport,
};
pub use resolver::{ResolutionStats, is_builtin, resolve};
pub use sequencer::{Sequence, sequence};
pub use settings::{
    ConfigFile, DependencyField, DependencyFields, PluginSettings, ResolvedRule, RuleOptions,
    RulePolicy, Severity, TraceSettings,
};
pub use types::{SpecKind, Specifier};
pub use uses::{ModuleUses, Usage, Used};
