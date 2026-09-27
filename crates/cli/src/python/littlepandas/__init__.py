"""littlepandas: a small, honestly-named, pure-Python drop-in for the tiny
slice of the pandas API this project uses, built incrementally as real
transformation scripts need more of it.

This is deliberately **not** called ``pandas`` and does not pretend to be
it: it is installed as ``sys.modules["littlepandas"]`` by the Rust host, so
a script does ``import littlepandas as pd`` -- an import that only resolves
locally. Porting a script to a real Fabric notebook is a small, visible
two-line diff, not a silent swap of what an unqualified ``import pandas``
means:

1. ``import littlepandas as pd`` becomes ``import pandas as pd``.
2. ``df = pd.read_input()`` (see below) becomes whatever real ingestion
   call fits Fabric (a lakehouse read, ``pd.read_csv``, ...).

``pd.DataFrame(...)`` behaves like real pandas: called with no arguments it
makes an empty frame, and it accepts either a list of row dicts
(``[{"a": 1}, {"a": 2}]``) or a dict of columns
(``{"Name": ["Alice", "Bob"], "Age": [25, 30]}``), for building small
literal tables directly in a script.

**``pd.read_input()`` reads the CLI's ingested ``--input`` file**, named
to match pandas' own ``read_csv``/``read_excel``/etc. family. It only
exists here -- there's no equivalent to strip out when porting, just a
line to replace with a real read. The Rust host sets the module-level
``_INGESTED_DATA`` below after loading ``--input`` and before running the
script; ``read_input()`` is a thin wrapper (`DataFrame(_INGESTED_DATA)`)
around it.

Known compatibility gap (the big one): this DataFrame has no real pandas
Index. Operations that in real pandas move something into (or read
something out of) the index -- e.g. ``groupby(...)`` without
``as_index=False``, or relying on a preserved row index after filtering --
behave here as if ``as_index=False`` were always in effect, and
``reset_index()`` is a no-op. Scripts that need to run unmodified on real
pandas too should pass ``as_index=False`` explicitly to ``groupby`` and
avoid depending on index alignment.
"""

from itertools import groupby as _itertools_groupby

__all__ = ["DataFrame", "Series", "concat", "read_input"]

# Set by the Rust host (crates/cli/src/python_runtime.rs) after loading
# --input and before running the transformation script. `read_input()`
# reads from here -- see the module docstring above.
_INGESTED_DATA = None


def read_input():
    """Return the CLI's ingested ``--input`` data as a DataFrame.

    Named to match pandas' ``read_csv``/``read_excel``/etc. family. Unlike
    those, this has no real-pandas equivalent to fall back to -- porting to
    Fabric means replacing this call with whatever real ingestion fits
    there (a lakehouse read, ``pd.read_csv``, ...).
    """
    return DataFrame(_INGESTED_DATA if _INGESTED_DATA is not None else [])


def _is_missing(v):
    return v is None


class Series:
    """A single named column: a list of values plus elementwise ops."""

    def __init__(self, values, name=None):
        self._values = list(values)
        self.name = name

    def __len__(self):
        return len(self._values)

    def __iter__(self):
        return iter(self._values)

    def __getitem__(self, i):
        return self._values[i]

    def tolist(self):
        return list(self._values)

    def _binop(self, other, fn):
        if isinstance(other, Series):
            other_values = other._values
        elif isinstance(other, (list, tuple)):
            other_values = other
        else:
            other_values = [other] * len(self._values)
        return Series([fn(a, b) for a, b in zip(self._values, other_values)])

    def _cmp(self, other, fn):
        return self._binop(other, fn)

    def __eq__(self, other):
        return self._cmp(other, lambda a, b: a == b)

    def __ne__(self, other):
        return self._cmp(other, lambda a, b: a != b)

    def __lt__(self, other):
        return self._cmp(other, lambda a, b: a < b)

    def __le__(self, other):
        return self._cmp(other, lambda a, b: a <= b)

    def __gt__(self, other):
        return self._cmp(other, lambda a, b: a > b)

    def __ge__(self, other):
        return self._cmp(other, lambda a, b: a >= b)

    def __add__(self, other):
        return self._binop(other, lambda a, b: a + b)

    def __radd__(self, other):
        return self._binop(other, lambda a, b: b + a)

    def __sub__(self, other):
        return self._binop(other, lambda a, b: a - b)

    def __rsub__(self, other):
        return self._binop(other, lambda a, b: b - a)

    def __mul__(self, other):
        return self._binop(other, lambda a, b: a * b)

    def __rmul__(self, other):
        return self._binop(other, lambda a, b: b * a)

    def __truediv__(self, other):
        return self._binop(other, lambda a, b: a / b)

    def __and__(self, other):
        return self._binop(other, lambda a, b: bool(a) and bool(b))

    def __or__(self, other):
        return self._binop(other, lambda a, b: bool(a) or bool(b))

    def __invert__(self):
        return Series([not bool(v) for v in self._values])

    def isna(self):
        return Series([_is_missing(v) for v in self._values])

    def notna(self):
        return Series([not _is_missing(v) for v in self._values])

    def map(self, fn):
        return Series([fn(v) if not _is_missing(v) else v for v in self._values], self.name)

    def astype(self, to):
        return Series([to(v) if not _is_missing(v) else v for v in self._values], self.name)

    def fillna(self, value):
        return Series([value if _is_missing(v) else v for v in self._values], self.name)

    def ffill(self):
        result = []
        last = None
        for v in self._values:
            last = v if not _is_missing(v) else last
            result.append(last)
        return Series(result, self.name)

    def bfill(self):
        result = [None] * len(self._values)
        nxt = None
        for i in range(len(self._values) - 1, -1, -1):
            v = self._values[i]
            nxt = v if not _is_missing(v) else nxt
            result[i] = nxt
        return Series(result, self.name)

    def _numeric(self):
        return [v for v in self._values if not _is_missing(v)]

    def sum(self):
        return sum(self._numeric())

    def mean(self):
        vals = self._numeric()
        return sum(vals) / len(vals) if vals else None

    def min(self):
        vals = self._numeric()
        return min(vals) if vals else None

    def max(self):
        vals = self._numeric()
        return max(vals) if vals else None

    def count(self):
        return len(self._numeric())

    def nunique(self):
        return len({v for v in self._numeric()})

    def unique(self):
        seen = []
        for v in self._values:
            if v not in seen:
                seen.append(v)
        return seen

    def __repr__(self):
        return f"Series({self._values!r})"


class DataFrame:
    """Row-oriented (``list[dict]``) drop-in for the pandas DataFrame surface used here."""

    def __init__(self, data=None, columns=None):
        if data is None:
            rows = []
        elif isinstance(data, dict):
            # Dict of columns, e.g. {"Name": ["Alice", "Bob"], "Age": [25, 30]}.
            col_names = list(data.keys())
            length = len(next(iter(data.values()))) if data else 0
            rows = [{c: data[c][i] for c in col_names} for i in range(length)]
            if columns is None:
                columns = col_names
        else:
            rows = list(data)
        if columns is not None:
            self._columns = list(columns)
        else:
            self._columns = []
            for row in rows:
                for k in row.keys():
                    if k not in self._columns:
                        self._columns.append(k)
        self._rows = [dict(row) for row in rows]

    # -- construction / conversion -----------------------------------
    @classmethod
    def _from_rows(cls, rows, columns=None):
        df = cls.__new__(cls)
        df._rows = rows
        df._columns = list(columns) if columns is not None else (
            list(rows[0].keys()) if rows else []
        )
        return df

    def copy(self):
        return DataFrame._from_rows([dict(r) for r in self._rows], list(self._columns))

    def to_dict(self, orient="records"):
        if orient != "records":
            raise NotImplementedError("only orient='records' is supported")
        return [dict(row) for row in self._rows]

    def reset_index(self, drop=True):
        # No Index is modeled; this is a no-op that returns a copy, matching
        # the effect of `reset_index(drop=True)` on an as_index=False frame.
        return self.copy()

    # -- shape ----------------------------------------------------------
    def __len__(self):
        return len(self._rows)

    @property
    def shape(self):
        return (len(self._rows), len(self._columns))

    @property
    def columns(self):
        return list(self._columns)

    @columns.setter
    def columns(self, new_columns):
        new_columns = list(new_columns)
        renamed = []
        for row in self._rows:
            renamed.append({new: row.get(old) for old, new in zip(self._columns, new_columns)})
        self._rows = renamed
        self._columns = new_columns

    # -- indexing ---------------------------------------------------------
    def __getitem__(self, key):
        if isinstance(key, str):
            return Series([row.get(key) for row in self._rows], name=key)
        if isinstance(key, (list, tuple)) and all(isinstance(k, str) for k in key):
            columns = list(key)
            rows = [{c: row.get(c) for c in columns} for row in self._rows]
            return DataFrame._from_rows(rows, columns)
        if isinstance(key, Series):
            mask = key.tolist()
            rows = [row for row, keep in zip(self._rows, mask) if keep]
            return DataFrame._from_rows(rows, list(self._columns))
        raise TypeError(f"unsupported indexer: {key!r}")

    def __setitem__(self, key, value):
        if isinstance(value, Series):
            values = value.tolist()
        elif isinstance(value, (list, tuple)):
            values = list(value)
        else:
            values = [value] * len(self._rows)
        if len(values) != len(self._rows):
            raise ValueError("length mismatch assigning column")
        for row, v in zip(self._rows, values):
            row[key] = v
        if key not in self._columns:
            self._columns.append(key)

    def __repr__(self):
        return f"DataFrame({self._rows!r})"

    # -- verbs -------------------------------------------------------------
    def rename(self, columns=None):
        columns = columns or {}
        rows = [
            {columns.get(k, k): v for k, v in row.items()} for row in self._rows
        ]
        new_cols = [columns.get(c, c) for c in self._columns]
        return DataFrame._from_rows(rows, new_cols)

    def sort_values(self, by, ascending=True):
        keys = [by] if isinstance(by, str) else list(by)
        rows = sorted(self._rows, key=lambda r: tuple(r.get(k) for k in keys), reverse=not ascending)
        return DataFrame._from_rows(rows, list(self._columns))

    def drop_duplicates(self, subset=None, keep="first"):
        seen = set()
        rows = []
        source = self._rows if keep == "first" else list(reversed(self._rows))
        for row in source:
            key = tuple(row.get(c) for c in subset) if subset else tuple(sorted(row.items()))
            if key not in seen:
                seen.add(key)
                rows.append(row)
        if keep != "first":
            rows.reverse()
        return DataFrame._from_rows(rows, list(self._columns))

    def fillna(self, value):
        if isinstance(value, dict):
            rows = []
            for row in self._rows:
                new_row = dict(row)
                for col, v in value.items():
                    if _is_missing(new_row.get(col)):
                        new_row[col] = v
                rows.append(new_row)
        else:
            rows = [
                {k: (value if _is_missing(v) else v) for k, v in row.items()}
                for row in self._rows
            ]
        return DataFrame._from_rows(rows, list(self._columns))

    def dropna(self, subset=None):
        cols = subset if subset is not None else self._columns
        rows = [row for row in self._rows if all(not _is_missing(row.get(c)) for c in cols)]
        return DataFrame._from_rows(rows, list(self._columns))

    def ffill(self):
        rows = [dict(row) for row in self._rows]
        last = {c: None for c in self._columns}
        for row in rows:
            for c in self._columns:
                if _is_missing(row.get(c)):
                    row[c] = last[c]
                else:
                    last[c] = row[c]
        return DataFrame._from_rows(rows, list(self._columns))

    def bfill(self):
        rows = [dict(row) for row in self._rows]
        nxt = {c: None for c in self._columns}
        for row in reversed(rows):
            for c in self._columns:
                if _is_missing(row.get(c)):
                    row[c] = nxt[c]
                else:
                    nxt[c] = row[c]
        return DataFrame._from_rows(rows, list(self._columns))

    def astype(self, spec):
        if not isinstance(spec, dict):
            raise NotImplementedError("astype only supports a {column: type} mapping")
        rows = []
        for row in self._rows:
            new_row = dict(row)
            for col, to in spec.items():
                if not _is_missing(new_row.get(col)):
                    new_row[col] = to(new_row[col])
            rows.append(new_row)
        return DataFrame._from_rows(rows, list(self._columns))

    def apply(self, fn, axis=1):
        if axis != 1:
            raise NotImplementedError("apply only supports axis=1 (row-wise)")
        return Series([fn(row) for row in self._rows])

    def map(self, fn):
        """Apply ``fn`` elementwise to every cell (mirrors ``DataFrame.map``,
        the modern replacement for the deprecated ``applymap``). Missing
        values are left as-is, consistent with ``Series.map`` above.
        """
        rows = [
            {k: (fn(v) if not _is_missing(v) else v) for k, v in row.items()}
            for row in self._rows
        ]
        return DataFrame._from_rows(rows, list(self._columns))

    def pop(self, column):
        """Remove ``column`` and return it as a Series, mutating this
        DataFrame in place -- matches real pandas' ``DataFrame.pop``.
        """
        series = Series([row.get(column) for row in self._rows], name=column)
        for row in self._rows:
            row.pop(column, None)
        self._columns.remove(column)
        return series

    def _row_mask(self, cond):
        if isinstance(cond, Series):
            return cond.tolist()
        if callable(cond):
            return [bool(cond(row)) for row in self._rows]
        raise TypeError(f"unsupported condition for where()/mask(): {cond!r}")

    def where(self, cond, other=None):
        """Keep a row's values where ``cond`` is true, else replace the
        whole row with ``other`` (broadcast, or a callable(row) -> value per
        column). ``cond`` is a boolean Series (one value per row, as from a
        column comparison) or a callable(row) -> bool -- this drop-in has no
        per-cell/whole-frame elementwise comparison, unlike real pandas'
        ``df.where(df > 0)``.
        """
        keep = self._row_mask(cond)
        rows = [
            dict(row) if k else {c: other for c in self._columns}
            for row, k in zip(self._rows, keep)
        ]
        return DataFrame._from_rows(rows, list(self._columns))

    def mask(self, cond, other=None):
        """Inverse of ``where``: replace a row with ``other`` where ``cond``
        is true, keep it otherwise.
        """
        keep = self._row_mask(cond)
        rows = [
            {c: other for c in self._columns} if k else dict(row)
            for row, k in zip(self._rows, keep)
        ]
        return DataFrame._from_rows(rows, list(self._columns))

    def assign(self, **kwargs):
        """Return a copy with columns added/overwritten. Each keyword's
        value is either a callable taking this DataFrame (evaluated against
        the frame *as assembled so far*, so later assignments can reference
        earlier ones, as in real pandas) or a scalar/list/Series assigned
        directly.
        """
        result = self.copy()
        for name, value in kwargs.items():
            result[name] = value(result) if callable(value) else value
        return result

    def groupby(self, by, as_index=True):
        return GroupBy(self, by)

    def merge(self, right, on=None, left_on=None, right_on=None, how="inner", suffixes=("_x", "_y")):
        if on is not None:
            left_key = right_key = on
        else:
            left_key, right_key = left_on, right_on

        right_index = {}
        for row in right._rows:
            right_index.setdefault(row.get(right_key), []).append(row)

        left_suffix, right_suffix = suffixes
        shared = (set(self._columns) & set(right._columns)) - {left_key if on else None, right_key if on else None}

        right_value_columns = [c for c in right._columns if not (on is not None and c == right_key)]

        rows = []
        for left_row in self._rows:
            matches = right_index.get(left_row.get(left_key), [])
            if matches:
                for right_row in matches:
                    merged = {}
                    for k, v in left_row.items():
                        merged[f"{k}{left_suffix}" if k in shared else k] = v
                    for k, v in right_row.items():
                        if on is not None and k == right_key:
                            continue
                        merged[f"{k}{right_suffix}" if k in shared else k] = v
                    rows.append(merged)
            elif how == "left":
                merged = dict(left_row)
                for k in right_value_columns:
                    merged[f"{k}{right_suffix}" if k in shared else k] = None
                rows.append(merged)
        return DataFrame._from_rows(rows)

    def head(self, n=5):
        return DataFrame._from_rows(list(self._rows[:n]), list(self._columns))

    def tail(self, n=5):
        return DataFrame._from_rows(list(self._rows[-n:]) if n else [], list(self._columns))

    def melt(self, id_vars=None, value_vars=None, var_name="variable", value_name="value"):
        """Unpivot from wide to long: each ``value_vars`` column becomes a
        row, keyed by ``id_vars``. Mirrors ``pandas.DataFrame.melt``.
        """
        id_vars = [id_vars] if isinstance(id_vars, str) else list(id_vars or [])
        value_vars = (
            [value_vars]
            if isinstance(value_vars, str)
            else list(value_vars) if value_vars is not None
            else [c for c in self._columns if c not in id_vars]
        )
        rows = []
        for row in self._rows:
            base = {k: row.get(k) for k in id_vars}
            for col in value_vars:
                rows.append({**base, var_name: col, value_name: row.get(col)})
        return DataFrame._from_rows(rows, id_vars + [var_name, value_name])

    def pivot(self, index, columns, values):
        """Reshape from long to wide, without aggregation. Mirrors
        ``pandas.DataFrame.pivot``; raises if a given ``(index, columns)``
        pair repeats, same as real pandas.
        """
        column_values = []
        for row in self._rows:
            c = row.get(columns)
            if c not in column_values:
                column_values.append(c)

        table = {}
        index_order = []
        for row in self._rows:
            idx = row.get(index)
            col = row.get(columns)
            if idx not in table:
                table[idx] = {}
                index_order.append(idx)
            if col in table[idx]:
                raise ValueError(
                    f"Index contains duplicate entries for ({index}={idx!r}, {columns}={col!r}); "
                    "cannot reshape with pivot()"
                )
            table[idx][col] = row.get(values)

        rows = []
        for idx in index_order:
            out_row = {index: idx}
            for col in column_values:
                out_row[col] = table[idx].get(col)
            rows.append(out_row)
        return DataFrame._from_rows(rows, [index] + column_values)


def concat(objs, ignore_index=False):
    """Stack DataFrames row-wise, union-ing their columns. Mirrors
    ``pandas.concat`` for the ``axis=0`` (row-wise) case; ``ignore_index``
    is accepted for signature compatibility but has no effect, since this
    drop-in never models a real Index in the first place.
    """
    dfs = list(objs)
    columns = []
    for df in dfs:
        for c in df._columns:
            if c not in columns:
                columns.append(c)
    rows = [{c: row.get(c) for c in columns} for df in dfs for row in df._rows]
    return DataFrame._from_rows(rows, columns)


_AGG_FUNCS = {
    "sum": lambda values: sum(values),
    "mean": lambda values: sum(values) / len(values) if values else None,
    "min": lambda values: min(values) if values else None,
    "max": lambda values: max(values) if values else None,
    "count": lambda values: len(values),
    "nunique": lambda values: len(set(values)),
    "list": lambda values: list(values),
}


class GroupBy:
    """Minimal groupby supporting pandas' named-aggregation ``.agg()`` shorthand.

    No Index is modeled -- results always come back with the group-by
    columns as regular columns (equivalent to real pandas' ``as_index=False``),
    regardless of what ``as_index`` was passed to ``groupby()``.
    """

    def __init__(self, df, by):
        self._df = df
        self._keys = [by] if isinstance(by, str) else list(by)

    def _groups(self):
        rows = sorted(self._df._rows, key=lambda r: tuple(r.get(k) for k in self._keys))
        for key_values, group_iter in _itertools_groupby(
            rows, key=lambda r: tuple(r.get(k) for k in self._keys)
        ):
            yield key_values, list(group_iter)

    def agg(self, **aggregations):
        result_rows = []
        for key_values, group in self._groups():
            out_row = dict(zip(self._keys, key_values))
            for out_name, spec in aggregations.items():
                source_column, agg = spec
                values = [row.get(source_column) for row in group]
                fn = _AGG_FUNCS[agg] if isinstance(agg, str) else agg
                out_row[out_name] = fn(values)
            result_rows.append(out_row)
        return DataFrame._from_rows(result_rows)

    def _reduce_all_columns(self, fn):
        result_rows = []
        for key_values, group in self._groups():
            out_row = dict(zip(self._keys, key_values))
            other_columns = [c for c in self._df._columns if c not in self._keys]
            for c in other_columns:
                values = [row.get(c) for row in group if not _is_missing(row.get(c))]
                try:
                    out_row[c] = fn(values)
                except TypeError:
                    continue
            result_rows.append(out_row)
        return DataFrame._from_rows(result_rows)

    def sum(self):
        return self._reduce_all_columns(sum)

    def mean(self):
        return self._reduce_all_columns(lambda vs: sum(vs) / len(vs) if vs else None)

    def count(self):
        return self._reduce_all_columns(len)

    def size(self):
        rows = [
            {**dict(zip(self._keys, key_values)), "size": len(group)}
            for key_values, group in self._groups()
        ]
        return DataFrame._from_rows(rows)
