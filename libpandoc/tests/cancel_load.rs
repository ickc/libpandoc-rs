//! A cancel that arrives while libpandoc.wasm is loading (a process's first
//! call in the directory sandbox): a test program of its own, so that the
//! module is not loaded yet when the call starts.
#![cfg(feature = "wasm")]

use std::thread;
use std::time::Duration;

use libpandoc::CancelToken;
use serde_json::json;

/// The cancel advanced the epoch before the call's store had a deadline,
/// and without a time limit nothing else reached it: the conversion ran
/// and returned its output.
#[test]
fn cancelled_while_the_module_loads() {
    let t = CancelToken::new().unwrap();
    let canceller = {
        let t = t.clone();
        thread::spawn(move || {
            // (the load takes 0.2 s from wasmtime's cache, a first
            // compilation far longer: 20 ms is in it; a cancel before the
            // call or after the load is one the call stops for anyway)
            thread::sleep(Duration::from_millis(20));
            t.cancel();
        })
    };
    let o = json!({"to": "html", "sandbox": {"read": []}, "cancel-token": t.id()});
    let r = libpandoc::convert(&o, Some(b"x"));
    canceller.join().unwrap();
    let e = r.expect_err("the cancel was lost: the conversion ran");
    assert!(e.is_cancelled(), "{e}");
}
