"""pandas-backed adapter with the exact same verb signatures as ``_local``.

Transformation scripts call ``wrangle.select(rows, [...])`` etc without
knowing which backend is active. This module accepts and returns the same
``list[dict]`` shape as ``_local`` -- it just uses pandas internally, so a
Fabric notebook gets a real DataFrame's performance without the script
changing at all.

This is the "same code, adapter shim" portability strategy: only the
import target (``wrangle`` resolving here instead of to ``_local``)
differs between local CLI and Fabric.
"""

import pandas as pd

__all__ = [
    "select",
    "rename",
    "filter_rows",
    "sort_by",
    "mutate",
    "group_by",
    "join",
    "dedupe",
    "fillna",
    "cast",
    "head",
    "tail",
]


def _to_records(df):
    return df.where(pd.notnull(df), None).to_dict(orient="records")


def select(records, columns):
    df = pd.DataFrame(records)
    return _to_records(df[columns])


def rename(records, mapping):
    df = pd.DataFrame(records)
    return _to_records(df.rename(columns=mapping))


def filter_rows(records, predicate):
    return [row for row in records if predicate(row)]


def sort_by(records, by, reverse=False):
    df = pd.DataFrame(records)
    return _to_records(df.sort_values(by=by, ascending=not reverse))


def mutate(records, **computations):
    result = []
    for row in records:
        new_row = dict(row)
        for name, fn in computations.items():
            new_row[name] = fn(new_row)
        result.append(new_row)
    return result


_AGG_NAME = {
    "sum": "sum",
    "mean": "mean",
    "min": "min",
    "max": "max",
    "count": "count",
    "list": lambda s: list(s),
}


def group_by(records, by, **aggregations):
    df = pd.DataFrame(records)
    keys = [by] if isinstance(by, str) else list(by)
    grouped = df.groupby(keys, dropna=False)

    out = grouped.size().reset_index()[keys]
    for out_name, (source_column, agg) in aggregations.items():
        fn = _AGG_NAME[agg] if isinstance(agg, str) else agg
        out[out_name] = grouped[source_column].agg(fn).reset_index(drop=True)
    return _to_records(out)


def join(left, right, on, how="inner"):
    left_df = pd.DataFrame(left)
    right_df = pd.DataFrame(right)
    if isinstance(on, tuple):
        left_key, right_key = on
        merged = left_df.merge(
            right_df, left_on=left_key, right_on=right_key, how=how, suffixes=("", "_right")
        )
        if right_key != left_key and right_key in merged.columns:
            merged = merged.drop(columns=[right_key])
    else:
        merged = left_df.merge(right_df, on=on, how=how, suffixes=("", "_right"))
    return _to_records(merged)


def dedupe(records, subset=None):
    df = pd.DataFrame(records)
    return _to_records(df.drop_duplicates(subset=subset))


def fillna(records, value, columns=None):
    df = pd.DataFrame(records)
    if columns is not None:
        df[columns] = df[columns].fillna(value)
    else:
        df = df.fillna(value)
    return _to_records(df)


def cast(records, column, to):
    df = pd.DataFrame(records)
    df[column] = df[column].map(lambda v: to(v) if v is not None and pd.notnull(v) else v)
    return _to_records(df)


def head(records, n=5):
    return list(records[:n])


def tail(records, n=5):
    return list(records[-n:]) if n else []
