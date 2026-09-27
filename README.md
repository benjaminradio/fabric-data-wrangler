# dw — local data-wrangling CLI

Offline CLI for wrangling small CSV/XLSX datasets (< ~10k rows) with plain
Python transformation scripts that look like a Fabric notebook cell —
`import littlepandas as pd`, `df = pd.read_input()`, ..., `display(df)`.

```
dw --input data.csv --script transform.py --output out.csv
```

`transform.py`:

```python
import littlepandas as pd

df = pd.read_input()
df = df[df["units"].notna()]
summary = df.groupby("region", as_index=False).agg(total=("units", "sum"))
display(summary)
```

See `examples/transform_example.py` and `examples/sample.csv` for a working
example.

## `littlepandas`, not `pandas`

This project ships a small, pure-Python DataFrame drop-in
(`crates/cli/src/python/littlepandas/__init__.py`) so transformation
scripts can be written in pandas-shaped code without needing real
pandas/numpy embedded (see "Why no real pandas/numpy locally" below). An
earlier version of this project installed that drop-in *as*
`sys.modules["pandas"]`, so `import pandas as pd` resolved to it locally
and to the real thing in Fabric, with no script changes at all. That's
tidy, but it hides something important: a script's `import pandas` would
silently mean two completely different libraries depending on where it
runs, with no visible marker of that fact in the code itself.

So the drop-in is named, and installed as, `littlepandas` instead — `import
littlepandas as pd` only ever resolves locally. Porting a script to a real
Fabric notebook is consequently a small, *visible* two-line diff rather
than a silent swap:

1. `import littlepandas as pd` → `import pandas as pd`.
2. `df = pd.read_input()` → whatever real ingestion fits Fabric (a
   lakehouse table read, `pd.read_csv`, ...).

`pd.DataFrame(...)` itself behaves exactly like real pandas: called with no
arguments it makes an empty frame, and it accepts either a list of row
dicts or a dict of columns (`pd.DataFrame({"Name": ["Alice", "Bob"], "Age":
[25, 30]})`) for literal tables written directly in a script — no special
local-only behavior there. **`pd.read_input()`, not `DataFrame`, is the
local-only call**, named to match pandas' own `read_csv`/`read_excel`/etc.
family: it reads the CLI's ingested `--input` file. Unlike `DataFrame`,
`read_input()` has no real-pandas meaning to fall back to — it's a visible
line marking "replace this with a real data source," not a silent stand-in
for one.

## How the cut-and-paste contract works

- **`littlepandas.read_input()`** — the Rust host ingests `--input`
  host-side (`csv`/`calamine` crates, no Python involved) and, before
  running the script, sets it on the `littlepandas` module as
  `_INGESTED_DATA`. `read_input()` is a thin wrapper returning
  `DataFrame(_INGESTED_DATA)`.
- **`display(df)`** — a native Rust function that writes the DataFrame's
  rows (via `.to_dict(orient="records")`, called on whatever is passed —
  a real pandas DataFrame included) to CSV/XLSX/NDJSON/table, to stdout or
  `--output`. In Fabric, `display()` is already a notebook built-in with
  the same name; the call itself needs no change either way.

## Architecture

- **`crates/ingest`** — pure Rust CSV (`csv` crate) / XLSX (`calamine`)
  reading and CSV/XLSX/NDJSON/table writing. No Python involved; produces
  and consumes plain `Vec<IndexMap<String, serde_json::Value>>` records.
- **`crates/cli`** — the `dw` binary. Parses arguments (`clap`), embeds a
  Python interpreter (`pyo3`), and:
  1. Installs the `littlepandas` drop-in into `sys.modules` directly from
     `include_str!`-embedded source — no temp-directory extraction, no
     files shipped beside the binary.
  2. Sets the ingested records (converted via `pythonize`) as
     `littlepandas._INGESTED_DATA`, and a native `display()` closure as a
     script global.
  3. Reads the user's `.py` script from disk (a legitimate input file) and
     runs it as top-level code via `Python::run_bound` — never written to a
     temp file.
- **`crates/cli/src/python/littlepandas/__init__.py`** — the pure-Python
  DataFrame/Series/GroupBy drop-in, built incrementally as real
  transformation scripts need more of it. Currently covers: construction
  from a list of row dicts or a dict of columns, column
  selection/assignment, boolean-mask filtering (`df[df["x"] > 5]`),
  `rename`, `sort_values`, `fillna`/`dropna`, `ffill`/`bfill`, `pop`,
  `where`/`mask` (row-wise condition, not full elementwise), `assign`,
  `astype`, `map` (elementwise) and `apply(axis=1)` (row-wise),
  `groupby(...).agg(...)` using pandas' named-aggregation tuple shorthand
  (`total=("col", "sum")`) plus `.sum()`/`.mean()`/`.count()`/`.size()`,
  `merge` (inner/left, `on` or `left_on`/`right_on`, suffix handling),
  `drop_duplicates`, `head`/`tail`, `melt` (wide→long), `pivot`
  (long→wide, no aggregation), module-level `concat` (row-wise stacking,
  union of columns), `to_dict(orient="records")`.

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

`littlepandas` is a subset of pandas' API surface, not a reimplementation,
and it's missing a real **Index**. Concretely:

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
  `apply(axis=0)`, no `pivot_table` (aggregating pivot — only the plain,
  non-aggregating `pivot` exists). Add them here as real scripts need them
  — see the drop-in's module docstring.
- `where`/`mask` take a row-wise condition (a boolean Series, one value per
  row, or a `callable(row) -> bool`) and replace or keep the *whole row*
  accordingly — matching real pandas' behavior for that specific case
  (`df.where(series_cond, other)`, verified against a real installation),
  but not real pandas' more general per-cell/whole-frame elementwise
  `df.where(df > 0, 0)`, since this drop-in has no DataFrame-wide
  elementwise comparison operators to build a per-cell condition from.

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

`.github/workflows/build.yml` runs on every push, as a matrix over
`ubuntu-latest` (`x86_64-unknown-linux-gnu`) and `windows-latest`
(`x86_64-pc-windows-msvc`), and produces a `dw`/`dw.exe` binary with
Python fully embedded: no system Python needed to build it, no
`libpython.so`/`pythonXY.dll` dependency to run it, and — the part that
makes it an actual single file — **no companion directory for the
standard library either**. Both platforms share one script (Windows
runners ship Git Bash, so the whole job runs under `shell: bash`), which
branches only where the two genuinely differ (library/executable naming,
stdlib directory layout — POSIX's `lib/pythonX.Y/` vs. Windows' `Lib/`
directly under the install prefix). For each platform, it:

1. Resolves the latest (or a pinned) `astral-sh/python-build-standalone`
   release for CPython 3.11 / the target triple, preferring an optimized
   "full" build and falling back through a few variant names if a given
   one isn't published for that triple.
2. Generates a `pyo3-build-config` file pointing `PYO3_CONFIG_FILE` at that
   distribution's static Python lib (`libpython*.a` on Linux,
   `python3*.lib` on Windows; `shared=false`), instead of letting pyo3
   auto-discover a system interpreter.
3. Discovers whichever other static libs the distribution bundles for its
   statically-compiled stdlib C extensions (`libbz2`, `libffi`, etc. —
   `_bz2`, `_ctypes`, `_ssl`, and others get compiled *into* the Python lib
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
   against a system interpreter is unaffected. None of this Rust code is
   platform-specific; `pyo3-build-config`/`pyo3::ffi` are what make the
   same source work against either target.
6. **Verifies there's no dynamic dependency on the Python library**: `ldd`
   on Linux (confirmed: only `libc`, `libm`, `libgcc_s`, and the dynamic
   linker remain), `dumpbin /dependents` on Windows where available
   (informational — the isolation test below is the authoritative check).
7. **Smoke-tests in total isolation**: copies *only* the binary plus a
   sample input/script into an empty temp directory elsewhere (no
   `python-runtime/`/`Lib/`, no repo checkout) and runs it there with a
   fully scrubbed environment (`env -i`, no `PYTHONHOME`/`PYTHONPATH`) —
   proving it's genuinely self-contained, not just "works when some
   environment variable happens to be set right."
8. Packages **the binary alone** (`.tar.gz` on Linux, `.zip` on Windows)
   and uploads it as a build artifact.

**Windows is new and less battle-tested than Linux here**: this project's
sandbox has no Windows/MSVC toolchain to develop or verify against
locally, so unlike the Linux path (which went through several real
CI-driven fixes before it worked — see the git history), the Windows job
is a best-effort first pass, validated only by whatever the CI runs
themselves show. If it needs another round of fixes (MSVC has its own
static-vs-import-library and CRT-linkage subtleties that differ from
Linux's `.a` static archives), check the latest Actions run for that
branch.

pyo3 itself needed one nudge along the way: it refuses to build with the
`auto-initialize` feature against a statically-embeddable Python
("Embedding the Python interpreter statically does not yet have
first-class support in PyO3... disable the auto-initialize feature").
`crates/cli/src/python_runtime.rs` calls `pyo3::prepare_freethreaded_python()`
explicitly instead — the same thing `auto-initialize` did implicitly, and
it works identically against a dynamically-linked system libpython, so
local dev builds are unaffected.

None of this needed the `littlepandas`/`display` injection mechanism to
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
project's own code (the `littlepandas` drop-in, the CLI itself) hits that case,
but a transformation script importing something unusual might. Widening the
freeze coverage or handling that fallback is future work if it comes up.

## What's deliberately out of scope for v1

- Zip-of-modules transformation input (single `.py` file only).
- A complete pandas API surface — methods are added as real scripts need
  them (see the drop-in's module docstring).
- A fallback path for stdlib C extensions that aren't compiled in as
  builtins on some other target triple/build variant — see "Known
  remaining gap" above.
