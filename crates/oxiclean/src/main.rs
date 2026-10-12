use std::{
    io::{BufWriter, Write},
    process::ExitCode,
    time::Instant,
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use oxiclean_core::{
    CheckResult,
    report::{self, OutputFormat, ReportContext, ReportMeta, ReportStyle},
};
use tracing::debug;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "oxiclean", version)]
#[command(about = "A collection of tools for cleaning up codebases", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Output format
    #[arg(long, value_enum, default_value_t = OutputFormat::Text, global = true)]
    format: OutputFormat,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Check for import bloat in JavaScript/TypeScript projects
    #[command(name = "bloat")]
    Bloat(oxiclean_bloat::Config),
    /// Check for excessive import depth in JavaScript/TypeScript projects
    #[command(name = "depth")]
    Depth(oxiclean_depth::Config),
    /// Check for circular imports in JavaScript/TypeScript projects
    #[command(name = "cycles")]
    Cycles(oxiclean_cycles::Config),
    /// Check for modules that export the same thing under several names
    #[command(name = "duplicates")]
    Duplicates(oxiclean_duplicates::Config),
    /// Check for exports that no other file imports
    #[command(name = "unused-exports")]
    UnusedExports(oxiclean_unused_exports::Config),
    /// Check for exported types, interfaces and enums that no other file imports
    #[command(name = "unused-types")]
    UnusedTypes(oxiclean_unused_exports::Config),
    /// Check for members of used enums that nothing uses
    #[command(name = "unused-enum-members")]
    UnusedEnumMembers(oxiclean_unused_exports::Config),
    /// Check for what used namespaces export that nothing uses
    #[command(name = "unused-namespace-members")]
    UnusedNamespaceMembers(oxiclean_unused_exports::Config),
    /// Check for packages a package.json lists that nothing imports or runs
    #[command(name = "dependencies")]
    Dependencies(oxiclean_dependencies::Config),
    /// Check for pnpm catalog entries no package.json refers to
    #[command(name = "catalog")]
    Catalog(oxiclean_dependencies::Config),
    /// Check for barrel files whose import pulls in too many modules
    #[command(name = "barrels")]
    Barrels(oxiclean_bloat::Config),
}

fn main() -> Result<ExitCode> {
    init_tracing();
    let cli = Cli::parse();
    debug!("Parsed CLI arguments: {:?}", cli.command);

    let start = Instant::now();
    let (result, style, rule) = match &cli.command {
        Commands::Bloat(cfg) => (
            oxiclean_bloat::run_bloat_check(cfg)?,
            oxiclean_bloat::REPORT_STYLE,
            oxiclean_bloat::RULE,
        ),
        Commands::Depth(cfg) => (
            oxiclean_depth::run_depth_check(cfg)?,
            oxiclean_depth::REPORT_STYLE,
            oxiclean_depth::RULE,
        ),
        Commands::Cycles(cfg) => (
            oxiclean_cycles::run_cycles_check(cfg)?,
            oxiclean_cycles::REPORT_STYLE,
            oxiclean_cycles::RULE,
        ),
        Commands::Duplicates(cfg) => (
            oxiclean_duplicates::run_duplicates_check(cfg)?,
            oxiclean_duplicates::REPORT_STYLE,
            oxiclean_duplicates::RULE,
        ),
        Commands::UnusedExports(cfg) => (
            oxiclean_unused_exports::run_unused_exports_check(cfg)?,
            oxiclean_unused_exports::UNUSED_EXPORTS_STYLE,
            oxiclean_unused_exports::UNUSED_EXPORTS_RULE,
        ),
        Commands::UnusedTypes(cfg) => (
            oxiclean_unused_exports::run_unused_types_check(cfg)?,
            oxiclean_unused_exports::UNUSED_TYPES_STYLE,
            oxiclean_unused_exports::UNUSED_TYPES_RULE,
        ),
        Commands::UnusedEnumMembers(cfg) => (
            oxiclean_unused_exports::run_unused_enum_members_check(cfg)?,
            oxiclean_unused_exports::UNUSED_ENUM_MEMBERS_STYLE,
            oxiclean_unused_exports::UNUSED_ENUM_MEMBERS_RULE,
        ),
        Commands::UnusedNamespaceMembers(cfg) => (
            oxiclean_unused_exports::run_unused_namespace_members_check(cfg)?,
            oxiclean_unused_exports::UNUSED_NAMESPACE_MEMBERS_STYLE,
            oxiclean_unused_exports::UNUSED_NAMESPACE_MEMBERS_RULE,
        ),
        Commands::Dependencies(cfg) => (
            oxiclean_dependencies::run_dependencies_check(cfg)?,
            oxiclean_dependencies::DEPENDENCIES_STYLE,
            oxiclean_dependencies::DEPENDENCIES_RULE,
        ),
        Commands::Catalog(cfg) => (
            oxiclean_dependencies::run_catalog_check(cfg)?,
            oxiclean_dependencies::CATALOG_STYLE,
            oxiclean_dependencies::CATALOG_RULE,
        ),
        Commands::Barrels(cfg) => (
            oxiclean_bloat::run_barrels_check(cfg)?,
            oxiclean_bloat::BARRELS_STYLE,
            oxiclean_bloat::BARRELS_RULE,
        ),
    };
    let elapsed_ms = start.elapsed().as_millis();

    let cwd = std::env::current_dir().context("could not read the current directory")?;
    let ctx = ReportContext { root: &result.root, cwd: &cwd };

    // stdout is line-buffered; a BufWriter avoids a syscall per line.
    // See https://github.com/rust-lang/rust/issues/60673
    let mut stdout = BufWriter::new(std::io::stdout().lock());
    let meta = ReportMeta { rule, style: &style, version: env!("CARGO_PKG_VERSION"), elapsed_ms };
    match cli.format {
        OutputFormat::Text => print_report(&mut stdout, &result, &style, elapsed_ms, ctx)?,
        OutputFormat::Json => report::write_json(&mut stdout, meta, &result)?,
        OutputFormat::Sarif => report::write_sarif(&mut stdout, meta, &result)?,
    }
    stdout.flush()?;

    // A non-zero exit fails CI when a finding with error severity crossed its
    // threshold; warnings alone don't.
    Ok(if result.error_count() == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

/// Sends diagnostics to stderr so stdout carries only the report. `RUST_LOG`
/// takes the usual filter syntax and defaults to errors only.
fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(tracing::Level::ERROR.into())
                .from_env_lossy(),
        )
        .with_writer(std::io::stderr)
        .init();
}

fn print_report(
    out: &mut impl Write,
    result: &CheckResult,
    style: &ReportStyle,
    elapsed_ms: u128,
    ctx: ReportContext<'_>,
) -> std::io::Result<()> {
    let threshold = result.threshold;
    if result.findings.is_empty() {
        report::print_ok(out, style, threshold)?;
    } else {
        report::print_findings(out, &result.findings, style, threshold, ctx)?;
    }
    report::print_finished(out, result, elapsed_ms, rayon::current_num_threads(), ctx)?;

    let resolution = &result.resolution;
    writeln!(
        out,
        "{} Resolved {} of {} imports ({}%), {} built-ins skipped.",
        "●".bright_blue(),
        resolution.resolved.to_string().cyan(),
        (resolution.resolved + resolution.unresolved).to_string().cyan(),
        format!("{:.1}", resolution.resolved_percent()).cyan(),
        resolution.builtin.to_string().cyan()
    )
}
