//! Generates the Rust glue (a byte blob plus an entry table) that
//! `python_runtime.rs` uses to register the pure-Python standard library as
//! CPython "frozen modules" -- compiled directly into the binary, needing
//! no file on disk at run time. See `scripts/freeze_stdlib.py`, which
//! produces the blob/manifest this reads. A no-op (empty tables) for a
//! local dev build, where `DW_FROZEN_STDLIB_DIR` isn't set; only
//! `.github/workflows/build.yml` sets it.

use std::env;
use std::fmt::Write as _;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-env-changed=DW_FROZEN_STDLIB_DIR");

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    let dest = Path::new(&out_dir).join("frozen_stdlib_generated.rs");

    let Ok(dir) = env::var("DW_FROZEN_STDLIB_DIR") else {
        std::fs::write(
            &dest,
            "pub static FROZEN_STDLIB_BLOB: &[u8] = &[];\n\
             pub static FROZEN_STDLIB_ENTRIES: &[(&str, usize, usize, bool)] = &[];\n",
        )
        .expect("writing empty frozen_stdlib_generated.rs");
        return;
    };

    let blob_path = Path::new(&dir).join("frozen_stdlib.bin");
    let manifest_path = Path::new(&dir).join("frozen_stdlib_manifest.tsv");
    println!("cargo:rerun-if-changed={}", blob_path.display());
    println!("cargo:rerun-if-changed={}", manifest_path.display());

    let manifest = std::fs::read_to_string(&manifest_path).unwrap_or_else(|e| {
        panic!(
            "DW_FROZEN_STDLIB_DIR set but couldn't read {}: {e}",
            manifest_path.display()
        )
    });

    let blob_path_str = blob_path
        .canonicalize()
        .unwrap_or(blob_path.clone())
        .display()
        .to_string();

    let mut out = String::new();
    let _ = writeln!(out, "pub static FROZEN_STDLIB_BLOB: &[u8] = include_bytes!(r#\"{blob_path_str}\"#);");
    let _ = writeln!(
        out,
        "pub static FROZEN_STDLIB_ENTRIES: &[(&str, usize, usize, bool)] = &["
    );
    for (lineno, line) in manifest.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
        let (Some(name), Some(offset), Some(size), Some(is_package)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            panic!(
                "{}:{}: malformed manifest line: {line:?}",
                manifest_path.display(),
                lineno + 1
            );
        };
        let offset: usize = offset.parse().expect("manifest offset is an integer");
        let size: usize = size.parse().expect("manifest size is an integer");
        let is_package = is_package == "1";
        let _ = writeln!(out, "    ({name:?}, {offset}, {size}, {is_package}),");
    }
    let _ = writeln!(out, "];");

    std::fs::write(&dest, out).expect("writing frozen_stdlib_generated.rs");
}
