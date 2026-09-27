//! Statically-linked python-build-standalone distributions compile several
//! stdlib extension modules (`_bz2`, `_ctypes`, `_ssl`, ...) directly into
//! `libpython*.a` rather than as separate `.so`s, but those modules still
//! depend on *their* own native libraries (`libbz2.a`, `libffi.a`, ...),
//! which pyo3-build-config's own build script doesn't know to link. CI
//! (`.github/workflows/build.yml`) discovers whichever such libraries ship
//! alongside the chosen distribution and passes them here via env vars;
//! a local dev build against the system's dynamically-linked libpython
//! doesn't need any of this, so it's a no-op when the vars are unset.
fn main() {
    if let Ok(dirs) = std::env::var("DW_EXTRA_STATIC_LIB_DIRS") {
        for dir in dirs.lines().map(str::trim).filter(|s| !s.is_empty()) {
            println!("cargo:rustc-link-search=native={dir}");
        }
    }
    if let Ok(libs) = std::env::var("DW_EXTRA_STATIC_LIBS") {
        for lib in libs.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            println!("cargo:rustc-link-lib=static={lib}");
        }
    }
}
