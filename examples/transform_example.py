"""Example transformation script -- looks like a Fabric notebook cell.

`import littlepandas as pd` and `pd.read_input()` are the two local-only
lines: porting to a real Fabric notebook means changing the import to
`import pandas as pd` and replacing `read_input()` with a real ingestion
call (a lakehouse table read, `pd.read_csv`, ...). `display()` is already
a Fabric notebook built-in, so that line needs no change either way.
"""

import littlepandas as pd

df = pd.read_input()
df["units"] = df["units"].fillna(0)
df = df.astype({"units": int})
df["revenue"] = df["units"] * df["unit_price"]

summary = df.groupby(["region", "rep"], as_index=False).agg(
    total_units=("units", "sum"),
    total_revenue=("revenue", "sum"),
)
summary = summary.sort_values("total_revenue", ascending=False)

display(summary)
