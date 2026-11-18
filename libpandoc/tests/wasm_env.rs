//! What a wasm filter sees of this process's environment. One test in its
//! own binary (its own process), as it sets the environment.
#![cfg(feature = "wasm")]

use std::path::PathBuf;
use std::process::Command;

use libpandoc::convert_with;
use libpandoc::wasm::WasmFilter;
use serde_json::{Value, json};

/// The example filter `conversion`, built for wasm.
fn conversion() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../filters");
    let status = Command::new(env!("CARGO"))
        .args(["build", "--release", "--target", "wasm32-wasip1", "--quiet"])
        .args(["--bin", "conversion"])
        .current_dir(&root)
        .status()
        .expect("cargo");
    assert!(
        status.success(),
        "building the example filters (rustup target add wasm32-wasip1)"
    );
    root.join("target/wasm32-wasip1/release/conversion.wasm")
}

/// What the filter saw: the names of its variables, and the values of
/// those named `LIBPANDOC_TEST_...`.
fn seen(f: WasmFilter) -> (Vec<String>, Value) {
    let out = convert_with(&json!({"to": "json"}), Some(b"x"), vec![f.into_filter()]).unwrap();
    let doc: Value = serde_json::from_slice(&out.bytes).unwrap();
    let told: Value = serde_json::from_str(
        doc["blocks"].as_array().unwrap().last().unwrap()["c"][1]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let names = told["env"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap().to_owned())
        .collect();
    (names, told["test-env"].clone())
}

/// The variables the host sets for a filter, in a conversion to JSON.
const OWN: [&str; 6] = [
    "PANDOC_INPUT_FORMAT",
    "PANDOC_OUTPUT_FORMAT",
    "PANDOC_READER_OPTIONS",
    "PANDOC_SANDBOX",
    "PANDOC_VERSION",
    "PANDOC_WRITER_OPTIONS",
];

#[test]
fn a_wasm_filter_sees_only_what_it_is_given() {
    // SAFETY: the only test in this binary: no other thread reads the
    // environment meanwhile.
    let set = |k: &str, v: Option<&str>| unsafe {
        match v {
            Some(v) => std::env::set_var(k, v),
            None => std::env::remove_var(k),
        }
    };
    let path = conversion();
    let filter = || WasmFilter::from_file(&path).unwrap();
    set("LIBPANDOC_TEST_SECRET", Some("canary"));
    // by default, none of the process's variables: the host's own only
    let (names, values) = seen(filter());
    assert_eq!(names, OWN, "{names:?}");
    assert_eq!(values, json!({}));
}
