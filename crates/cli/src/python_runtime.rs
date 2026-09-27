//! Embeds the Python interpreter and runs a user transformation script that
//! looks like a Fabric notebook cell:
//!
//! ```python
//! import littlepandas as pd
//!
//! df = pd.read_input()
//! ...
//! display(df)
//! ```
//!
//! `display` is injected as a global (mirroring Fabric's own notebook
//! built-in), and `import littlepandas as pd` resolves to our pure-Python
//! drop-in (`crates/cli/src/python/littlepandas/__init__.py`), installed
//! into `sys.modules["littlepandas"]`. Unlike an earlier version of this
//! project, that module is deliberately *not* named or installed as
//! `pandas` -- it isn't pandas, and hiding that behind an identical import
//! would make a script's actual data source invisible. Porting to Fabric
//! is a small, visible two-line diff instead: the import name, and
//! `pd.read_input()` (which reads `--input` locally -- see
//! `_INGESTED_DATA` below and the module's own docstring) becoming
//! whatever real ingestion call fits Fabric. `pd.DataFrame(...)` itself
//! behaves like real pandas (empty with no arguments, accepts a list of
//! row dicts or a dict of columns) -- it's `read_input()`, not
//! `DataFrame`, that only exists locally.
//!
//! **No disk writes for embedding Python.** The `littlepandas` drop-in's
//! source is compiled into the binary at build time via `include_str!` and
//! installed into `sys.modules` directly from that in-memory string -- it
//! is never extracted to a temp directory or shipped as a loose file beside
//! the binary. The user's transformation script is read from disk (a
//! legitimate input file, not a packaged resource) and executed directly
//! from the in-memory string via `Python::run_bound` -- again, no
//! intermediate file.
//!
//! **Interpreter linking.** `pyo3`'s build script (`pyo3-build-config`)
//! decides which Python library to link against from `PYO3_CONFIG_FILE` (or
//! `PYO3_PYTHON`, or a `python3` found on `PATH`) at *build* time -- no
//! code here changes between a system-Python dev build and a CI build
//! linked against a `python-build-standalone` distribution.
//! `.github/workflows/build.yml` builds `dw` on both Linux and Windows
//! dynamically linked against that distribution's Python shared
//! library/DLL, and ships it alongside the binary as one extra file --
//! this was originally attempted as a fully static build on Linux, but
//! since Windows has no equivalent static option in python-build-standalone
//! (only a DLL), unifying both platforms around "binary + one shared
//! library" removed a whole static-linking-specific subsystem (discovering
//! and linking the several extra native libs that statically-compiled
//! stdlib extensions need) for a real simplification, not just consistency
//! for its own sake.
//!
//! **Stdlib, frozen into the binary regardless.** Dynamic linking of the
//! Python library itself doesn't affect this: the pure-Python standard
//! library (`.py` sources) still has to come from *somewhere* at run time,
//! and shipping those files on disk beside the binary is exactly the
//! per-platform companion-directory problem this project set out to avoid.
//! `.github/workflows/build.yml` runs `scripts/freeze_stdlib.py` (with the
//! *same* interpreter whose library is linked in, so bytecode compatibility
//! is guaranteed) to compile every stdlib module to marshalled bytecode,
//! and `build.rs` embeds the result into the binary via `include_bytes!`.
//! `install_frozen_stdlib` below registers those modules as CPython "frozen
//! modules" -- the same mechanism CPython itself uses to embed
//! `importlib._bootstrap` -- by prepending them to `PyImport_FrozenModules`
//! before the interpreter initializes. `FrozenImporter` is always the first
//! entry on `sys.meta_path`, so every one of these imports is satisfied
//! before CPython (or a transformation script) ever consults the
//! filesystem. A
//! local dev build (system libpython, no frozen stdlib generated) leaves
//! `FROZEN_STDLIB_ENTRIES` empty and this is a no-op, falling back to
//! the system's normal on-disk stdlib.

use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use dw_ingest::{OutputFormat, Record};
use serde_json::Value;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict, PyTuple};

const LITTLEPANDAS_DROPIN_PY: &str = include_str!("python/littlepandas/__init__.py");

include!(concat!(env!("OUT_DIR"), "/frozen_stdlib_generated.rs"));

/// Register the embedded stdlib bytecode as CPython frozen modules, on top
/// of whatever frozen modules CPython's own build already provides
/// (`importlib._bootstrap`, `zipimport`, ...). Must run before any
/// interpreter initialization -- `PyImport_FrozenModules` is only consulted
/// while the interpreter starts up.
fn install_frozen_stdlib() {
    if FROZEN_STDLIB_ENTRIES.is_empty() {
        return;
    }

    let mut combined: Vec<pyo3::ffi::_frozen> = Vec::new();

    // SAFETY: PyImport_FrozenModules is a plain C global CPython's own
    // startup code (Modules/frozen.c) initializes independently of
    // Py_Initialize, so reading it here (before we've touched the
    // interpreter at all) observes CPython's own default frozen-module
    // table, terminated by a null-name sentinel entry.
    unsafe {
        let mut existing = pyo3::ffi::PyImport_FrozenModules;
        if !existing.is_null() {
            while !(*existing).name.is_null() {
                combined.push(*existing);
                existing = existing.add(1);
            }
        }
    }

    for &(name, offset, size, is_package) in FROZEN_STDLIB_ENTRIES {
        // Intentionally leaked: `_frozen.name` must stay valid for the life
        // of the interpreter, and CPython does not take ownership of it.
        let name_ptr = CString::new(name)
            .expect("frozen module name has no interior NUL")
            .into_raw() as *const c_char;
        combined.push(pyo3::ffi::_frozen {
            name: name_ptr,
            code: FROZEN_STDLIB_BLOB[offset..offset + size].as_ptr(),
            size: size as c_int,
            is_package: is_package as c_int,
            get_code: None,
        });
    }

    combined.push(pyo3::ffi::_frozen {
        name: std::ptr::null(),
        code: std::ptr::null(),
        size: 0,
        is_package: 0,
        get_code: None,
    });

    let leaked: &'static [pyo3::ffi::_frozen] = combined.leak();
    // SAFETY: leaked has 'static lifetime and ends with a null-name
    // sentinel, matching what CPython's frozen importer expects.
    unsafe {
        pyo3::ffi::PyImport_FrozenModules = leaked.as_ptr();
    }
}

/// Where `display(df)` calls should write output. Every `display()` call
/// writes immediately; writing to a file overwrites it each time (so with
/// multiple calls, the last one wins), and writing to stdout prints each
/// call's table in turn.
#[derive(Debug, Clone)]
pub enum OutputTarget {
    Stdout(OutputFormat),
    File(PathBuf, OutputFormat),
}

/// Install the `littlepandas` drop-in into `sys.modules` and return the
/// module object, so the caller can set `_INGESTED_DATA` on it before
/// running the transformation script.
fn install_littlepandas_dropin<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
    let sys_modules = py.import_bound("sys")?.getattr("modules")?;
    let types = py.import_bound("types")?;
    let module = types.call_method1("ModuleType", ("littlepandas",))?;

    let locals = PyDict::new_bound(py);
    locals.set_item("src", LITTLEPANDAS_DROPIN_PY)?;
    locals.set_item("name", "littlepandas")?;
    let compiled = py.eval_bound("compile(src, name, 'exec')", None, Some(&locals))?;

    let exec_locals = PyDict::new_bound(py);
    exec_locals.set_item("compiled", &compiled)?;
    exec_locals.set_item("module", &module)?;
    py.eval_bound("exec(compiled, module.__dict__)", None, Some(&exec_locals))?;

    sys_modules.set_item("littlepandas", &module)?;
    Ok(module)
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

/// Read a `list[dict]` of records from Python, stringifying each dict's
/// keys -- a headerless DataFrame (`read_input(header=None)`) has integer
/// column names, same as real pandas' default RangeIndex columns, but
/// output formats (CSV headers, etc.) need string column names regardless.
fn records_from_py(records_obj: &Bound<'_, PyAny>) -> PyResult<Vec<Record>> {
    let mut records = Vec::new();
    for row in records_obj.iter()? {
        let row = row?;
        let dict = row.downcast::<PyDict>().map_err(|_| {
            PyRuntimeError::new_err("display(): expected a dict for each record")
        })?;
        let mut record = Record::new();
        for (key, value) in dict.iter() {
            let key: String = match key.extract::<String>() {
                Ok(s) => s,
                Err(_) => key.str()?.to_string(),
            };
            let value: Value = pythonize::depythonize(&value).map_err(|e| {
                PyRuntimeError::new_err(format!("display(): couldn't read value: {e}"))
            })?;
            record.insert(key, value);
        }
        records.push(record);
    }
    Ok(records)
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
            let records = records_from_py(&records_obj)?;
            write_target(&records, &target)
                .map_err(|e| PyRuntimeError::new_err(format!("display(): {e}")))
        },
    )
}

/// Best-effort fallback only: with the stdlib frozen in (see
/// `install_frozen_stdlib`), nothing on the happy path needs `PYTHONHOME` at
/// all, since every stdlib import is satisfied before the filesystem is ever
/// consulted. This only matters for a build with no frozen stdlib embedded
/// (a local dev build against the system's dynamic libpython, which already
/// has a real prefix baked in from its own build, so this is a no-op there
/// too) or for locating an optional `python-runtime/` directory if someone
/// chooses to ship one anyway. Precedence: an already-set `PYTHONHOME` wins,
/// then `DW_PYTHON_HOME`, then a `python-runtime/` sibling directory.
///
/// When something is found, this sets both the `PYTHONHOME` env var *and*
/// calls `Py_SetPythonHome` directly via the C API, since the env var alone
/// was not reliable for a statically-linked interpreter in practice.
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

/// Make `records` (and `headerless_rows`, the same data with the first row
/// kept as an ordinary row rather than consumed as column names) available
/// to `littlepandas.read_input()`/`read_input(header=None)` via
/// `_INGESTED_DATA`/`_INGESTED_DATA_HEADERLESS`, then run the user's
/// transformation script as top-level code (like a notebook cell): it reads
/// the data, transforms it, and calls `display(df)` zero or more times to
/// produce output.
pub fn run_script(
    script_path: &Path,
    records: Vec<Record>,
    headerless_rows: Vec<Vec<Value>>,
    output: OutputTarget,
) -> Result<()> {
    let source = std::fs::read_to_string(script_path)
        .with_context(|| format!("reading transform script {}", script_path.display()))?;

    install_frozen_stdlib();
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
        let littlepandas = install_littlepandas_dropin(py)
            .map_err(|e| anyhow!("installing littlepandas drop-in: {e}"))?;

        let py_data = pythonize::pythonize(py, &records)
            .map_err(|e| anyhow!("converting input records to Python: {e}"))?;
        littlepandas.setattr("_INGESTED_DATA", py_data)?;

        let py_headerless = pythonize::pythonize(py, &headerless_rows)
            .map_err(|e| anyhow!("converting headerless input rows to Python: {e}"))?;
        littlepandas.setattr("_INGESTED_DATA_HEADERLESS", py_headerless)?;

        let globals = PyDict::new_bound(py);
        globals.set_item("__name__", "__main__")?;

        let display_fn = make_display_fn(py, output)
            .map_err(|e| anyhow!("building display() function: {e}"))?;
        globals.set_item("display", display_fn)?;

        py.run_bound(&source, Some(&globals), None)
            .map_err(|e| anyhow!("running {}: {e}", script_path.display()))?;

        Ok(())
    })
}
