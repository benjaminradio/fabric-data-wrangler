"""Example transformation script -- looks like a Fabric notebook cell.

Cut-and-paste unmodified into a Fabric notebook: there, `data` would come
from your own ingestion cell (e.g. a lakehouse table read into a list of
rows/dicts) and `import pandas as pd` resolves to real pandas instead of
this project's pure-Python drop-in. `display()` is likewise already a
Fabric notebook built-in.
"""

import pandas as pd

df = pd.DataFrame(data)
df["units"] = df["units"].fillna(0)
df = df.astype({"units": int})
df["revenue"] = df["units"] * df["unit_price"]

summary = df.groupby(["region", "rep"], as_index=False).agg(
    total_units=("units", "sum"),
    total_revenue=("revenue", "sum"),
)
summary = summary.sort_values("total_revenue", ascending=False)

display(summary)
