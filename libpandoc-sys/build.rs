//! Finds libpandoc and links to it.
//!
//! libpandoc (the shared library, and on Windows `bin/pandoc.dll`) is looked
//! for in `$LIBPANDOC_PREFIX`, then `$PREFIX` and `$CONDA_PREFIX` (conda
//! builds and environments): the same places as libpandoc-python. A prefix
//! is a directory with `lib/libpandoc.so` (`.dylib`), such as an unpacked
//! release tarball (https://github.com/ickc/libpandoc/releases) or a conda
//! environment.
//!
//! So that programs find the library when they run, without
//! `LD_LIBRARY_PATH`, the directory is passed on to dependents as an rpath
//! (`DEP_PANDOC_RPATH`), which their build scripts give the linker (see
//! the `libpandoc` crate's). `$LIBPANDOC_RPATH` overrides it: e.g.
//! `$ORIGIN/../lib` for a relocatable install (a program in `bin/`, the
//! library in `lib/`, as in a conda package; `$ORIGIN` is `@loader_path`
//! on macOS), or empty for none. libpandoc-python's setup.py reads it the
//! same way.

use std::env;
use std::path::{Path, PathBuf};

const VARS: [&str; 3] = ["LIBPANDOC_PREFIX", "PREFIX", "CONDA_PREFIX"];

fn main() {
    for var in VARS.iter().chain(&["LIBPANDOC_RPATH"]) {
        println!("cargo:rerun-if-env-changed={var}");
    }
    let windows = env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let Some(prefix) = find_prefix(windows) else {
        panic!(
            "libpandoc not found. Set LIBPANDOC_PREFIX to a directory with \
             lib/libpandoc.so (.dylib; bin/pandoc.dll on Windows), such as an \
             unpacked release from https://github.com/ickc/libpandoc/releases, \
             or build in a conda environment that has libpandoc. Looked in: {}",
            VARS.join(", ")
        );
    };
    // Windows links with raw-dylib (src/lib.rs): no import library needed,
    // the DLL is found at run time on PATH or next to the program.
    let lib = prefix.join(if windows { "bin" } else { "lib" });
    println!("cargo:rustc-link-search=native={}", lib.display());
    if !windows {
        println!("cargo:rustc-link-lib=dylib=pandoc");
    }
    let rpath = match env::var("LIBPANDOC_RPATH") {
        Ok(r) if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") => {
            r.replace("$ORIGIN", "@loader_path")
        }
        Ok(r) => r,
        Err(_) => lib.display().to_string(),
    };
    // DEP_PANDOC_ROOT, DEP_PANDOC_LIB_DIR, DEP_PANDOC_RPATH for dependents
    println!("cargo:root={}", prefix.display());
    println!("cargo:lib_dir={}", lib.display());
    println!("cargo:rpath={rpath}");
    if !windows && !rpath.is_empty() {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{rpath}");
    }
}

fn find_prefix(windows: bool) -> Option<PathBuf> {
    VARS.iter()
        .filter_map(|v| env::var_os(v).filter(|s| !s.is_empty()))
        .map(PathBuf::from)
        .flat_map(|p| {
            // conda on Windows puts libraries under Library/
            let library = p.join("Library");
            [p, library]
        })
        .find(|p| has_library(p, windows))
}

fn has_library(prefix: &Path, windows: bool) -> bool {
    if windows {
        prefix.join("bin/pandoc.dll").is_file()
    } else {
        ["so", "dylib"]
            .iter()
            .any(|ext| prefix.join(format!("lib/libpandoc.{ext}")).is_file())
    }
}
