//! Without the `wasm` feature, a directory sandbox is refused, never run
//! without.
#![cfg(not(feature = "wasm"))]

use serde_json::json;

#[test]
fn a_directory_sandbox_is_refused() {
    for sandbox in [json!({"read": ["."]}), json!({"write": ["."]}), json!({})] {
        let options = json!({"from": "markdown", "to": "html", "sandbox": sandbox});
        let e = libpandoc::convert(&options, Some(b"x")).unwrap_err();
        assert!(e.message.contains("wasm"), "{e}");
        let e = libpandoc::convert_with(&options, Some(b"x"), vec![]).unwrap_err();
        assert!(e.message.contains("wasm"), "{e}");
    }
    // pandoc's own sandbox is as it was
    let out = libpandoc::convert(&json!({"to": "html", "sandbox": true}), Some(b"*x*")).unwrap();
    assert_eq!(out.text(), "<p><em>x</em></p>\n");
}
