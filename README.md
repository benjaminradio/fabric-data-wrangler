# dw — local data-wrangling CLI

Offline CLI for wrangling small CSV/XLSX datasets (< ~10k rows) with plain
Python transformation scripts written exactly like a Fabric notebook cell —
`import pandas as pd`, `df = pd.DataFrame(data)`, ..., `display(df)` — so the
same script cuts and pastes unmodified into a real Fabric notebook later.

```
dw --input data.csv --script transform.py --output out.csv
```

`transform.py`:

```python
import pandas as pd

df = pd.DataFrame(data)
df = df[df["units"].notna()]
summary = df.groupby("region", as_index=False).agg(total=("units", "sum"))
display(summary)
```

See `examples/transform_example.py` and `examples/sample.csv` for a working
example.

## How the cut-and-paste contract works

A script like the one above needs exactly two things to exist that it
didn't create itself: a `data` variable and a `display()` function. Those
are exactly what a Fabric notebook already gives you — an earlier cell
populates `data` (e.g. from a lakehouse read), and `display()` is a Fabric
built-in. Locally, the `dw` CLI supplies both:

- **`data`** — the input CSV/XLSX, ingested host-side in Rust (`csv` /
  `calamine` crates, no Python involved) and injected as a global: a plain
  `list[dict]`, exactly the shape `pd.DataFrame(data)` expects and exactly
  the shape you'd get back from most Fabric ingestion paths too.
- **`import pandas as pd`** — resolves to this project's pure-Python
  drop-in (`crates/cli/src/python/pandas/__init__.py`), installed into
  `sys.modules["pandas"]` before the script runs. In a Fabric notebook, real
  pandas is already installed, so the identical import line resolves to the
  genuine library instead. **This one line is the entire portability
  contract** — nothing else in the script needs an adapter or a different
  import path.
- **`display(df)`** — locally, a native Rust function writes the
  DataFrame's rows (via `.to_dict(orient="records")`, called on whatever is
  passed — real pandas DataFrames included) to CSV/XLSX/NDJSON/table, to
  stdout or `--output`. In Fabric, `display()` renders the DataFrame in the
  notebook instead; the call is a no-op difference either way.

## Architecture

- **`crates/ingest`** — pure Rust CSV (`csv` crate) / XLSX (`calamine`)
  reading and CSV/XLSX/NDJSON/table writing. No Python involved; produces
  and consumes plain `Vec<IndexMap<String, serde_json::Value>>` records.
- **`crates/cli`** — the `dw` binary. Parses arguments (`clap`), embeds a
  Python interpreter (`pyo3`), and:
  1. Installs the `pandas` drop-in into `sys.modules` directly from
     `include_str!`-embedded source — no temp-directory extraction, no
     files shipped beside the binary.
  2. Sets `data` (the ingested records, converted via `pythonize`) and a
     native `display()` closure as globals.
  3. Reads the user's `.py` script from disk (a legitimate input file) and
     runs it as top-level code via `Python::run_bound` — never written to a
     temp file.
- **`crates/cli/src/python/pandas/__init__.py`** — the pure-Python
  DataFrame/Series/GroupBy drop-in, built incrementally as real
  transformation scripts need more of it. Currently covers: column
  selection/assignment, boolean-mask filtering (`df[df["x"] > 5]`),
  `rename`, `sort_values`, `fillna`, `dropna`, `astype`, `apply(axis=1)`,
  `groupby(...).agg(...)` using pandas' named-aggregation tuple shorthand
  (`total=("col", "sum")`) plus `.sum()`/`.mean()`/`.count()`/`.size()`,
  `merge` (inner/left, `on` or `left_on`/`right_on`, suffix handling),
  `drop_duplicates`, `head`/`tail`, `to_dict(orient="records")`.

## Why no real pandas/numpy locally

See the original design doc for the full argument; in short: CPython's
in-memory resource loading (the mechanism this project relies on for
zero-disk-write embedding) only covers pure-Python source/bytecode/data —
not compiled `.so`/`.pyd` extension modules, which must `dlopen` from a real
path on a real filesystem. pandas and numpy ship as dozens of such compiled
extensions; there is no supported way to make third-party wheels like these
built-in/statically-linked without hand-maintaining a fork against a
fast-moving upstream. So the local drop-in runs on plain `list[dict]`
records instead, mirroring only the pandas surface area actually used.

## Known compatibility gaps

This is a subset of pandas, not a reimplementation, and it's missing a real
**Index**. Concretely:

- `groupby(...)` results always come back with the group-by columns as
  regular columns, as if `as_index=False` were always passed. **Always pass
  `as_index=False` explicitly** in scripts that must also run unmodified on
  real pandas, or the two will disagree once you skip `reset_index()`.
- `reset_index()` is a no-op (returns a copy) — fine given the above, but
  it means this drop-in can't catch a script relying on real index
  semantics elsewhere.
- `NaN` vs `None`: missing values are plain `None` here, not `float('nan')`
  (`unit_price` etc. read from CSV as blank become `None`). Comparisons
  and `.fillna()`/`.dropna()` treat `None` as the missing marker
  consistently, but code that specifically checks `math.isnan(x)` will not
  behave the same locally as on real pandas.
- No `.loc`/`.iloc`, no multi-index, no non-`records` `to_dict` orients, no
  `apply(axis=0)`. Add them here as real scripts need them — see the
  drop-in's module docstring.

## Building and running

**Local dev build** (links the system's libpython dynamically — needs a
Python 3 development install, e.g. `python3-dev`/`python3-config` on
Debian/Ubuntu):

```
cargo build --release
./target/release/dw --input examples/sample.csv --script examples/transform_example.py
```

**CI build** (`.github/workflows/build.yml`, runs on every push): downloads
a `python-build-standalone` distribution and statically links its
`libpython*.a` in instead — no system Python needed to build, and no
`libpython.so` dependency at run time either. See "Static linking via CI"
below for how it works and what it produces.

### Output formats

`--format csv|xlsx|ndjson|table`, or inferred from `--output`'s extension.
`--output -` (the default) streams to stdout as a table. Each `display()`
call in the script writes immediately: multiple calls each print their own
block to stdout, or overwrite the same `--output` file in turn (last call
wins).

## Static linking via CI

`.github/workflows/build.yml` runs on every push and does the interpreter
swap the original design called for: no more system-Python dependency to
build, and no `libpython.so` dependency to run. It:

1. Resolves the latest (or a pinned) `astral-sh/python-build-standalone`
   release for CPython 3.11 / `x86_64-unknown-linux-gnu`, preferring an
   optimized "full" build and falling back through `pgo+lto` → `pgo` →
   `lto` → plain if a variant isn't published for this triple.
2. Generates a `pyo3-build-config` file pointing `PYO3_CONFIG_FILE` at that
   distribution's static `libpython*.a` (`shared=false`), instead of
   letting pyo3 auto-discover the system's `python3-config`.
3. Discovers whichever other static libs the distribution bundles for its
   statically-compiled stdlib C extensions (`libbz2.a`, `libffi.a`, etc. —
   `_bz2`, `_ctypes`, `_ssl`, and others get compiled *into* `libpython.a`
   itself in a static build, but still reference symbols from those
   libraries) and passes them to `crates/cli/build.rs`, which emits the
   matching `cargo:rustc-link-lib=static=...` directives. This is a no-op
   locally (the env vars it reads are only set in CI), so it doesn't affect
   a normal dev build against the system's dynamic libpython.
4. Builds `dw`, then **verifies with `ldd`** that the resulting binary has
   no dynamic dependency on libpython (confirmed: only `libc`, `libm`,
   `libgcc_s`, and the dynamic linker remain).
5. **Smoke-tests** the built binary against `examples/` with
   `PYTHONHOME`/`PYTHONPATH` cleared from the environment, to prove it
   doesn't accidentally depend on the runner's own Python.
6. Packages the binary alongside a `python-runtime/` copy of the
   distribution's install prefix (needed for the stdlib — see "Remaining
   gap" below) and uploads it as a build artifact.

pyo3 itself required one nudge for this to work: it refuses to build with
the `auto-initialize` feature against a statically-embeddable Python
("Embedding the Python interpreter statically does not yet have
first-class support in PyO3... disable the auto-initialize feature").
`crates/cli/src/python_runtime.rs` now calls
`pyo3::prepare_freethreaded_python()` explicitly instead — the same thing
`auto-initialize` did implicitly, and it works identically against a
dynamically-linked system libpython, so local dev builds are unaffected.

None of this needed the `data`/`pandas`/`display` injection mechanism (or
anything else in `crates/cli/src/python_runtime.rs` beyond that one
initialization call) to change — the whole point of `pyo3-build-config`'s
env-var-driven interpreter discovery is that swapping the linked
interpreter is a build-environment change, not a code change.

**Remaining gap:** even with libpython itself statically linked, the pure-
Python standard library (`.py`/`.pyc` files) still needs to be found on
disk at run time — this project doesn't yet wire up an
`oxidized-importer`-style in-memory `sys.meta_path` finder for it, so the
released bundle is a binary plus a `python-runtime/` directory
(`configure_python_home` in `python_runtime.rs` points `PYTHONHOME` at it
when it's present as a sibling of the executable), not a single file. True
zero-disk stdlib loading is a distinct, larger follow-up: wiring in the
`oxidized-importer` crate (the standalone, non-PyOxidizer-tool crate
published under that name) as a meta path finder over the distribution's
packed resources data.

## What's deliberately out of scope for v1

- Zip-of-modules transformation input (single `.py` file only).
- A complete pandas API surface — methods are added as real scripts need
  them (see the drop-in's module docstring).
- Fully single-file distribution (binary with no companion directory) —
  see "Remaining gap" above.
