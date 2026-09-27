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
//! **Interpreter linking.** `pyo3`'s build script (`pyo3-build-config`)
//! decides which libpython to link against from `PYO3_CONFIG_FILE` (or
//! `PYO3_PYTHON`, or a `python3` found on `PATH`) at *build* time -- no
//! code here changes between a system-Python dev build and a CI build
//! statically linked against a `python-build-standalone` distribution. See
//! `.github/workflows/build.yml`, which downloads a `python-build-standalone`
//! release, points `PYO3_CONFIG_FILE` at its static `libpython*.a`, and
//! verifies with `ldd` that the resulting binary has no dynamic dependency
//! on libpython. A local `cargo build` without that env var still falls
//! back to the system's libpython via `auto-initialize`, for convenience.
//!
//! **Stdlib location.** Even with libpython statically linked, the pure-Python
//! standard library (`.py`/`.pyc` files) still needs to be found on disk at
//! run time, since this project doesn't yet wire up an `oxidized-importer`-style
//! in-memory finder for it (see README "Spike status" for that remaining
//! step). `configure_python_home` below points `PYTHONHOME` at a
//! `python-runtime/` directory shipped as a sibling of the executable, if
//! one is present -- that's what the CI workflow packages alongside `dw`,
//! so the released bundle needs no system Python install, even though it's
//! a binary-plus-directory bundle rather than a single file today.

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

/// Point Python's home at a shipped runtime, if one is available, before the
/// interpreter initializes. Precedence: an already-set `PYTHONHOME` wins
/// (never override an explicit choice), then `DW_PYTHON_HOME`, then a
/// `python-runtime/` directory sitting next to the current executable (what
/// the CI-built bundle ships). If none of these apply, Python falls back to
/// whatever its own build-time defaults are (the system install, for a
/// locally built dev binary).
///
/// This sets both the `PYTHONHOME` env var *and* calls `Py_SetPythonHome`
/// directly via the C API. The env var alone was not reliable for a
/// statically-linked interpreter in practice (a static/LTO'd libpython can
/// have already resolved its path config by the time `getenv("PYTHONHOME")`
/// would normally be consulted, depending on exactly how/when the runtime's
/// path-configuration step runs) -- `Py_SetPythonHome` is the API CPython's
/// own embedding docs recommend for exactly this reason: it hands the
/// interpreter the prefix directly rather than asking it to rediscover it
/// from the environment.
fn configure_python_home() {
    let home = std::env::var_os("PYTHONHOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("DW_PYTHON_HOME").map(PathBuf::from))
        .or_else(sibling_python_runtime_dir);

    let Some(home) = home else {
        return;
    };

    std::env::set_var("PYTHONHOME", &home);

    if !set_python_home_ffi(&home) {
        eprintln!(
            "warning: found python-runtime at {} but could not pass it to Py_SetPythonHome \
             (non-UTF-8 path?); falling back to the PYTHONHOME environment variable only",
            home.display()
        );
    }
}

/// Call `Py_SetPythonHome(Py_DecodeLocale(path))`, the pattern CPython's own
/// embedding documentation shows for setting the interpreter's home
/// programmatically. The decoded string is intentionally leaked: CPython
/// stores the pointer as-is (it does not copy it) and expects it to remain
/// valid for the life of the interpreter.
fn set_python_home_ffi(home: &Path) -> bool {
    let Some(home_str) = home.to_str() else {
        return false;
    };
    let Ok(home_c) = std::ffi::CString::new(home_str) else {
        return false;
    };
    unsafe {
        let wide = pyo3::ffi::Py_DecodeLocale(home_c.as_ptr(), std::ptr::null_mut());
        if wide.is_null() {
            return false;
        }
        pyo3::ffi::Py_SetPythonHome(wide);
    }
    true
}

fn sibling_python_runtime_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let candidate = exe.parent()?.join("python-runtime");
    candidate.is_dir().then_some(candidate)
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

    configure_python_home();

    // Explicit rather than relying on pyo3's "auto-initialize" feature:
    // when linked against a statically-embeddable python-build-standalone
    // interpreter (see .github/workflows/build.yml), pyo3 refuses to build
    // with "auto-initialize" enabled and requires this manual call instead
    // (static embedding has caveats around interpreter finalization/restart
    // that auto-initialize's implicit behavior doesn't account for). This
    // same call works identically against a dynamically-linked system
    // libpython, so it's used unconditionally rather than gated per build.
    pyo3::prepare_freethreaded_python();

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
