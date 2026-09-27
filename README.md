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
a `python-build-standalone` distribution, statically links its
`libpython*.a` in, and freezes the standard library into the binary as
bytecode — a true single-file `dw` with no system Python needed to build
it and nothing else needed alongside it to run it. See "True single-binary
embedding, via CI" below for how it works and what it produces.

### Output formats

`--format csv|xlsx|ndjson|table`, or inferred from `--output`'s extension.
`--output -` (the default) streams to stdout as a table. Each `display()`
call in the script writes immediately: multiple calls each print their own
block to stdout, or overwrite the same `--output` file in turn (last call
wins).

## True single-binary embedding, via CI

`.github/workflows/build.yml` runs on every push and produces a `dw` binary
with Python fully embedded: no system Python needed to build it, no
`libpython.so` dependency to run it, and — the part that makes it an
actual single file — **no companion directory for the standard library
either**. It:

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
   matching `cargo:rustc-link-lib=static=...` directives.
4. Runs `scripts/freeze_stdlib.py`, using *that exact downloaded
   interpreter*, to compile every pure-Python stdlib module to marshalled
   bytecode and pack it into one blob + manifest.
5. Builds `dw`. `crates/cli/build.rs` embeds that blob via `include_bytes!`
   and generates a small entry table; `python_runtime.rs`'s
   `install_frozen_stdlib` registers every module in it as a CPython
   **frozen module** — the exact mechanism CPython itself already uses to
   embed `importlib._bootstrap` — by prepending them to
   `PyImport_FrozenModules` before the interpreter initializes.
   `FrozenImporter` is always first on `sys.meta_path`, so `encodings`,
   `io`, and everything else the interpreter needs at startup — and
   anything a transformation script imports — resolves from memory before
   the filesystem is ever consulted. All of steps 3–4 are no-ops locally
   (the env vars they read are only set in CI), so a normal dev build
   against the system's dynamic libpython/stdlib is unaffected.
6. **Verifies with `ldd`** that the resulting binary has no dynamic
   dependency on libpython (confirmed: only `libc`, `libm`, `libgcc_s`, and
   the dynamic linker remain).
7. **Smoke-tests in total isolation**: copies *only* the binary plus a
   sample input/script into an empty temp directory elsewhere (no
   `python-runtime/`, no repo checkout) and runs it there with a fully
   scrubbed environment (`env -i`, no `PYTHONHOME`/`PYTHONPATH`) — proving
   it's genuinely self-contained, not just "works when PYTHONHOME happens
   to be set right."
8. Packages **the binary alone** and uploads it as a build artifact.

pyo3 itself needed one nudge along the way: it refuses to build with the
`auto-initialize` feature against a statically-embeddable Python
("Embedding the Python interpreter statically does not yet have
first-class support in PyO3... disable the auto-initialize feature").
`crates/cli/src/python_runtime.rs` calls `pyo3::prepare_freethreaded_python()`
explicitly instead — the same thing `auto-initialize` did implicitly, and
it works identically against a dynamically-linked system libpython, so
local dev builds are unaffected.

None of this needed the `data`/`pandas`/`display` injection mechanism to
change at all — freezing the stdlib and swapping the linked interpreter are
both build-environment changes, layered underneath that mechanism rather
than through it.

**Known remaining gap:** the freeze script compiles every pure-Python
stdlib module it finds under the distribution's `lib/pythonX.Y/` (skipping
`test`/`tests`/`lib2to3`/`site-packages`), and essentially all commonly-used
C-extension modules come pre-compiled as interpreter builtins in a
`python-build-standalone` static build (confirmed by the extra-static-libs
step above — `_bz2`, `_ctypes`, etc. are already linked in, not loaded from
`.so` files). An obscure extension module that a *different*
python-build-standalone build variant ships as a separate `lib-dynload/*.so`
rather than a builtin would still need that file on disk; none of this
project's own code (the `pandas` drop-in, the CLI itself) hits that case,
but a transformation script importing something unusual might. Widening the
freeze coverage or handling that fallback is future work if it comes up.

## What's deliberately out of scope for v1

- Zip-of-modules transformation input (single `.py` file only).
- A complete pandas API surface — methods are added as real scripts need
  them (see the drop-in's module docstring).
- A fallback path for stdlib C extensions that aren't compiled in as
  builtins on some other target triple/build variant — see "Known
  remaining gap" above.
