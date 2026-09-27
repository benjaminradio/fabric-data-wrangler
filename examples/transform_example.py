"""Example transformation script.

Runs unmodified against the local `wrangle` drop-in and, later, a Fabric
notebook's pandas/PySpark-backed `wrangle` adapter -- only the import
resolution differs, never this code.
"""

import wrangle


def transform(records):
    rows = wrangle.fillna(records, 0, columns=["units"])
    rows = wrangle.cast(rows, "units", int)
    rows = wrangle.mutate(rows, revenue=lambda r: r["units"] * r["unit_price"])

    summary = wrangle.group_by(
        rows,
        by=["region", "rep"],
        total_units=("units", "sum"),
        total_revenue=("revenue", "sum"),
    )
    return wrangle.sort_by(summary, "total_revenue", reverse=True)
