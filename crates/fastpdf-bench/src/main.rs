//! fastpdf-bench — GUI-independent benchmark harness (spec §26, §41).
//!
//! ```text
//! fastpdf-bench open    <file.pdf>
//! fastpdf-bench render  <file.pdf> [--page N] [--tile PX] [--workers N] [--out page.png]
//! fastpdf-bench full    <file.pdf>
//! fastpdf-bench corpus  <manifest.json | dir> [--out results.json]
//! fastpdf-bench compare <baseline.json> <candidate.json> [--threshold 10]
//! fastpdf-bench diff    <file.pdf> --engine hayro,zpdf [--out dir]
//! fastpdf-bench diff-corpus <manifest.json | dir> --engine hayro,zpdf
//! fastpdf-bench engines
//! ```
//!
//! `corpus` runs every file in its own child process so peak memory is
//! measured per file and a crash or hang in one file cannot take down the run.

mod args;
mod compare;
mod corpus;
mod diff;
mod engines;
mod measure;
mod metrics;
mod png;
mod report;

use std::process::ExitCode;

use args::{Args, Command};

fn main() -> ExitCode {
    let args = match args::parse(std::env::args_os().skip(1)) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("error: {e}\n\n{}", args::USAGE);
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<ExitCode, String> {
    match &args.command {
        Command::Help => {
            println!("{}", args::USAGE);
            Ok(ExitCode::SUCCESS)
        }
        Command::Engines => {
            for engine in engines::all() {
                let info = engine.info();
                println!(
                    "{:<10} {:<10} {:?}",
                    info.name, info.version, info.capabilities
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Open(file) => {
            let engine = engines::select(args.engine.as_deref())?;
            let report = measure::open(engine.as_ref(), file, args);
            print_json(&report)?;
            Ok(report.exit_code())
        }
        Command::Render(file) => {
            let engine = engines::select(args.engine.as_deref())?;
            let report = measure::render(engine.as_ref(), file, args);
            print_json(&report)?;
            Ok(report.exit_code())
        }
        Command::Full(file) => {
            let engine = engines::select(args.engine.as_deref())?;
            let report = measure::full(engine.as_ref(), file, args);
            print_json(&report)?;
            Ok(report.exit_code())
        }
        Command::Corpus(input) => corpus::run(input, args),
        Command::Compare {
            baseline,
            candidate,
        } => compare::run(baseline, candidate, args),
        Command::Diff(file) => diff::run(file, args),
        Command::DiffCorpus(input) => diff::run_corpus(input, args),
    }
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    println!("{text}");
    Ok(())
}
