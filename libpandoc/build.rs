// libpandoc-sys found the library; give this crate's tests and examples its
// rpath, so they run without LD_LIBRARY_PATH. A program using libpandoc does
// the same in its own build script (Cargo passes link arguments only to the
// package that asks):
//
//     if let Ok(rpath) = std::env::var("DEP_PANDOC_RPATH") { ... }
//
// which needs `libpandoc-sys` as a dependency (for `links`); see pandocrs.
fn main() {
    println!("cargo:rerun-if-env-changed=DEP_PANDOC_RPATH");
    if let Ok(rpath) = std::env::var("DEP_PANDOC_RPATH") {
        if !rpath.is_empty() && std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{rpath}");
        }
    }
}
