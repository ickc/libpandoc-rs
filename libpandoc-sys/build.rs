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
//!    When the release has a `SHA256SUMS` (libpandoc 1.12's on), each
//!    download is checked against it, and a mismatch or a file it doesn't
//!    list fails the build; without one, the build only warns.
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
//! With the `wasm` feature (the `libpandoc` crate's, for conversions in a
//! directory sandbox), libpandoc.wasm too, the same for every target:
//! `$LIBPANDOC_WASM` if set at build time, else the one in the download
//! directory (`continuous/libpandoc.wasm`), else, with `download`, the
//! release's, downloaded there once. Its path goes to dependents as
//! `DEP_PANDOC_WASM` (the `libpandoc` crate builds it in; `$LIBPANDOC_WASM`
//! still overrides it at run time).
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
        "LIBPANDOC_WASM",
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
    if env::var_os("CARGO_FEATURE_WASM").is_some() {
        match find_wasm(windows) {
            // DEP_PANDOC_WASM for dependents
            Some(wasm) => println!("cargo:wasm={}", wasm.display()),
            None => println!(
                "cargo:warning=libpandoc.wasm not found: set LIBPANDOC_WASM (at build or run \
                 time) or enable the `download` feature, for conversions in a sandbox"
            ),
        }
    }
}

/// libpandoc.wasm: `$LIBPANDOC_WASM`, else the download directory's, else
/// (with `download`) downloaded there.
fn find_wasm(windows: bool) -> Option<PathBuf> {
    if let Some(p) = env::var_os("LIBPANDOC_WASM").filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(p)).filter(|p| p.is_file());
    }
    let path = download_root(windows)?.join(RELEASE).join("libpandoc.wasm");
    if path.is_file() {
        return Some(path);
    }
    download_wasm(&path)
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

/// The release the `download` feature fetches, and the SHA-256 of each file
/// in it (a target's tarball, libpandoc.wasm) pinned here. `continuous` (the
/// latest build of libpandoc's main) has none to pin: they are pinned once
/// there is a tagged release. Meanwhile the release's own `SHA256SUMS`, if
/// it has one, is checked: it shows a file arrived whole, not that it is the
/// one meant (it comes from the same place, which anyone who can publish
/// there can change).
const RELEASE: &str = "continuous";
#[cfg(feature = "download")]
const SHA256: &[(&str, &str)] = &[];

/// Where downloads go: `$LIBPANDOC_DOWNLOAD_DIR`, else the user's data
/// directory's `libpandoc`.
fn download_root(windows: bool) -> Option<PathBuf> {
    env::var_os("LIBPANDOC_DOWNLOAD_DIR")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| data_dir(windows).map(|d| d.join("libpandoc")))
}

/// Download and unpack the release's build for this target, unless it is
/// there already; its prefix.
#[cfg(feature = "download")]
fn download(windows: bool) -> Option<PathBuf> {
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
    let root = download_root(windows).expect("no data directory: set LIBPANDOC_DOWNLOAD_DIR");
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
    let bytes = fetch(&url).unwrap_or_else(|| panic!("downloading {url}: not found"));
    check(&url, &bytes);
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

/// The bytes at `url`: a local file (for mirrors and offline builds), or a
/// URL; `None` if there is none (no such file, or HTTP's 404). Any other
/// failure fails the build.
#[cfg(feature = "download")]
fn fetch(url: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    if !url.contains("://") {
        return match std::fs::read(url) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => panic!("{url}: {e}"),
        };
    }
    let mut bytes = Vec::new();
    match ureq::get(url).call() {
        Ok(r) => r.into_body().into_reader().read_to_end(&mut bytes),
        Err(ureq::Error::StatusCode(404)) => return None,
        Err(e) => panic!("downloading {url}: {e}"),
    }
    .unwrap_or_else(|e| panic!("downloading {url}: {e}"));
    Some(bytes)
}

/// Check the bytes downloaded from `url` against the SHA-256 pinned for its
/// file name, else against the release's `SHA256SUMS` beside it (`sha256sum`'s
/// lines), else only say that nothing checks them.
#[cfg(feature = "download")]
fn check(url: &str, bytes: &[u8]) {
    use sha2::{Digest, Sha256};
    let digest: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let (sums_url, name) = match url.rsplit_once(['/', '\\']) {
        Some((dir, name)) => (format!("{dir}/SHA256SUMS"), name),
        None => ("SHA256SUMS".into(), url),
    };
    if let Some((_, want)) = SHA256.iter().find(|(n, _)| *n == name) {
        assert!(*want == digest, "{url}: SHA-256 {digest}, expected {want}");
        return;
    }
    let Some(sums) = fetch(&sums_url) else {
        println!(
            "cargo:warning=libpandoc {RELEASE} has no SHA256SUMS: {name} (SHA-256 {digest}) isn't checked"
        );
        return;
    };
    let sums = String::from_utf8_lossy(&sums);
    let want = sums.lines().find_map(|l| {
        let (sum, file) = l.split_once(char::is_whitespace)?;
        (file.trim_start().trim_start_matches('*') == name).then_some(sum)
    });
    match want {
        Some(want) if want.eq_ignore_ascii_case(&digest) => {}
        Some(want) => panic!(
            "{url}: SHA-256 {digest}, but {sums_url} says {want} (the release may be being \
             replaced: try again in a few minutes)"
        ),
        None => panic!("{url}: {name} isn't in {sums_url}"),
    }
}

#[cfg(not(feature = "download"))]
fn download_wasm(_path: &Path) -> Option<PathBuf> {
    None
}

/// Download the release's libpandoc.wasm to `path`.
#[cfg(feature = "download")]
fn download_wasm(path: &Path) -> Option<PathBuf> {
    let url =
        format!("https://github.com/ickc/libpandoc/releases/download/{RELEASE}/libpandoc.wasm");
    println!(
        "cargo:warning=downloading libpandoc.wasm ({RELEASE}, ~60 MB) into {}",
        path.display()
    );
    let bytes = fetch(&url).unwrap_or_else(|| panic!("downloading {url}: not found"));
    check(&url, &bytes);
    // written beside, then moved into place: never half there
    std::fs::create_dir_all(path.parent()?).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, &bytes).unwrap_or_else(|e| panic!("{}: {e}", tmp.display()));
    if std::fs::rename(&tmp, path).is_err() {
        // another build got there first
        let _ = std::fs::remove_file(&tmp);
    }
    Some(path.to_owned()).filter(|p| p.is_file())
}

/// Where per-user data goes: `$XDG_DATA_HOME` or `~/.local/share`,
/// `~/Library/Application Support` on macOS, `%LOCALAPPDATA%` on Windows.
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
