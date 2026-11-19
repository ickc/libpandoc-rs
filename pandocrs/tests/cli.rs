//! pandocrs as a command, with the example filters built for wasm and the
//! example program with a filter compiled in.

use std::io::Write;
use std::path::Path;
#[cfg(feature = "wasm")]
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[cfg(feature = "wasm")]
use serde_json::{Value, json};

#[cfg(feature = "wasm")]
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The example filters, built for wasm (the tests that run them need
/// pandocrs's `wasm` feature; without it, a `.wasm` is a JSON filter
/// pandoc can't run).
#[cfg(feature = "wasm")]
fn wasm_filters() -> &'static PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
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
    run_with(prog, args, input, cwd, None)
}

/// [`run`], with `LIBPANDOC_WASM_DIRS` set to `dirs` (else unset).
fn run_with(
    prog: &Path,
    args: &[&str],
    input: &str,
    cwd: Option<&Path>,
    dirs: Option<&std::ffi::OsStr>,
) -> (i32, String, String) {
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .env_remove("LIBPANDOC_WASM_DIRS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    if let Some(d) = dirs {
        cmd.env("LIBPANDOC_WASM_DIRS", d);
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    // pandoc's command writes native line endings (CRLF on Windows)
    let s = |b: Vec<u8>| String::from_utf8(b).unwrap().replace("\r\n", "\n");
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

/// A standard output closed before anything is written to it (as `| head`
/// may leave it): pandoc's status and nothing said, as with pandoc, for
/// pandoc's output and for pandocrs's own line after it (`--version`).
#[test]
fn a_closed_standard_output_ends_quietly() {
    for args in [&["--version"][..], &["-t", "html"]] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let mut child = Command::new(env!("CARGO_BIN_EXE_pandocrs"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(writer)
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // (pandoc reads it all before it writes; --version reads none)
        let _ = child.stdin.take().unwrap().write_all(b"*hi*");
        let out = child.wait_with_output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!((out.status.code(), err.as_ref()), (Some(0), ""), "{args:?}");
    }
}

/// Another failure to write its own line (a full disk): said, status 1.
#[cfg(target_os = "linux")]
#[test]
fn a_full_standard_output_is_an_error() {
    let full = std::fs::File::create("/dev/full").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_pandocrs"))
        .arg("--version")
        .stdout(full)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert_eq!(err, "pandocrs: No space left on device (os error 28)\n");
}

#[cfg(feature = "wasm")]
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

#[cfg(feature = "wasm")]
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

#[cfg(feature = "wasm")]
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

/// A tree for what a wasm filter sees: `work` (the working directory, with
/// `img`), `common` beside it, `secret` (a canary) and a file.
#[cfg(feature = "wasm")]
fn tree(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("pandocrs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    for d in ["work/img", "common", "secret"] {
        std::fs::create_dir_all(base.join(d)).unwrap();
    }
    for (f, text) in [
        ("work/here.txt", "HERE-OK"),
        ("work/img/in.txt", "IMG-OK"),
        ("common/part.txt", "COMMON-OK"),
        ("secret/secret.txt", "CANARY-WASM-DIRS"),
        ("a-file.txt", "not a directory"),
    ] {
        std::fs::write(base.join(f), text).unwrap();
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink("../secret/secret.txt", base.join("work/link-out.txt")).unwrap();
    // the real path, as the working directory is (macOS's /var is
    // /private/var), but not Windows' `\\?\` form
    match cfg!(windows) {
        true => base,
        false => base.canonicalize().unwrap(),
    }
}

/// A wasm filter sees no directory but those `LIBPANDOC_WASM_DIRS` gives,
/// each under its absolute path and its path relative to the working
/// directory (and the working directory as `.` inside one); paths are not
/// normalised.
#[cfg(feature = "wasm")]
#[test]
fn a_wasm_filter_sees_the_directories_given() {
    let base = tree("dirs");
    let work = base.join("work");
    let include = wasm_filters().join("include.wasm");
    let list = |dirs: &[&str]| std::env::join_paths(dirs).unwrap();
    // (status, output, error) of `include` on `file` with the variable
    let include = |dirs: Option<&[&str]>, file: &str| {
        let dirs = dirs.map(list);
        run_with(
            Path::new(env!("CARGO_BIN_EXE_pandocrs")),
            &["-t", "plain", "-F", include.to_str().unwrap()],
            &format!("```include\n{file}\n```\n"),
            Some(&work),
            dirs.as_deref(),
        )
    };
    // (as the filter names it: on Windows `C:\x` is `/c/x`)
    let absolute = |p: &str| {
        let s = base.join(p).display().to_string().replace('\\', "/");
        match s.as_bytes() {
            [d, b':', ..] if cfg!(windows) => {
                format!("/{}{}", d.to_ascii_lowercase() as char, &s[2..])
            }
            _ => s,
        }
    };
    let reads = |dirs: Option<&[&str]>, file: &str, text: &str| {
        let (status, out, err) = include(dirs, file);
        assert_eq!((status, out.trim()), (0, text), "{dirs:?} {file}: {err}");
    };
    let fails = |dirs: Option<&[&str]>, file: &str| {
        let (status, out, err) = include(dirs, file);
        assert_eq!(status, 83, "{dirs:?} {file}: {out} {err}");
        assert!(!out.contains("CANARY") && !err.contains("CANARY"), "{err}");
        err
    };
    // none by default, and none when empty: the failure says so
    for dirs in [None, Some(&[][..]), Some(&[""][..])] {
        let err = fails(dirs, "here.txt");
        assert!(
            err.contains("exited with status 3 (it was given no directory: LIBPANDOC_WASM_DIRS)"),
            "{err}"
        );
    }
    // the working directory, as `.`, and by its absolute path
    reads(Some(&["."]), "here.txt", "HERE-OK");
    reads(Some(&["."]), "img/in.txt", "IMG-OK");
    reads(Some(&["."]), &absolute("work/here.txt"), "HERE-OK");
    let err = fails(Some(&["."]), "../common/part.txt");
    assert!(!err.contains("given no directory"), "{err}");
    let work_dir = work.display().to_string();
    reads(Some(&[&work_dir]), "here.txt", "HERE-OK");
    // its parent, as `..`, and the working directory as `.` inside it
    reads(Some(&[".."]), "../common/part.txt", "COMMON-OK");
    reads(Some(&[".."]), "here.txt", "HERE-OK");
    reads(Some(&[".."]), &absolute("common/part.txt"), "COMMON-OK");
    // a sibling, and a list
    reads(Some(&["../common"]), "../common/part.txt", "COMMON-OK");
    fails(Some(&["../common"]), "here.txt");
    reads(Some(&[".", "../common"]), "here.txt", "HERE-OK");
    reads(Some(&[".", "../common"]), "../common/part.txt", "COMMON-OK");
    // a subdirectory: only below it
    reads(Some(&["img"]), "img/in.txt", "IMG-OK");
    fails(Some(&["img"]), "here.txt");
    // not normalised
    fails(Some(&["img"]), "./img/in.txt");
    for file in ["./../common/part.txt", "img/../../common/part.txt"] {
        let err = fails(Some(&[".", "../common"]), file);
        assert!(err.contains("Operation not permitted"), "{err}");
    }
    // outside what was given: missing
    fails(Some(&[".", "../common"]), &absolute("secret/secret.txt"));
    fails(Some(&[".", "../common"]), "../secret/secret.txt");
    #[cfg(unix)]
    fails(Some(&["."]), "link-out.txt");
    // a directory that doesn't exist, a file: the filter fails, naming it
    let err = fails(Some(&[".", "nope"]), "here.txt");
    assert!(
        err.contains("LIBPANDOC_WASM_DIRS: nope does not exist"),
        "{err}"
    );
    let file = Path::new("..").join("a-file.txt").display().to_string();
    let err = fails(Some(&[&file]), "here.txt");
    assert!(
        err.contains(&format!("LIBPANDOC_WASM_DIRS: {file} is not a directory")),
        "{err}"
    );
    std::fs::remove_dir_all(&base).unwrap();
}

/// `wasm-filter`, pandocrs's second program, as pandocrs: no directory
/// unless `LIBPANDOC_WASM_DIRS` gives some.
#[cfg(feature = "wasm")]
#[test]
fn wasm_filter_sees_the_directories_given() {
    let base = tree("wasm-filter");
    let work = base.join("work");
    let (status, doc, err) = pandocrs(&["-t", "json"], "```include\nhere.txt\n```\n");
    assert_eq!(status, 0, "{err}");
    let include = wasm_filters().join("include.wasm");
    let run = |dirs: Option<&std::ffi::OsStr>| {
        run_with(
            Path::new(env!("CARGO_BIN_EXE_wasm-filter")),
            &[include.to_str().unwrap(), "plain"],
            &doc,
            Some(&work),
            dirs,
        )
    };
    let (status, _, err) = run(None);
    assert_ne!(status, 0);
    assert!(
        err.contains("(it was given no directory: LIBPANDOC_WASM_DIRS)"),
        "{err}"
    );
    let (status, out, err) = run(Some(".".as_ref()));
    assert!(status == 0 && out.contains("HERE-OK"), "{out} {err}");
    std::fs::remove_dir_all(&base).unwrap();
}

/// The variables a filter sees besides the host's own, and the values of
/// those named `LIBPANDOC_TEST_...` (the example filter `conversion`'s
/// report, the last block of a document as JSON).
#[cfg(feature = "wasm")]
fn seen_in(doc: &str) -> (Vec<String>, Value) {
    const OWN: [&str; 6] = [
        "PANDOC_INPUT_FORMAT",
        "PANDOC_OUTPUT_FORMAT",
        "PANDOC_READER_OPTIONS",
        "PANDOC_SANDBOX",
        "PANDOC_VERSION",
        "PANDOC_WRITER_OPTIONS",
    ];
    let doc: Value = serde_json::from_str(doc).unwrap();
    let told = doc["blocks"].as_array().unwrap().last().unwrap()["c"][1]
        .as_str()
        .unwrap();
    let told: Value = serde_json::from_str(told).unwrap();
    let names = told["env"].as_array().unwrap().iter();
    let names = names.map(|n| n.as_str().unwrap().to_owned());
    let others = names.filter(|n| !OWN.contains(&n.as_str())).collect();
    (others, told["test-env"].clone())
}

/// pandocrs, and `wasm-filter`, give a wasm filter the variables
/// `LIBPANDOC_WASM_ENV` names, with their values, and no other of theirs;
/// one that can't be a name fails it, named up to an `=`.
#[cfg(feature = "wasm")]
#[test]
fn a_wasm_filter_is_given_the_variables_named() {
    let conversion = wasm_filters().join("conversion.wasm");
    let conversion = conversion.to_str().unwrap();
    let (status, doc, err) = pandocrs(&["-t", "json"], "x");
    assert_eq!(status, 0, "{err}");
    let run = |wasm_filter: bool, list: Option<&str>| {
        let (prog, args, input) = match wasm_filter {
            false => (
                env!("CARGO_BIN_EXE_pandocrs"),
                vec!["-t", "json", "-F", conversion],
                "x",
            ),
            true => (
                env!("CARGO_BIN_EXE_wasm-filter"),
                vec![conversion, "json"],
                doc.as_str(),
            ),
        };
        let mut cmd = Command::new(prog);
        cmd.args(args)
            .env_remove("LIBPANDOC_WASM_ENV")
            .env_remove("LIBPANDOC_TEST_MISSING")
            .env("LIBPANDOC_TEST_A", "a")
            .env("LIBPANDOC_TEST_B", "b")
            .env("LIBPANDOC_TEST_SECRET", "canary")
            .env("PANDOC_VERSION", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(l) = list {
            cmd.env("LIBPANDOC_WASM_ENV", l);
        }
        let mut child = cmd.spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(input.as_bytes()).unwrap();
        drop(stdin);
        let out = child.wait_with_output().unwrap();
        let s = |b: Vec<u8>| String::from_utf8(b).unwrap();
        (
            out.status.code().unwrap_or(-1),
            s(out.stdout),
            s(out.stderr),
        )
    };
    for wasm_filter in [false, true] {
        let seen = |list| {
            let (status, out, err) = run(wasm_filter, list);
            assert_eq!(status, 0, "{wasm_filter} {list:?}: {err}");
            seen_in(&out)
        };
        assert_eq!(seen(None), (vec![], json!({})), "{wasm_filter}");
        assert_eq!(seen(Some("")), (vec![], json!({})), "{wasm_filter}");
        let (names, values) = seen(Some(
            "LIBPANDOC_TEST_A,LIBPANDOC_TEST_MISSING,,LIBPANDOC_TEST_B,PANDOC_VERSION",
        ));
        assert_eq!(names, ["LIBPANDOC_TEST_A", "LIBPANDOC_TEST_B"]);
        assert_eq!(
            values,
            json!({"LIBPANDOC_TEST_A": "a", "LIBPANDOC_TEST_B": "b"})
        );
        let prog = match wasm_filter {
            false => "pandocrs",
            true => "wasm-filter",
        };
        for (list, says) in [
            ("LIBPANDOC_TEST_A,A B", r#""A B""#),
            (
                "LIBPANDOC_TEST_SECRET=canary",
                r#""LIBPANDOC_TEST_SECRET=...""#,
            ),
        ] {
            let (status, out, err) = run(wasm_filter, Some(list));
            assert_eq!(status, if wasm_filter { 1 } else { 83 }, "{out} {err}");
            let says = format!("{prog}: LIBPANDOC_WASM_ENV: not a variable's name: {says}\n");
            assert!(err.ends_with(&says), "{err}");
            assert!(!err.contains("canary"), "{err}");
        }
    }
    // the host's own win: PANDOC_VERSION is its pandoc's
    for wasm_filter in [false, true] {
        let (_, out, _) = run(wasm_filter, Some("PANDOC_VERSION"));
        let doc: Value = serde_json::from_str(&out).unwrap();
        let told = doc["blocks"].as_array().unwrap().last().unwrap()["c"][1].as_str();
        let told: Value = serde_json::from_str(told.unwrap()).unwrap();
        assert_ne!(told["pandoc-version"], "0", "{told}");
    }
}

/// The example filter `citeproc`: the files the metadata names, read by
/// the filter in the directories given (`"citeproc-files"`); without them
/// they are missing, and a canary outside them never shows, however the
/// conversion is sandboxed (not, pandoc's `--sandbox`, the directory
/// sandbox listing the canary's directory).
#[cfg(feature = "wasm")]
#[test]
fn a_wasm_filter_runs_citeproc() {
    let base = tree("citeproc");
    let work = base.join("work");
    let files = [
        (
            "work/refs.bib",
            "@article{key, title={BIB-OK}, journal={Journal of Long Names}, \
             author={Author, A.}, year={2000}}\n",
        ),
        (
            "work/style.csl",
            r#"<?xml version="1.0" encoding="utf-8"?>
<style xmlns="http://purl.org/net/xbiblio/csl" class="in-text" version="1.0">
  <info><title>Test</title><id>test</id><updated>2026-01-01T00:00:00+00:00</updated></info>
  <citation><layout><text value="STYLE-OK "/><text variable="title"/><text value=" in "/><text variable="container-title" form="short"/></layout></citation>
</style>
"#,
        ),
        (
            "work/abbr.json",
            r#"{"default": {"container-title": {"Journal of Long Names": "ABBR-OK"}}}"#,
        ),
        (
            "secret/canary.bib",
            "@book{key, title={CANARY-WASM-CITEPROC}, author={B}, year={2001}}\n",
        ),
    ];
    for (f, text) in files {
        std::fs::write(base.join(f), text).unwrap();
    }
    let citeproc = wasm_filters().join("citeproc.wasm");
    let doc =
        "---\nbibliography: refs.bib\ncsl: style\ncitation-abbreviations: abbr\n---\n\n[@key]\n";
    let canary = "---\nbibliography: ../secret/canary.bib\nnocite: '@*'\n---\n\n[@key]\n";
    let run = |args: &[&str], sandbox: Option<&str>, dirs: Option<&str>, input: &str| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_pandocrs"));
        cmd.args(["-t", "plain", "-F", citeproc.to_str().unwrap()])
            .args(args)
            .current_dir(&work)
            .env_remove("LIBPANDOC_WASM_DIRS")
            .env_remove("LIBPANDOC_SANDBOX")
            .env_remove("LIBPANDOC_SANDBOX_WRITE");
        if let Some(d) = dirs {
            cmd.env("LIBPANDOC_WASM_DIRS", d);
        }
        if let Some(d) = sandbox {
            cmd.env("LIBPANDOC_SANDBOX", d);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let s = |b: Vec<u8>| String::from_utf8_lossy(&b).into_owned();
        (
            out.status.code().unwrap_or(-1),
            s(out.stdout),
            s(out.stderr),
        )
    };
    // (the arguments, LIBPANDOC_SANDBOX): native, pandoc's sandbox, the
    // directory sandbox (`..`: the canary's directory too)
    let conversions: [(&[&str], Option<&str>); 3] =
        [(&[], None), (&["--sandbox"], None), (&[], Some(".."))];
    for (args, sandbox) in conversions {
        let what = format!("{args:?} {sandbox:?}");
        let (status, out, err) = run(args, sandbox, Some("."), doc);
        assert_eq!(status, 0, "{what}: {err}");
        for ok in ["STYLE-OK", "BIB-OK", "ABBR-OK"] {
            assert!(out.contains(ok), "{what}: {out}");
        }
        let (status, out, err) = run(args, sandbox, None, doc);
        assert_eq!(status, 83, "{what}: {out} {err}");
        assert!(err.contains("not found"), "{what}: {err}");
        assert!(
            err.contains("(it was given no directory: LIBPANDOC_WASM_DIRS)"),
            "{what}: {err}"
        );
        let (status, out, err) = run(args, sandbox, Some("."), canary);
        assert_eq!(status, 83, "{what}: {out} {err}");
        assert!(
            !out.contains("CANARY") && !err.contains("CANARY"),
            "{what}: {out} {err}"
        );
        assert!(err.contains("canary.bib' not found"), "{what}: {err}");
    }
    std::fs::remove_dir_all(&base).unwrap();
}

/// `-F panclude` is libpandoc's include step, never a program of that name
/// (one in the working directory is not run) nor a filter of pandocrs's:
/// with no filter of its own, with wasm filters after it (which, given no
/// directory, see the content), under `--sandbox` (no roots) and in the
/// directory sandbox (the document's directory mounted).
#[cfg(feature = "wasm")]
#[test]
fn panclude_is_the_include_step() {
    let base = std::env::temp_dir().join(format!("pandocrs-panclude-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let work = base.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let work = match cfg!(windows) {
        true => work,
        false => work.canonicalize().unwrap(),
    };
    for (f, text) in [
        (
            "doc.md",
            "```{.parse include=\"part.md\"}\nbefore the part:\n```\n\n```{include=\"../secret.txt\"}\nout\n```\n",
        ),
        ("part.md", "hello *from the part*\n"),
        // a program named panclude, which must not run
        ("panclude", "#!/bin/sh\necho RAN > ran.txt\ncat\n"),
    ] {
        std::fs::write(work.join(f), text).unwrap();
    }
    std::fs::write(base.join("secret.txt"), "CANARY-PANCLUDE").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let p = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(work.join("panclude"), p).unwrap();
    }
    let parse = wasm_filters().join("parse.wasm");
    let upper = wasm_filters().join("upper.wasm");
    let (parse, upper) = (parse.to_str().unwrap(), upper.to_str().unwrap());
    let run = |args: &[&str], sandbox: Option<&str>| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_pandocrs"));
        cmd.args(["doc.md", "-t", "plain"])
            .args(args)
            .current_dir(&work)
            .env_remove("LIBPANDOC_WASM_DIRS")
            .env_remove("LIBPANDOC_SANDBOX")
            .env_remove("LIBPANDOC_SANDBOX_WRITE");
        if let Some(d) = sandbox {
            cmd.env("LIBPANDOC_SANDBOX", d);
        }
        let out = cmd.stdin(Stdio::null()).output().unwrap();
        let s = |b: Vec<u8>| String::from_utf8(b).unwrap().replace("\r\n", "\n");
        let (status, out, err) = (
            out.status.code().unwrap_or(-1),
            s(out.stdout),
            s(out.stderr),
        );
        assert_eq!(status, 0, "{args:?}: {err}");
        assert!(
            !work.join("ran.txt").exists(),
            "{args:?}: the program panclude ran"
        );
        assert!(
            !out.contains("CANARY") && !err.contains("CANARY"),
            "{out} {err}"
        );
        (out, err)
    };
    let outside = "panclude: cannot include \"../secret.txt\" in doc.md: outside the roots";
    let (out, err) = run(&["-F", "panclude"], None);
    assert!(out.contains("hello *from the part*"), "{out}");
    assert!(err.contains(outside), "{err}");
    let (out, _) = run(&["-F", "panclude", "-F", parse, "-F", upper], None);
    assert!(
        out.starts_with("BEFORE THE PART: HELLO FROM THE PART\n"),
        "{out}"
    );
    let (out, _) = run(&["-F", parse, "-F", upper], None);
    assert!(out.starts_with("BEFORE THE PART:\n"), "{out}");
    // pandoc's sandbox: no roots
    let (out, err) = run(
        &["--sandbox", "-F", "panclude", "-F", parse, "-F", upper],
        None,
    );
    assert!(out.starts_with("BEFORE THE PART:\n"), "{out}");
    assert!(
        err.contains("there are no roots, so no file is included"),
        "{err}"
    );
    // the directory sandbox, the working directory mounted
    let (out, err) = run(&["-F", "panclude"], Some("."));
    assert!(out.contains("hello *from the part*"), "{out}");
    assert!(err.contains(outside), "{err}");
    let (out, _) = run(&["-F", "panclude", "-F", parse, "-F", upper], Some("."));
    assert!(
        out.starts_with("BEFORE THE PART: HELLO FROM THE PART\n"),
        "{out}"
    );
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn a_compiled_in_filter() {
    // cargo test builds the examples into the profile's directory: the
    // test's grandparent before Rust 1.100 (`target/debug/deps/cli-*`),
    // further up since (`target/debug/build/pandocrs/HASH/out/cli-*`)
    let exe = std::env::current_exe().unwrap();
    let name = format!("my-pandoc{}", std::env::consts::EXE_SUFFIX);
    let prog = exe
        .ancestors()
        .map(|d| d.join("examples").join(&name))
        .find(|p| p.is_file())
        .expect("the my-pandoc example, built by cargo test");
    let (status, out, err) = run(
        &prog,
        &["-t", "plain", "-F", "upper"],
        "hello *world*",
        None,
    );
    assert_eq!((status, out.as_str()), (0, "HELLO WORLD\n"), "{err}");
    let (_, out, _) = run(&prog, &["--help"], "", None);
    assert!(out.contains("compiled in (-F NAME): upper"), "{out}");
    // consecutive compiled-in filters are one exchange with pandoc, as
    // its log tells: compiled in, Lua, compiled in twice is two callbacks
    // and the Lua filter between them
    let lua = concat!(env!("CARGO_MANIFEST_DIR"), "/../libpandoc/tests/upper.lua");
    let args = [
        "--verbose",
        "-t",
        "plain",
        "-F",
        "upper",
        "-L",
        lua,
        "-F",
        "upper",
        "-F",
        "upper",
    ];
    let (status, out, err) = run(&prog, &args, "hello *world*", None);
    assert_eq!((status, out.as_str()), (0, "HELLO WORLD\n"), "{err}");
    let ran: Vec<&str> = err
        .lines()
        .filter_map(|l| l.strip_prefix("[INFO] Running filter "))
        .collect();
    assert_eq!(
        ran,
        ["libpandoc:callback/0", lua, "libpandoc:callback/1"],
        "{err}"
    );
    // LIBPANDOC_SEPARATE_FILTERS=1: each its own; a value not 1, 0 or
    // empty is an error naming it
    let separate = |v: &str| {
        let out = Command::new(&prog)
            .args(["--verbose", "-t", "plain", "-F", "upper", "-F", "upper"])
            .env("LIBPANDOC_SEPARATE_FILTERS", v)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        let callbacks = err
            .matches("[INFO] Running filter libpandoc:callback/")
            .count();
        (out.status.code(), callbacks, err)
    };
    assert_eq!(separate("").1, 1);
    assert_eq!(separate("0").1, 1);
    assert_eq!(separate("1").1, 2);
    let (status, _, err) = separate("yes");
    assert_eq!(status, Some(1), "{err}");
    assert!(
        err.contains("LIBPANDOC_SEPARATE_FILTERS: 1 (each filter its own exchange with pandoc), or 0 or empty, not \"yes\""),
        "{err}"
    );
}

/// Ctrl-C ends pandocrs at once, by the signal, as it ends pandoc: SIGINT
/// keeps its default action (the library handles no signal).
#[cfg(unix)]
#[test]
fn ctrl_c() {
    use std::os::unix::process::ExitStatusExt;
    use std::time::{Duration, Instant};
    let dir = std::env::temp_dir().join(format!("pandocrs-ctrl-c-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let slow = dir.join("slow.md");
    std::fs::write(&slow, ">".repeat(100_000)).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_pandocrs"))
        .arg(&slow)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_secs(2));
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
    let _ = std::fs::remove_dir_all(&dir);
}
