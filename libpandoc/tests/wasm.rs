//! Wasm filters (the example filters, built for wasm32-wasip1).
#![cfg(feature = "wasm")]

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use libpandoc::convert_with;
use libpandoc::wasm::WasmFilter;
use serde_json::{json, Value};

/// The example filters' directory of .wasm files, built once.
fn filters() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../filters");
        let status = Command::new(env!("CARGO"))
            .args(["build", "--release", "--target", "wasm32-wasip1", "--quiet"])
            .current_dir(&root)
            .status()
            .expect("cargo");
        assert!(
            status.success(),
            "building the example filters (rustup target add wasm32-wasip1)"
        );
        root.join("target/wasm32-wasip1/release")
    })
}

fn wasm(name: &str) -> WasmFilter {
    WasmFilter::from_file(filters().join(format!("{name}.wasm"))).unwrap()
}

#[test]
fn a_wasm_filter_runs_in_the_conversion() {
    let out = convert_with(
        &json!({"from": "markdown", "to": "plain"}),
        Some(b"hello *world*"),
        vec![wasm("upper").into_filter()],
    )
    .unwrap();
    assert_eq!(out.text(), "HELLO WORLD\n");
}

#[test]
fn it_is_told_the_conversion() {
    let out = convert_with(
        &json!({"from": "commonmark_x", "to": "html5"}),
        Some(b"x"),
        vec![wasm("conversion").into_filter()],
    )
    .unwrap();
    let text = out.text();
    let json_text = text
        .split("<code>")
        .nth(1)
        .unwrap()
        .split("</code>")
        .next()
        .unwrap();
    let told: Value = serde_json::from_str(&json_text.replace("&quot;", "\"")).unwrap();
    assert_eq!(told["format"], "html5");
    assert!(told["input-format"]
        .as_str()
        .unwrap()
        .starts_with("commonmark_x"));
    assert!(told["output-format"].as_str().unwrap().starts_with("html5"));
    assert_eq!(told["reader-options"], true);
    assert_eq!(told["pandoc-version"], libpandoc::pandoc_version().unwrap());
}

#[test]
fn it_sees_only_what_it_is_given() {
    let dir = std::env::temp_dir().join(format!("libpandoc-wasm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("part.txt"), "included").unwrap();
    let input = b"```include\npart.txt\n```\n";
    let opts = json!({"from": "markdown", "to": "plain"});
    // the directory given, as the filter's current directory
    let f = wasm("include").no_files().preopen(&dir, ".", false);
    let out = convert_with(&opts, Some(input), vec![f.into_filter()]).unwrap();
    assert_eq!(out.text().trim(), "included");
    // no directory: the file can't be read, and the filter's failure fails the conversion
    let f = wasm("include").no_files();
    let e = convert_with(&opts, Some(input), vec![f.into_filter()]).unwrap_err();
    assert_eq!(e.kind, "PandocFilterError");
    assert!(e.message.contains("exited with status 3"), "{}", e.message);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn not_a_wasm_file() {
    assert!(WasmFilter::from_bytes("x", b"not wasm").is_err());
}
