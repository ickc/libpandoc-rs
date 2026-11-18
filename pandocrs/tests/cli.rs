//! pandocrs as a command, with the example filters built for wasm and the
//! example program with a filter compiled in.

use std::io::Write;
use std::path::Path;
#[cfg(feature = "wasm")]
use std::path::PathBuf;
use std::process::{Command, Stdio};

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
    base
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
