# dw — local data-wrangling CLI

Offline CLI for wrangling small CSV/XLSX datasets (< ~10k rows) with plain
Python transformation scripts, without pandas/numpy, designed so the same
transformation code runs unmodified later in a Microsoft Fabric notebook.

```
dw --input data.csv --script transform.py --output out.csv
```

`transform.py` defines a single entry point:

```python
import wrangle

def transform(records):
    rows = wrangle.filter_rows(records, lambda r: r["units"] is not None)
    return wrangle.group_by(rows, by="region", total=("units", "sum"))
```

`records` is a plain `list[dict]`. See `examples/transform_example.py` and
`examples/sample.csv` for a working example.

## Architecture

- **`crates/ingest`** — pure Rust CSV (`csv` crate) / XLSX (`calamine`)
  reading and CSV/XLSX/NDJSON/table writing. No Python involved; produces
  and consumes plain `Vec<IndexMap<String, serde_json::Value>>` records.
- **`crates/cli`** — the `dw` binary. Parses arguments (`clap`), embeds a
  Python interpreter (`pyo3`), and:
  1. Installs the `wrangle` package into `sys.modules` directly from
     `include_str!`-embedded source (`crates/cli/src/python/wrangle/*.py`) —
     no temp-directory extraction, no files shipped beside the binary.
  2. Reads the user's `.py` transform script from disk (a legitimate input
     file) and compiles it in memory via `PyModule::from_code`.
  3. Converts records to/from Python via `pythonize`, calls `transform(records)`.
- **`wrangle` package** (`crates/cli/src/python/wrangle/`):
  - `_local.py` — the pure-Python drop-in verb library: `select`, `rename`,
    `filter_rows`, `sort_by`, `mutate`, `group_by` (with per-column `agg`),
    `join` (inner/left), `dedupe`, `fillna`, `cast`, `head`, `tail`. Plain
    functions over `list[dict]`, no classes, no third-party dependencies —
    only this needs to load from memory with zero disk writes.
  - `_fabric.py` — a pandas-backed adapter exposing the *identical*
    function signatures, accepting and returning the same `list[dict]`
    shape, so calling code is byte-for-byte identical either way.
  - `__init__.py` — the portability shim. Autodetects environment
    (`notebookutils`/`pyspark` importable ⇒ Fabric; else local), or honors
    `WRANGLE_BACKEND=local|fabric` to force one, and re-exports the chosen
    backend's verbs as `wrangle.*`. **This is the whole portability
    contract**: a transformation script only ever does `import wrangle` and
    calls verbs on it; nothing else differs between local and Fabric.

Both backends were verified side by side against the same input (see
"What was verified" below) and produce identical output shape.

## Why no pandas/numpy

See the original design doc for the full argument; in short: CPython's
in-memory resource loading (the mechanism this project relies on for
zero-disk-write embedding) only covers pure-Python source/bytecode/data —
not compiled `.so`/`.pyd` extension modules, which must `dlopen` from a real
path on a real filesystem. pandas and numpy ship as dozens of such compiled
extensions; there is no supported way to make third-party wheels like these
built-in/statically-linked without hand-maintaining a fork against a
fast-moving upstream. So all local transformation logic runs on pure-Python
`list[dict]` records instead, and the pandas dependency is pushed entirely
into the Fabric-side adapter, where a real pandas/PySpark install already
exists.

## Spike status — what was verified here vs. what remains

This environment's outbound network access is restricted to a small
allow-list (crates.io, pypi.org, npm, a few others) and does **not** include
`github.com`, which is where `python-build-standalone` distributions are
published as release assets. That specific download was not reachable from
here, so the final swap described below could not be built or run in this
session. Everything else in the design was implemented and exercised:

**Verified in this sandbox:**
- The Rust workspace builds cleanly (`cargo build --workspace`, zero warnings).
- End-to-end pipeline: CSV → embedded Python (`wrangle` + user script) →
  CSV/NDJSON/table/XLSX output, and XLSX → ... → table, all produce correct
  results (see `examples/`).
- The `wrangle` package's own loading mechanism — `include_str!` source
  compiled and installed into `sys.modules` directly, no filesystem
  extraction — works exactly as designed. This is the piece the design doc
  flagged as needing `oxidized_importer`/`OxidizedFinder`-style memory
  loading; the manual `sys.modules` injection here achieves the same
  zero-disk-write property for our own resources without depending on that
  crate at all. It will keep working unchanged after the interpreter swap
  below, since it doesn't touch the interpreter's own bootstrap.
- Both `wrangle` backends (`_local` pure-Python, `_fabric` pandas-backed,
  the latter tested with a real `pandas` install) were run against the same
  fixture data through the public API and produce identical results.
- User transformation scripts are compiled straight from an in-memory
  string (`PyModule::from_code`) — never written to a temp file.

**Not yet done — the interpreter itself:**
`dw` currently embeds Python via `pyo3`'s `auto-initialize` feature, which
links against **the system's libpython** (found via `python3-config` on
`PATH` at build time) rather than a statically-linked
`python-build-standalone` distribution. That means:
- The binary today is *not* dependency-free — it needs a compatible
  libpython on the machine that built it (and, for a dynamically-linked
  build, at runtime too).
- The interpreter's own standard library is loaded the normal way (from
  disk paths baked in at Python's own build time), not through an
  in-memory resource loader.

The follow-up spike this design doc calls for is still open: fetch a
`python-build-standalone` release for the target triple, point
`PYO3_PYTHON`/`PYO3_CONFIG_FILE` at its statically-linked `libpython`, and
replace `auto-initialize` linking with that config so the final binary has
no external Python dependency. Loading the *stdlib itself* purely from
memory (rather than from the distribution's on-disk layout) additionally
needs the `oxidized-importer` crate (the standalone, non-PyOxidizer-tool
crate published under that name) wired in as a `sys.meta_path` finder over
the distribution's packed resources data — this is exactly the
`OxidizedFinder`/`oxidized_importer` mechanism the design doc identifies as
the relevant building block, decoupled from needing the full PyOxidizer
build tool. Neither of these needed anything from this codebase's structure
to change; they replace `Python::with_gil`'s initialization path in
`crates/cli/src/python_runtime.rs` only.

## Building and running

```
cargo build --release
./target/release/dw --input examples/sample.csv --script examples/transform_example.py
```

Requires a Python 3 development install on the build machine for now (see
"Spike status" above) — e.g. `python3-dev`/`python3-config` on Debian/Ubuntu.

### Output formats

`--format csv|xlsx|ndjson|table`, or inferred from `--output`'s extension.
`--output -` (the default) streams to stdout as a table.

## Fabric portability

To validate the adapter without a real Fabric workspace:

```
pip install pandas
WRANGLE_BACKEND=fabric python3 -c "
import sys; sys.path.insert(0, 'crates/cli/src/python')
import wrangle
print(wrangle.group_by([{'r':'e','v':1},{'r':'e','v':2}], 'r', total=('v','sum')))
"
```

In an actual Fabric notebook, ship `wrangle/__init__.py` and `_fabric.py`
(not `_local.py`) as a notebook-attached library or workspace package; the
autodetection (`notebookutils`/`pyspark` present) picks `_fabric`
automatically, so no code changes are needed in the transformation scripts
themselves — only the deployment/import step, per the "same code, adapter
shim" strategy.

## What's deliberately out of scope for v1

- Zip-of-modules transformation input (single `.py` file only).
- A fixed/complete verb API — verbs are added as real scripts need them
  (see `wrangle/_local.py`'s docstring).
- The `python-build-standalone`/`oxidized-importer` interpreter swap (see
  "Spike status").
