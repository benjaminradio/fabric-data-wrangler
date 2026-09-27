"""Pure-Python dataframe-verb drop-in.

Canonical shape: row-oriented ``list[dict]``. Every verb is a plain
function over that shape -- no classes, no inheritance, no third-party
dependencies (not even the stdlib beyond ``itertools``/``operator``). This
is what makes it loadable purely from memory: it's just Python source.

Verbs are added incrementally as real transformation scripts need them; this
is not meant to be a complete dataframe API.
"""

from itertools import groupby as _itertools_groupby
from operator import itemgetter as _itemgetter

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

Record = dict


def select(records, columns):
    """Keep only the given columns, in the given order."""
    return [{c: row.get(c) for c in columns} for row in records]


def rename(records, mapping):
    """Rename columns per ``{old_name: new_name}``. Unlisted columns pass through."""
    return [
        {mapping.get(k, k): v for k, v in row.items()}
        for row in records
    ]


def filter_rows(records, predicate):
    """Keep rows where ``predicate(row)`` is truthy."""
    return [row for row in records if predicate(row)]


def sort_by(records, by, reverse=False):
    """Sort by one column name or a list of column names."""
    keys = [by] if isinstance(by, str) else list(by)
    return sorted(records, key=_itemgetter(*keys), reverse=reverse)


def mutate(records, **computations):
    """Add or overwrite columns.

    Each keyword's value is a callable taking the row (a dict) and
    returning the new column value, e.g. ``mutate(rows, total=lambda r: r["a"] + r["b"])``.
    """
    result = []
    for row in records:
        new_row = dict(row)
        for name, fn in computations.items():
            new_row[name] = fn(new_row)
        result.append(new_row)
    return result


_AGG_FUNCS = {
    "sum": lambda values: sum(values),
    "mean": lambda values: sum(values) / len(values) if values else None,
    "min": lambda values: min(values) if values else None,
    "max": lambda values: max(values) if values else None,
    "count": lambda values: len(values),
    "list": lambda values: list(values),
}


def group_by(records, by, **aggregations):
    """Group by one column name or a list of column names, then aggregate.

    Each keyword's value is ``(source_column, agg)`` where ``agg`` is one of
    ``"sum"``, ``"mean"``, ``"min"``, ``"max"``, ``"count"``, ``"list"``, or
    a callable taking the list of values in the group.

    Example::

        group_by(rows, by="region", total=("sales", "sum"), n=("sales", "count"))
    """
    keys = [by] if isinstance(by, str) else list(by)
    sorted_records = sorted(records, key=_itemgetter(*keys))

    result = []
    for key_values, group_iter in _itertools_groupby(
        sorted_records, key=_itemgetter(*keys)
    ):
        group = list(group_iter)
        out_row = {}
        if len(keys) == 1:
            out_row[keys[0]] = key_values
        else:
            for k, v in zip(keys, key_values):
                out_row[k] = v
        for out_name, (source_column, agg) in aggregations.items():
            values = [row.get(source_column) for row in group]
            fn = _AGG_FUNCS[agg] if isinstance(agg, str) else agg
            out_row[out_name] = fn(values)
        result.append(out_row)
    return result


def join(left, right, on, how="inner"):
    """Join two record lists on a shared column name (or ``(left_key, right_key)``).

    ``how`` is ``"inner"`` or ``"left"``. Matching columns from the right
    side are merged in; on conflict the right side wins except for the join
    key itself.
    """
    if isinstance(on, tuple):
        left_key, right_key = on
    else:
        left_key = right_key = on

    right_index = {}
    for row in right:
        right_index.setdefault(row.get(right_key), []).append(row)

    result = []
    for left_row in left:
        matches = right_index.get(left_row.get(left_key), [])
        if matches:
            for right_row in matches:
                merged = dict(left_row)
                for k, v in right_row.items():
                    if k == right_key:
                        continue
                    merged[k] = v
                result.append(merged)
        elif how == "left":
            result.append(dict(left_row))
    return result


def dedupe(records, subset=None):
    """Drop duplicate rows, keeping the first occurrence.

    ``subset`` restricts the uniqueness check to those columns; defaults to
    the whole row.
    """
    seen = set()
    result = []
    for row in records:
        key = tuple(row.get(c) for c in subset) if subset else tuple(sorted(row.items()))
        if key not in seen:
            seen.add(key)
            result.append(row)
    return result


def fillna(records, value, columns=None):
    """Replace ``None`` values with ``value``, in the given columns (or all)."""
    result = []
    for row in records:
        new_row = dict(row)
        target_columns = columns if columns is not None else new_row.keys()
        for c in target_columns:
            if new_row.get(c) is None:
                new_row[c] = value
        result.append(new_row)
    return result


def cast(records, column, to):
    """Cast a column's values with ``to`` (e.g. ``int``, ``float``, ``str``), skipping ``None``."""
    result = []
    for row in records:
        new_row = dict(row)
        if new_row.get(column) is not None:
            new_row[column] = to(new_row[column])
        result.append(new_row)
    return result


def head(records, n=5):
    return list(records[:n])


def tail(records, n=5):
    return list(records[-n:]) if n else []
