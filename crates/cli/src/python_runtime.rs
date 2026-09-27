//! Embeds the Python interpreter and runs a user transformation script
//! against in-memory records.
//!
//! **No disk writes for embedding Python.** The `wrangle` package
//! (`__init__.py`, `_local.py`, `_fabric.py`) is compiled into the binary at
//! build time via `include_str!` and installed into `sys.modules` directly
//! from those in-memory strings -- it is never extracted to a temp
//! directory or shipped as loose files beside the binary. The user's
//! transformation script is read from disk (a legitimate input file, not a
//! packaged resource) and compiled from the in-memory string via
//! `compile()`/`exec` -- again, no intermediate file.
//!
//! What this does NOT yet do (see README "Spike status"): the interpreter
//! itself is linked against the *system* libpython (via pyo3's
//! `auto-initialize`, which shells out to `python3-config`), not a
//! statically-linked `python-build-standalone` distribution with its
//! stdlib loaded through an `oxidized-importer`-style in-memory finder.
//! That swap is the next step and needs network access to fetch a
//! `python-build-standalone` release, which this development environment
//! did not have. The `wrangle` package loading mechanism here (manual
//! `sys.modules` injection from `include_str!` source, no `PyModule::from_code`
//! touching disk) is exactly the mechanism that continues to work once that
//! swap happens -- only the interpreter's own bootstrap changes, not this code.

use anyhow::{anyhow, Context, Result};
use dw_ingest::Record;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule};

const WRANGLE_INIT_PY: &str = include_str!("python/wrangle/__init__.py");
const WRANGLE_LOCAL_PY: &str = include_str!("python/wrangle/_local.py");
const WRANGLE_FABRIC_PY: &str = include_str!("python/wrangle/_fabric.py");

/// Install the `wrangle` package into `sys.modules` from in-memory source,
/// bypassing the filesystem-based import machinery entirely for our own code.
fn install_wrangle_package(py: Python<'_>) -> PyResult<()> {
    let sys_modules = py.import_bound("sys")?.getattr("modules")?;

    // `wrangle` itself must look like a package (needs `__path__`) so that
    // `from wrangle._local import *` inside its `__init__.py` resolves via
    // the submodules we register below rather than trying to hit disk.
    let types = py.import_bound("types")?;
    let package = types.call_method1("ModuleType", ("wrangle",))?;
    package.setattr("__path__", Vec::<String>::new())?;
    sys_modules.set_item("wrangle", &package)?;

    for (name, source) in [
        ("wrangle._local", WRANGLE_LOCAL_PY),
        ("wrangle._fabric", WRANGLE_FABRIC_PY),
    ] {
        let submodule = types.call_method1("ModuleType", (name,))?;
        let compiled = py.eval_bound(
            "compile(src, name, 'exec')",
            None,
            Some(&{
                let locals = PyDict::new_bound(py);
                locals.set_item("src", source)?;
                locals.set_item("name", name)?;
                locals
            }),
        )?;
        py.eval_bound(
            "exec(compiled, module.__dict__)",
            None,
            Some(&{
                let locals = PyDict::new_bound(py);
                locals.set_item("compiled", &compiled)?;
                locals.set_item("module", &submodule)?;
                locals
            }),
        )?;
        sys_modules.set_item(name, &submodule)?;
        package.setattr(name.rsplit('.').next().unwrap(), &submodule)?;
    }

    // Now run wrangle/__init__.py in the package's own namespace.
    let init_locals = PyDict::new_bound(py);
    init_locals.set_item("src", WRANGLE_INIT_PY)?;
    init_locals.set_item("name", "wrangle")?;
    let compiled = py.eval_bound("compile(src, name, 'exec')", None, Some(&init_locals))?;
    let exec_locals = PyDict::new_bound(py);
    exec_locals.set_item("compiled", &compiled)?;
    exec_locals.set_item("module", &package)?;
    py.eval_bound("exec(compiled, module.__dict__)", None, Some(&exec_locals))?;

    Ok(())
}

/// Compile and run the user's transformation script against `records`,
/// calling its module-level `transform(records) -> list[dict]` function.
pub fn run_transform(script_path: &std::path::Path, records: Vec<Record>) -> Result<Vec<Record>> {
    let source = std::fs::read_to_string(script_path)
        .with_context(|| format!("reading transform script {}", script_path.display()))?;
    let script_name = script_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("transform.py")
        .to_string();

    Python::with_gil(|py| -> Result<Vec<Record>> {
        install_wrangle_package(py).map_err(|e| anyhow!("installing wrangle package: {e}"))?;

        let module = PyModule::from_code_bound(py, &source, &script_name, "user_transform")
            .map_err(|e| anyhow!("compiling/running {}: {e}", script_path.display()))?;

        let transform_fn = module.getattr("transform").map_err(|_| {
            anyhow!(
                "{} must define a top-level `transform(records)` function",
                script_path.display()
            )
        })?;

        let py_records = pythonize::pythonize(py, &records)
            .map_err(|e| anyhow!("converting input records to Python: {e}"))?;

        let py_result = transform_fn
            .call1((py_records,))
            .map_err(|e| anyhow!("running transform() in {}: {e}", script_path.display()))?;

        let result: Vec<Record> = pythonize::depythonize(&py_result)
            .map_err(|e| anyhow!("converting transform() output back to records: {e}"))?;

        Ok(result)
    })
}
