//! Finds libpandoc and links to it.
//!
//! libpandoc (the shared library, and on Windows `bin/pandoc.dll`) is looked
//! for, in order:
//!
//! 1. in `$LIBPANDOC_PREFIX`, then `$PREFIX` and `$CONDA_PREFIX` (conda
//!    builds and environments): the same places as libpandoc-python. A
//!    prefix is a directory with `lib/libpandoc.so` (`.dylib`), such as an
//!    unpacked release tarball (https://github.com/ickc/libpandoc/releases)
//!    or a conda environment;
//! 2. through pkg-config (`libpandoc.pc`, in the release and the conda
//!    package);
//! 3. with the `download` feature (on by default in pandocrs), the
//!    release's build for this target, downloaded once into
//!    `$LIBPANDOC_DOWNLOAD_DIR` (default: the user's data directory, e.g.
//!    `~/.local/share/libpandoc`), where programs built with it find it
//!    afterwards: not the build directory, which `cargo install` deletes.
//!
//! So that programs find the library when they run, without
//! `LD_LIBRARY_PATH`, the directory is passed on to dependents as an rpath
//! (`DEP_PANDOC_RPATH`), which their build scripts give the linker (see
//! the `libpandoc` crate's). `$LIBPANDOC_RPATH` overrides it: e.g.
//! `$ORIGIN/../lib` for a relocatable install (a program in `bin/`, the
//! library in `lib/`, as in a conda package; `$ORIGIN` is `@loader_path`
//! on macOS), or empty for none. libpandoc-python's setup.py reads it the
//! same way.
//!
//! On docs.rs (no library, no network) nothing is linked.

use std::env;
use std::path::{Path, PathBuf};

const VARS: [&str; 3] = ["LIBPANDOC_PREFIX", "PREFIX", "CONDA_PREFIX"];

fn main() {
    for var in VARS.iter().chain(&[
        "LIBPANDOC_RPATH",
        "LIBPANDOC_DOWNLOAD_DIR",
        "LIBPANDOC_DOWNLOAD_URL",
        "PKG_CONFIG_PATH",
        "DOCS_RS",
    ]) {
        println!("cargo:rerun-if-env-changed={var}");
    }
    if env::var_os("DOCS_RS").is_some() {
        return;
    }
    let windows = env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let prefix = find_prefix(windows)
        .or_else(|| from_pkg_config(windows))
        .or_else(|| download(windows));
    let Some(prefix) = prefix else {
        panic!(
            "libpandoc not found. Set LIBPANDOC_PREFIX to a directory with \
             lib/libpandoc.so (.dylib; bin/pandoc.dll on Windows), such as an \
             unpacked release from https://github.com/ickc/libpandoc/releases, \
             build in a conda environment that has libpandoc, or enable the \
             `download` feature. Looked in: {}, and pkg-config",
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

fn from_pkg_config(windows: bool) -> Option<PathBuf> {
    if windows {
        return None;
    }
    let prefix = PathBuf::from(pkg_config::get_variable("libpandoc", "prefix").ok()?);
    // `${pcfiledir}/../..`, resolved
    let prefix = prefix.canonicalize().unwrap_or(prefix);
    has_library(&prefix, windows).then_some(prefix)
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

#[cfg(not(feature = "download"))]
fn download(_windows: bool) -> Option<PathBuf> {
    None
}

/// The release the `download` feature fetches, and the SHA-256 of each
/// target's tarball in it. `continuous` (the latest build of libpandoc's
/// main) has none to pin: they are checked once there is a tagged release.
#[cfg(feature = "download")]
const RELEASE: &str = "continuous";
#[cfg(feature = "download")]
const SHA256: &[(&str, &str)] = &[];

/// Download and unpack the release's build for this target, unless it is
/// there already; its prefix.
#[cfg(feature = "download")]
fn download(windows: bool) -> Option<PathBuf> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let subdir = match (os.as_str(), arch.as_str()) {
        ("linux", "x86_64") => "linux-64",
        ("linux", "aarch64") => "linux-aarch64",
        ("macos", "x86_64") => "osx-64",
        ("macos", "aarch64") => "osx-arm64",
        ("windows", "x86_64") => "win-64",
        _ => panic!("libpandoc has no release build for {arch}-{os}"),
    };
    let root = env::var_os("LIBPANDOC_DOWNLOAD_DIR")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| data_dir(windows).map(|d| d.join("libpandoc")))
        .expect("no data directory: set LIBPANDOC_DOWNLOAD_DIR");
    let prefix = root.join(RELEASE).join(subdir);
    if has_library(&prefix, windows) {
        return Some(prefix);
    }
    let url = env::var("LIBPANDOC_DOWNLOAD_URL").unwrap_or_else(|_| {
        format!(
            "https://github.com/ickc/libpandoc/releases/download/{RELEASE}/libpandoc-{subdir}.tar.gz"
        )
    });
    println!(
        "cargo:warning=downloading libpandoc ({RELEASE}, {subdir}) into {}",
        prefix.display()
    );
    // a local file (for mirrors and offline builds), or a URL
    let bytes = if Path::new(&url).is_file() {
        std::fs::read(&url).unwrap_or_else(|e| panic!("{url}: {e}"))
    } else {
        let mut bytes = Vec::new();
        ureq::get(&url)
            .call()
            .unwrap_or_else(|e| panic!("downloading {url}: {e}"))
            .into_body()
            .into_reader()
            .read_to_end(&mut bytes)
            .unwrap_or_else(|e| panic!("downloading {url}: {e}"));
        bytes
    };
    let digest: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    match SHA256.iter().find(|(s, _)| *s == subdir) {
        Some((_, want)) if *want != digest => {
            panic!("{url}: SHA-256 {digest}, expected {want}")
        }
        Some(_) => {}
        None => println!(
            "cargo:warning=libpandoc {RELEASE} is a moving build: its SHA-256 ({digest}) isn't pinned"
        ),
    }
    // unpacked beside, then moved into place: never half there
    std::fs::create_dir_all(&root).unwrap_or_else(|e| panic!("{}: {e}", root.display()));
    std::fs::create_dir_all(prefix.parent().unwrap()).unwrap();
    let tmp = prefix.with_extension(format!("tmp{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    tar::Archive::new(flate2::read::GzDecoder::new(&bytes[..]))
        .unpack(&tmp)
        .unwrap_or_else(|e| panic!("unpacking {url}: {e}"));
    if std::fs::rename(&tmp, &prefix).is_err() {
        // another build got there first
        let _ = std::fs::remove_dir_all(&tmp);
    }
    if windows {
        println!(
            "cargo:warning=add {} to PATH to run programs linked with libpandoc",
            prefix.join("bin").display()
        );
    }
    has_library(&prefix, windows).then_some(prefix)
}

/// Where per-user data goes: `$XDG_DATA_HOME` or `~/.local/share`,
/// `~/Library/Application Support` on macOS, `%LOCALAPPDATA%` on Windows.
#[cfg(feature = "download")]
fn data_dir(windows: bool) -> Option<PathBuf> {
    let var = |k| env::var_os(k).filter(|s| !s.is_empty()).map(PathBuf::from);
    if windows {
        return var("LOCALAPPDATA");
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        return var("HOME").map(|h| h.join("Library/Application Support"));
    }
    var("XDG_DATA_HOME").or_else(|| var("HOME").map(|h| h.join(".local/share")))
}
