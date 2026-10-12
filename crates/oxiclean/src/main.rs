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
    #[command(name = "import-bloat")]
    Bloat(oxiclean_import_bloat::Config),
    /// Check for excessive import depth in JavaScript/TypeScript projects
    #[command(name = "import-depth")]
    Depth(oxiclean_import_depth::Config),
    /// Check for circular imports in JavaScript/TypeScript projects
    #[command(name = "import-cycles")]
    Cycles(oxiclean_import_cycles::Config),
}

fn main() -> Result<ExitCode> {
    init_tracing();
    let cli = Cli::parse();
    debug!("Parsed CLI arguments: {:?}", cli.command);

    let start = Instant::now();
    let (result, style, rule) = match &cli.command {
        Commands::Bloat(cfg) => (
            oxiclean_import_bloat::run_import_bloat_check(cfg)?,
            oxiclean_import_bloat::REPORT_STYLE,
            oxiclean_import_bloat::RULE,
        ),
        Commands::Depth(cfg) => (
            oxiclean_import_depth::run_import_depth_check(cfg)?,
            oxiclean_import_depth::REPORT_STYLE,
            oxiclean_import_depth::RULE,
        ),
        Commands::Cycles(cfg) => (
            oxiclean_import_cycles::run_import_cycles_check(cfg)?,
            oxiclean_import_cycles::REPORT_STYLE,
            oxiclean_import_cycles::RULE,
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
