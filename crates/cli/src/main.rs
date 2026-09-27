mod python_runtime;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use dw_ingest::OutputFormat;

/// Local, offline data-wrangling CLI. Runs a pure-Python transformation
/// script (written against the `wrangle` drop-in library) against a CSV/XLSX
/// input file, entirely locally, with no pandas/numpy dependency.
#[derive(Parser, Debug)]
#[command(name = "dw", version, about)]
struct Args {
    /// Input data file (.csv or .xlsx)
    #[arg(short, long)]
    input: PathBuf,

    /// Python transformation script defining a top-level `transform(records)` function
    #[arg(short, long)]
    script: PathBuf,

    /// Output path, or "-" for stdout (default)
    #[arg(short, long, default_value = "-")]
    output: String,

    /// Output format: csv, xlsx, ndjson, table. Inferred from --output's
    /// extension when writing to a file; defaults to "table" on stdout.
    #[arg(short, long)]
    format: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let records = dw_ingest::read_records(&args.input)
        .with_context(|| format!("reading input {}", args.input.display()))?;

    let transformed = python_runtime::run_transform(&args.script, records)
        .with_context(|| format!("running transform script {}", args.script.display()))?;

    let format = match &args.format {
        Some(f) => OutputFormat::parse(f)?,
        None if args.output == "-" => OutputFormat::Table,
        None => OutputFormat::infer_from_path(&PathBuf::from(&args.output)),
    };

    if args.output == "-" {
        let stdout = std::io::stdout();
        dw_ingest::write_records(&transformed, format, stdout.lock())?;
    } else {
        let out_path = PathBuf::from(&args.output);
        if format == OutputFormat::Xlsx {
            dw_ingest::write_xlsx(&transformed, &out_path)?;
        } else {
            let file = std::fs::File::create(&out_path)
                .with_context(|| format!("creating output file {}", out_path.display()))?;
            dw_ingest::write_records(&transformed, format, file)?;
        }
    }

    Ok(())
}
