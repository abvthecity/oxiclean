use std::{
    io::{BufWriter, Write},
    process::ExitCode,
    time::Instant,
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use log::debug;
use oxiclean_core::{
    CheckResult,
    report::{self, ReportContext, ReportStyle},
};

#[derive(Parser)]
#[command(name = "oxiclean", version)]
#[command(about = "A collection of tools for cleaning up codebases", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Check for import bloat in JavaScript/TypeScript projects
    ImportBloat(oxiclean_import_bloat::Config),
    /// Check for excessive import depth in JavaScript/TypeScript projects
    ImportDepth(oxiclean_import_depth::Config),
}

fn main() -> Result<ExitCode> {
    env_logger::init();
    let cli = Cli::parse();
    debug!("Parsed CLI arguments: {:?}", cli.command);

    let start = Instant::now();
    let (result, style, threshold) = match &cli.command {
        Commands::ImportBloat(cfg) => (
            oxiclean_import_bloat::run_import_bloat_check(cfg)?,
            oxiclean_import_bloat::REPORT_STYLE,
            cfg.threshold,
        ),
        Commands::ImportDepth(cfg) => (
            oxiclean_import_depth::run_import_depth_check(cfg)?,
            oxiclean_import_depth::REPORT_STYLE,
            cfg.threshold,
        ),
    };
    let elapsed_ms = start.elapsed().as_millis();

    let cwd = std::env::current_dir().context("could not read the current directory")?;
    let ctx = ReportContext { root: &result.root, cwd: &cwd };

    // stdout is line-buffered; a BufWriter avoids a syscall per line.
    // See https://github.com/rust-lang/rust/issues/60673
    let mut stdout = BufWriter::new(std::io::stdout().lock());
    print_report(&mut stdout, &result, &style, threshold, elapsed_ms, ctx)?;
    stdout.flush()?;

    // A non-zero exit fails CI when anything crossed the threshold.
    Ok(if result.findings.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

fn print_report(
    out: &mut impl Write,
    result: &CheckResult,
    style: &ReportStyle,
    threshold: usize,
    elapsed_ms: u128,
    ctx: ReportContext<'_>,
) -> std::io::Result<()> {
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
