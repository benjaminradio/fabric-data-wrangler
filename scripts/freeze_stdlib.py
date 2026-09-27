"""Compile a CPython stdlib tree's pure-Python modules to marshalled
bytecode and pack them into a single blob + manifest, for embedding into
the `dw` binary as frozen modules (see crates/cli/build.rs and
crates/cli/src/python_runtime.rs).

Must be run with the *same* Python interpreter/version whose libpython is
being statically linked into the binary -- marshalled code objects are not
portable across CPython versions, and frozen modules are unmarshalled
directly (no .pyc magic-number check to catch a mismatch).

Usage: python3 freeze_stdlib.py <stdlib_root> <output_dir>

Writes <output_dir>/frozen_stdlib.bin (concatenated marshalled code objects)
and <output_dir>/frozen_stdlib_manifest.tsv (one "name\toffset\tsize\tis_package"
line per module).
"""

import marshal
import sys
from pathlib import Path

# Directories that aren't part of the importable stdlib surface, or that
# are large and not needed by this project's transformation scripts.
EXCLUDED_DIR_NAMES = {
    "test",
    "tests",
    "idle_test",
    "lib2to3",
    "__pycache__",
    "site-packages",
}


def iter_stdlib_modules(root: Path):
    for py_file in sorted(root.rglob("*.py")):
        rel = py_file.relative_to(root)
        if EXCLUDED_DIR_NAMES & set(rel.parts[:-1]):
            continue

        parts = list(rel.parts)
        is_package = parts[-1] == "__init__.py"
        if is_package:
            parts = parts[:-1]
            if not parts:
                continue
        else:
            parts[-1] = parts[-1][: -len(".py")]

        module_name = ".".join(parts)
        yield module_name, py_file, is_package


def main():
    stdlib_root = Path(sys.argv[1])
    output_dir = Path(sys.argv[2])
    output_dir.mkdir(parents=True, exist_ok=True)

    blob_path = output_dir / "frozen_stdlib.bin"
    manifest_path = output_dir / "frozen_stdlib_manifest.tsv"

    offset = 0
    n_ok = 0
    n_failed = 0
    with blob_path.open("wb") as blob_f, manifest_path.open("w") as manifest_f:
        for module_name, py_file, is_package in iter_stdlib_modules(stdlib_root):
            source = py_file.read_bytes()
            try:
                code = compile(source, f"<frozen {module_name}>", "exec")
            except SyntaxError as exc:
                print(f"warning: skipping {module_name} ({py_file}): {exc}", file=sys.stderr)
                n_failed += 1
                continue

            data = marshal.dumps(code)
            blob_f.write(data)
            manifest_f.write(f"{module_name}\t{offset}\t{len(data)}\t{int(is_package)}\n")
            offset += len(data)
            n_ok += 1

    print(f"froze {n_ok} modules ({offset} bytes), skipped {n_failed}", file=sys.stderr)


if __name__ == "__main__":
    main()
