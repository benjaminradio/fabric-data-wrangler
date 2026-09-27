"""Portability shim: same verb API, backend picked by environment.

Local CLI runs on the pure-Python ``_local`` implementation (plain
``list[dict]`` records, zero third-party dependencies). In a Microsoft
Fabric notebook, the same import resolves to ``_fabric`` instead, which is
backed by pandas/PySpark. Transformation scripts import only from
``wrangle`` and never know which backend they're running on -- that is the
entire portability contract.

Backend selection:
  1. ``WRANGLE_BACKEND=local`` or ``WRANGLE_BACKEND=fabric`` env var, if set,
     wins outright (useful for testing the fabric adapter without Fabric).
  2. Otherwise, autodetect: if a Fabric/Spark environment is importable
     (``notebookutils`` or ``pyspark``), use the fabric backend; else local.
"""

import os


def _detect_backend() -> str:
    forced = os.environ.get("WRANGLE_BACKEND")
    if forced in ("local", "fabric"):
        return forced

    try:
        import notebookutils  # noqa: F401

        return "fabric"
    except ImportError:
        pass

    try:
        import pyspark  # noqa: F401

        return "fabric"
    except ImportError:
        pass

    return "local"


_BACKEND = _detect_backend()

if _BACKEND == "fabric":
    from wrangle._fabric import *  # noqa: F401,F403
    from wrangle._fabric import __all__  # noqa: F401
else:
    from wrangle._local import *  # noqa: F401,F403
    from wrangle._local import __all__  # noqa: F401

BACKEND = _BACKEND
