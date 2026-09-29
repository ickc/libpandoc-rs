// Find libpandoc at run time where libpandoc-sys found it at build time (or
// $LIBPANDOC_RPATH, e.g. $ORIGIN/../lib for a relocatable install).
fn main() {
    println!("cargo:rerun-if-env-changed=DEP_PANDOC_RPATH");
    if let Ok(rpath) = std::env::var("DEP_PANDOC_RPATH") {
        if !rpath.is_empty() && std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{rpath}");
        }
    }
}
