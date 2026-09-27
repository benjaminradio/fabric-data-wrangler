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

```
cargo build --release
./target/release/dw --input examples/sample.csv --script examples/transform_example.py
```

Requires a Python 3 development install on the build machine for now (see
"Spike status" below) — e.g. `python3-dev`/`python3-config` on Debian/Ubuntu.

### Output formats

`--format csv|xlsx|ndjson|table`, or inferred from `--output`'s extension.
`--output -` (the default) streams to stdout as a table. Each `display()`
call in the script writes immediately: multiple calls each print their own
block to stdout, or overwrite the same `--output` file in turn (last call
wins).

## Spike status — what was verified here vs. what remains

This environment's outbound network access is restricted to a small
allow-list (crates.io, pypi.org, npm, a few others) and does **not** include
`github.com`, which is where `python-build-standalone` distributions are
published as release assets. That specific download was not reachable from
here, so the final interpreter swap described below could not be built or
run in this session. Everything else was implemented and exercised:

**Verified in this sandbox:**
- The Rust workspace builds cleanly (`cargo build --workspace`, zero warnings).
- End-to-end: CSV/XLSX input → embedded Python (`data` + `pd` drop-in +
  `display()`) → CSV/NDJSON/table/XLSX output, all correct (see `examples/`).
- **Cut-and-paste fidelity**: `examples/transform_example.py` was run
  byte-for-byte unmodified (only substituting a real-pandas-backed `data`
  loader and a trivial `display()` shim in place of the CLI's injected
  globals) against a real, installed pandas and produced identical output
  to the local drop-in run — confirming the portability contract actually
  holds for that script.
- The `pandas` drop-in's own loading mechanism — `include_str!` source
  compiled and installed into `sys.modules` directly, no filesystem
  extraction — works as designed and is independent of the interpreter
  swap below.

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
memory additionally needs the `oxidized-importer` crate (the standalone,
non-PyOxidizer-tool crate published under that name) wired in as a
`sys.meta_path` finder over the distribution's packed resources data.
Neither of these needs anything in this codebase's structure to change;
they replace `Python::with_gil`'s initialization path in
`crates/cli/src/python_runtime.rs` only — the `data`/`pandas`/`display`
injection mechanism stays exactly as-is.

## What's deliberately out of scope for v1

- Zip-of-modules transformation input (single `.py` file only).
- A complete pandas API surface — methods are added as real scripts need
  them (see the drop-in's module docstring).
- The `python-build-standalone`/`oxidized-importer` interpreter swap (see
  "Spike status").
