//! Cancelling calls ([`libpandoc::CancelToken`], `"cancel-token"`;
//! libpandoc 1.13's cancel tokens) and Ctrl-C: before, during and after a
//! call, from another thread, with a callback running, in the directory
//! sandbox, and SIGINT sent to a process converting a slow document.
//!
//! The slow input is libpandoc's test/cancel.c's: 100,000 `>` read as
//! markdown, minutes of pandoc's time (a `"timeout"` bounds each use here,
//! so that a cancel that doesn't work fails the test rather than hanging it).

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use libpandoc::{CancelToken, Error, Filter, Output};
use serde_json::{Value, json};

const LIMIT: u64 = 60;

fn slow() -> String {
    ">".repeat(100_000)
}

fn with_token(options: Value, t: &CancelToken) -> Value {
    let mut o = options;
    o["cancel-token"] = t.id().into();
    o["timeout"] = LIMIT.into();
    o
}

/// `t` cancelled from another thread after `after`.
fn cancel_after(t: &CancelToken, after: Duration) -> thread::JoinHandle<()> {
    let t = t.clone();
    thread::spawn(move || {
        thread::sleep(after);
        t.cancel();
    })
}

/// What `f` returned, and how long it took.
fn timed<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let started = Instant::now();
    let r = f();
    (r, started.elapsed())
}

fn cancelled<T: std::fmt::Debug>(r: libpandoc::Result<T>) -> Error {
    let e = r.unwrap_err();
    assert!(e.is_cancelled() && e.kind == Error::CANCELLED, "{e}");
    e
}

#[test]
fn a_token() {
    let t = CancelToken::new().unwrap();
    assert!(t.id() > 0 && !t.is_cancelled());
    let u = t.clone();
    u.cancel();
    u.cancel(); // (again: nothing more)
    assert!(t.is_cancelled() && t.id() == u.id());
    fn send_sync<T: Send + Sync>(_: &T) {}
    send_sync(&t);
}

#[test]
fn cancelled_before() {
    let t = CancelToken::new().unwrap();
    t.cancel();
    let slow = slow();
    let (r, took) = timed(|| {
        libpandoc::convert(
            &with_token(json!({"to": "html"}), &t),
            Some(slow.as_bytes()),
        )
    });
    cancelled(r);
    assert!(took < Duration::from_secs(1), "{took:?}");
    cancelled(libpandoc::read_many(
        &["a", "b"],
        &with_token(json!({}), &t),
    ));
}

#[test]
fn cancelled_during() {
    let slow = slow();
    let doc = libpandoc::read("x", "markdown").unwrap();
    type Call<'a> = Box<dyn Fn(&CancelToken) -> libpandoc::Result<Output> + 'a>;
    let calls: Vec<(&str, Call)> = vec![
        (
            "convert",
            Box::new(|t| libpandoc::convert(&with_token(json!({}), t), Some(slow.as_bytes()))),
        ),
        (
            "read_many",
            Box::new(|t| {
                libpandoc::read_many(&[slow.as_str(), "x"], &with_token(json!({}), t)).map(|r| {
                    Output {
                        bytes: Vec::new(),
                        log: r.log,
                    }
                })
            }),
        ),
        (
            "convert_with",
            Box::new(|t| {
                libpandoc::convert_with(
                    &with_token(json!({}), t),
                    Some(slow.as_bytes()),
                    vec![Filter::func(|_, _| Ok(()))],
                )
            }),
        ),
        (
            "write_with",
            Box::new(|t| {
                // (cancelled before it starts: it fails at once)
                t.cancel();
                libpandoc::write_with(&doc, &with_token(json!({"to": "html"}), t))
            }),
        ),
    ];
    for (name, call) in calls {
        let t = CancelToken::new().unwrap();
        let canceller = cancel_after(&t, Duration::from_millis(500));
        let (r, took) = timed(|| call(&t));
        canceller.join().unwrap();
        cancelled(r);
        assert!(took < Duration::from_secs(3), "{name}: {took:?}");
    }
    // the library goes on
    let out = libpandoc::convert(&json!({"to": "html"}), Some(b"*x*")).unwrap();
    assert_eq!(out.text(), "<p><em>x</em></p>\n");
}

#[test]
fn cancelled_after() {
    let t = CancelToken::new().unwrap();
    let o = with_token(json!({"to": "html"}), &t);
    assert_eq!(
        libpandoc::convert(&o, Some(b"*x*")).unwrap().text(),
        "<p><em>x</em></p>\n"
    );
    t.cancel();
    cancelled(libpandoc::convert(&o, Some(b"*x*")));
}

#[test]
fn one_token_for_many_calls() {
    let t = CancelToken::new().unwrap();
    let canceller = cancel_after(&t, Duration::from_millis(500));
    let slow = slow();
    thread::scope(|s| {
        let calls: Vec<_> = (0..3)
            .map(|_| {
                s.spawn(|| libpandoc::convert(&with_token(json!({}), &t), Some(slow.as_bytes())))
            })
            .collect();
        for c in calls {
            cancelled(c.join().unwrap());
        }
    });
    canceller.join().unwrap();
}

/// A callback running when the token is cancelled runs to its end; the
/// call then fails. A callback may cancel its own call's token.
#[test]
fn a_callback_runs_to_its_end() {
    let t = CancelToken::new().unwrap();
    let ran = AtomicBool::new(false);
    let canceller = cancel_after(&t, Duration::from_millis(200));
    let (r, took) = timed(|| {
        libpandoc::convert_with(
            &with_token(json!({"to": "html"}), &t),
            Some(b"x"),
            vec![Filter::func(|_, _| {
                thread::sleep(Duration::from_secs(1));
                ran.store(true, Ordering::SeqCst);
                Ok(())
            })],
        )
    });
    canceller.join().unwrap();
    cancelled(r);
    assert!(ran.load(Ordering::SeqCst) && took >= Duration::from_secs(1));

    let t = CancelToken::new().unwrap();
    let r = libpandoc::convert_with(
        &with_token(json!({"to": "html"}), &t),
        Some(b"x"),
        vec![Filter::func(|_, _| {
            t.cancel();
            Ok(())
        })],
    );
    cancelled(r);
}

/// A token's number once it is dropped, or one never made, is libpandoc's
/// option error; `"untrusted"` options take none.
#[test]
fn what_is_refused() {
    let id = CancelToken::new().unwrap().id();
    let e = libpandoc::convert(&json!({"cancel-token": id}), Some(b"x")).unwrap_err();
    assert_eq!(e.kind, "PandocOptionError", "{e}");
    let t = CancelToken::new().unwrap();
    let e = libpandoc::convert(
        &json!({"untrusted": true, "cancel-token": t.id()}),
        Some(b"x"),
    )
    .unwrap_err();
    assert_eq!(e.kind, "PandocOptionError", "{e}");
}

/// In the sandbox the token stops libpandoc.wasm from outside (wasmtime's
/// epoch): pandoc, and Lua, which nothing stops natively.
#[cfg(feature = "wasm")]
#[test]
fn the_directory_sandbox() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("cancel-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    std::fs::write(
        dir.join("forever.lua"),
        "function Pandoc(d) while true do end end\n",
    )
    .unwrap();
    let sandbox = json!({"read": [dir], "timeout": LIMIT});
    // (the module compiled first)
    libpandoc::convert(&json!({"to": "html", "sandbox": sandbox}), Some(b"x")).unwrap();
    let slow = slow();
    let lua = dir.join("forever.lua");
    for (options, input) in [
        (json!({"to": "html", "sandbox": sandbox}), slow.as_bytes()),
        (
            json!({"to": "html", "sandbox": sandbox, "filters": [lua]}),
            b"x".as_slice(),
        ),
    ] {
        let t = CancelToken::new().unwrap();
        let mut o = options.clone();
        o["cancel-token"] = t.id().into();
        let canceller = cancel_after(&t, Duration::from_secs(1));
        let (r, took) = timed(|| libpandoc::convert(&o, Some(input)));
        canceller.join().unwrap();
        cancelled(r);
        assert!(took < Duration::from_secs(3), "{took:?}");
    }
    let t = CancelToken::new().unwrap();
    t.cancel();
    let o = json!({"to": "html", "sandbox": sandbox, "cancel-token": t.id()});
    cancelled(libpandoc::convert(&o, Some(b"x")));
    let o = json!({"to": "html", "sandbox": sandbox, "cancel-token": 0});
    let e = libpandoc::convert(&o, Some(b"x")).unwrap_err();
    assert!(
        e.kind == "PandocOptionError" && e.message.contains("no token"),
        "{e}"
    );
}

/// Ctrl-C: the library handles no signal, so a program keeps SIGINT's
/// default action and ends at once, by the signal, with no crash, during
/// a long conversion. Run in a child process of this test program.
#[cfg(unix)]
#[test]
fn ctrl_c_ends_the_program() {
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};
    for sandboxed in [false, cfg!(feature = "wasm")] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "slow_child", "--nocapture", "--test-threads=1"])
            .env("SLOW_CHILD", if sandboxed { "sandbox" } else { "native" })
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        thread::sleep(Duration::from_secs(3));
        let sent = Instant::now();
        let ok = Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let status = child.wait().unwrap();
        assert_eq!(status.signal(), Some(2), "{status:?}");
        assert!(
            sent.elapsed() < Duration::from_secs(3),
            "{:?}",
            sent.elapsed()
        );
    }
}

/// The child of [`ctrl_c_ends_the_program`]: a long conversion.
#[test]
fn slow_child() {
    let Ok(how) = std::env::var("SLOW_CHILD") else {
        return;
    };
    let mut o = json!({"to": "html", "timeout": LIMIT});
    if how == "sandbox" {
        o["sandbox"] = json!({"read": [], "timeout": LIMIT});
    }
    let _ = libpandoc::convert(&o, Some(slow().as_bytes()));
    std::process::exit(3);
}
