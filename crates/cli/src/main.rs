mod python_runtime;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use dw_ingest::OutputFormat;
use python_runtime::OutputTarget;

/// Local, offline data-wrangling CLI. Runs a Python transformation script,
/// written like a Fabric notebook cell (`df = pd.DataFrame()`, ...,
/// `display(df)`), against a CSV/XLSX input file -- locally, with no
/// pandas/numpy dependency. Porting to a real Fabric notebook is a small,
/// visible two-line diff: `import littlepandas as pd` becomes `import
/// pandas as pd`, and `pd.DataFrame()` gets a real data source.
#[derive(Parser, Debug)]
#[command(name = "dw", version, about)]
struct Args {
    /// Input data file (.csv or .xlsx)
    #[arg(short, long)]
    input: PathBuf,

    /// Python transformation script. Sees `display` as a global, plus
    /// `import littlepandas as pd` resolving to the pure-Python drop-in,
    /// whose `pd.DataFrame()` (no arguments) reads the ingested records.
    #[arg(short, long)]
    script: PathBuf,

    /// Output path for display() calls, or "-" for stdout (default)
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

    let target = if args.output == "-" {
        let format = match &args.format {
            Some(f) => OutputFormat::parse(f)?,
            None => OutputFormat::Table,
        };
        OutputTarget::Stdout(format)
    } else {
        let path = PathBuf::from(&args.output);
        let format = match &args.format {
            Some(f) => OutputFormat::parse(f)?,
            None => OutputFormat::infer_from_path(&path),
        };
        OutputTarget::File(path, format)
    };

    python_runtime::run_script(&args.script, records, target)
        .with_context(|| format!("running transform script {}", args.script.display()))?;

    Ok(())
}
