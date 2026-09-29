//! pandocrs as a command, with the example filters built for wasm and the
//! example program with a filter compiled in.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn wasm_filters() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = root().join("filters");
        let ok = Command::new(env!("CARGO"))
            .args(["build", "--release", "--target", "wasm32-wasip1", "--quiet"])
            .current_dir(&dir)
            .status()
            .unwrap()
            .success();
        assert!(ok, "building the example filters");
        dir.join("target/wasm32-wasip1/release")
    })
}

/// Run a program with standard input; (status, stdout, stderr).
fn run(prog: &Path, args: &[&str], input: &str, cwd: Option<&Path>) -> (i32, String, String) {
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let s = |b: Vec<u8>| String::from_utf8(b).unwrap();
    (
        out.status.code().unwrap_or(-1),
        s(out.stdout),
        s(out.stderr),
    )
}

fn pandocrs(args: &[&str], input: &str) -> (i32, String, String) {
    run(Path::new(env!("CARGO_BIN_EXE_pandocrs")), args, input, None)
}

#[test]
fn is_pandoc() {
    let (status, out, _) = pandocrs(&["-f", "markdown", "-t", "html"], "*hi*");
    assert_eq!((status, out.as_str()), (0, "<p><em>hi</em></p>\n"));
    let (status, _, err) = pandocrs(&["-f", "nope"], "");
    assert_ne!(status, 0);
    assert!(err.contains("nope"), "{err}");
}

#[test]
fn runs_wasm_filters_in_order_with_pandocs() {
    let upper = wasm_filters().join("upper.wasm");
    let lua = root().join("libpandoc/tests/upper.lua");
    // the wasm filter adds nothing to upper-case; use it after a Lua filter
    // that does, then check both ran by their effects on different text
    let (status, out, err) = pandocrs(
        &[
            "-t",
            "plain",
            "--lua-filter",
            lua.to_str().unwrap(),
            "-F",
            upper.to_str().unwrap(),
        ],
        "hello *world*",
    );
    assert_eq!((status, out.as_str()), (0, "HELLO WORLD\n"), "{err}");
}

#[test]
fn finds_wasm_filters_in_the_data_dir() {
    let dir = std::env::temp_dir().join(format!("pandocrs-data-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("filters")).unwrap();
    std::fs::copy(
        wasm_filters().join("upper.wasm"),
        dir.join("filters/up.wasm"),
    )
    .unwrap();
    let (status, out, err) = pandocrs(
        &[
            "--data-dir",
            dir.to_str().unwrap(),
            "-t",
            "plain",
            "-F",
            "up.wasm",
        ],
        "hi",
    );
    assert_eq!((status, out.as_str()), (0, "HI\n"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_failing_wasm_filter_fails_as_pandoc_does() {
    let include = wasm_filters().join("include.wasm");
    let (status, _, err) = pandocrs(
        &["-t", "plain", "-F", include.to_str().unwrap()],
        "```include\nmissing\n```\n",
    );
    assert_eq!(status, 83, "{err}");
    assert!(err.contains("exited with status 3"), "{err}");
}

#[test]
fn a_wasm_filter_reads_the_current_directory() {
    let dir = std::env::temp_dir().join(format!("pandocrs-cwd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("part.txt"), "from a file").unwrap();
    let include = wasm_filters().join("include.wasm");
    let (status, out, err) = run(
        Path::new(env!("CARGO_BIN_EXE_pandocrs")),
        &["-t", "plain", "-F", include.to_str().unwrap()],
        "```include\npart.txt\n```\n",
        Some(&dir),
    );
    assert_eq!((status, out.trim()), (0, "from a file"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_compiled_in_filter() {
    // cargo test builds the examples beside the test
    let exe = std::env::current_exe().unwrap();
    let prog = exe
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/my-pandoc");
    let (status, out, err) = run(
        &prog,
        &["-t", "plain", "-F", "upper"],
        "hello *world*",
        None,
    );
    assert_eq!((status, out.as_str()), (0, "HELLO WORLD\n"), "{err}");
    let (_, out, _) = run(&prog, &["--help"], "", None);
    assert!(out.contains("compiled in (-F NAME): upper"), "{out}");
}
