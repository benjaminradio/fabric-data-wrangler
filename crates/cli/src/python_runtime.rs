//! Embeds the Python interpreter and runs a user transformation script that
//! looks exactly like a Fabric notebook cell:
//!
//! ```python
//! df = pd.DataFrame(data)
//! ...
//! display(df)
//! ```
//!
//! `data` and `display` are injected as globals (mirroring how a Fabric
//! notebook already has `data`-producing cells and a built-in `display()`
//! before your cell runs), and `import pandas as pd` resolves to our
//! pure-Python drop-in (`crates/cli/src/python/pandas/__init__.py`)
//! installed into `sys.modules["pandas"]`. In a real Fabric notebook, real
//! pandas is already installed, so that same `import pandas as pd` line
//! resolves to the genuine library instead -- the transformation code does
//! not change at all; only how `data` gets populated differs.
//!
//! **No disk writes for embedding Python.** The `pandas` drop-in's source
//! is compiled into the binary at build time via `include_str!` and
//! installed into `sys.modules` directly from that in-memory string -- it
//! is never extracted to a temp directory or shipped as a loose file beside
//! the binary. The user's transformation script is read from disk (a
//! legitimate input file, not a packaged resource) and executed directly
//! from the in-memory string via `Python::run_bound` -- again, no
//! intermediate file.
//!
//! What this does NOT yet do (see README "Spike status"): the interpreter
//! itself is linked against the *system* libpython (via pyo3's
//! `auto-initialize`, which shells out to `python3-config`), not a
//! statically-linked `python-build-standalone` distribution with its
//! stdlib loaded through an `oxidized-importer`-style in-memory finder.
//! That swap is the next step and needs network access to fetch a
//! `python-build-standalone` release, which this development environment
//! did not have. The in-memory module loading here is exactly the
//! mechanism that continues to work once that swap happens -- only the
//! interpreter's own bootstrap changes, not this code.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use dw_ingest::{OutputFormat, Record};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict, PyTuple};

const PANDAS_DROPIN_PY: &str = include_str!("python/pandas/__init__.py");

/// Where `display(df)` calls should write output. Every `display()` call
/// writes immediately; writing to a file overwrites it each time (so with
/// multiple calls, the last one wins), and writing to stdout prints each
/// call's table in turn.
#[derive(Debug, Clone)]
pub enum OutputTarget {
    Stdout(OutputFormat),
    File(PathBuf, OutputFormat),
}

fn install_pandas_dropin(py: Python<'_>) -> PyResult<()> {
    let sys_modules = py.import_bound("sys")?.getattr("modules")?;
    let types = py.import_bound("types")?;
    let module = types.call_method1("ModuleType", ("pandas",))?;

    let locals = PyDict::new_bound(py);
    locals.set_item("src", PANDAS_DROPIN_PY)?;
    locals.set_item("name", "pandas")?;
    let compiled = py.eval_bound("compile(src, name, 'exec')", None, Some(&locals))?;

    let exec_locals = PyDict::new_bound(py);
    exec_locals.set_item("compiled", &compiled)?;
    exec_locals.set_item("module", &module)?;
    py.eval_bound("exec(compiled, module.__dict__)", None, Some(&exec_locals))?;

    sys_modules.set_item("pandas", &module)?;
    Ok(())
}

fn write_target(records: &[Record], target: &OutputTarget) -> Result<()> {
    match target {
        OutputTarget::Stdout(format) => {
            let stdout = std::io::stdout();
            dw_ingest::write_records(records, *format, stdout.lock())?;
        }
        OutputTarget::File(path, OutputFormat::Xlsx) => {
            dw_ingest::write_xlsx(records, path)?;
        }
        OutputTarget::File(path, format) => {
            let file = std::fs::File::create(path)
                .with_context(|| format!("creating output file {}", path.display()))?;
            dw_ingest::write_records(records, *format, file)?;
        }
    }
    Ok(())
}

/// Build the native `display()` callable that a transformation script calls
/// with a DataFrame (or anything with a pandas-shaped `to_dict(orient=...)`
/// method, or already a plain `list[dict]`).
fn make_display_fn<'py>(
    py: Python<'py>,
    target: OutputTarget,
) -> PyResult<Bound<'py, PyCFunction>> {
    PyCFunction::new_closure_bound(
        py,
        Some(c"display"),
        Some(c"display(df): write df to the CLI's configured output (CSV/xlsx/ndjson/table, stdout or --output)."),
        move |args: &Bound<'_, PyTuple>, _kwargs| -> PyResult<()> {
            let df_obj = args.get_item(0)?;
            let records_obj = if df_obj.hasattr("to_dict")? {
                df_obj.call_method1("to_dict", ("records",))?
            } else {
                df_obj
            };
            let records: Vec<Record> = pythonize::depythonize(&records_obj).map_err(|e| {
                PyRuntimeError::new_err(format!("display(): couldn't read records: {e}"))
            })?;
            write_target(&records, &target)
                .map_err(|e| PyRuntimeError::new_err(format!("display(): {e}")))
        },
    )
}

/// Read `records` in as the `data` global, then run the user's
/// transformation script as top-level code (like a notebook cell): it
/// builds a DataFrame from `data`, transforms it, and calls `display(df)`
/// zero or more times to produce output.
pub fn run_script(
    script_path: &Path,
    records: Vec<Record>,
    output: OutputTarget,
) -> Result<()> {
    let source = std::fs::read_to_string(script_path)
        .with_context(|| format!("reading transform script {}", script_path.display()))?;

    Python::with_gil(|py| -> Result<()> {
        install_pandas_dropin(py).map_err(|e| anyhow!("installing pandas drop-in: {e}"))?;

        let globals = PyDict::new_bound(py);
        globals.set_item("__name__", "__main__")?;

        let py_data = pythonize::pythonize(py, &records)
            .map_err(|e| anyhow!("converting input records to Python: {e}"))?;
        globals.set_item("data", py_data)?;

        let display_fn = make_display_fn(py, output)
            .map_err(|e| anyhow!("building display() function: {e}"))?;
        globals.set_item("display", display_fn)?;

        py.run_bound(&source, Some(&globals), None)
            .map_err(|e| anyhow!("running {}: {e}", script_path.display()))?;

        Ok(())
    })
}
