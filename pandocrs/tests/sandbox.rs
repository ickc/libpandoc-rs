//! pandocrs with `LIBPANDOC_SANDBOX` and `LIBPANDOC_SANDBOX_WRITE`: the
//! conversion in libpandoc.wasm, which sees only the directories listed
//! (and the user data directories, read-only), within the time and memory
//! limits.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// A directory with `work` (listed, and the working directory), `out`
/// (listed writable in some tests), `secret` beside them, and user data
/// directories.
fn lab() -> &'static PathBuf {
    static LAB: OnceLock<PathBuf> = OnceLock::new();
    LAB.get_or_init(|| {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("pandocrs-sandbox-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in [
            "work",
            "out",
            "secret",
            "data/pandoc/templates",
            "dirs/templates",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        // the real path (macOS's /var is /private/var), but not Windows'
        // `\\?\` form, which pandoc can't take as the working directory
        let root = match cfg!(windows) {
            true => std::path::absolute(&root),
            false => root.canonicalize(),
        }
        .unwrap();
        let files = [
            ("secret/secret.tex", "CANARY-OUTSIDE\n"),
            ("work/inside.tex", "INSIDE-OK\n"),
            ("work/doc.md", "*hi*\n"),
            ("data/pandoc/templates/mine.html", "DATA-DIR-OK $body$\n"),
            ("dirs/templates/theirs.html", "DATA-DIRS-OK $body$\n"),
        ];
        for (f, text) in files {
            std::fs::write(root.join(f), text).unwrap();
        }
        root
    })
}

/// A path of the lab's, for this process.
fn path(p: &str) -> String {
    lab().join(p).display().to_string()
}

/// An absolute path as the module names it: as it is, with `/`; on
/// Windows `C:\x` is `/c/x` (a path with a drive letter means nothing in
/// the module: documented).
#[cfg(feature = "wasm")]
fn named(p: &Path) -> String {
    let s = p.display().to_string().replace('\\', "/");
    let s = s.strip_prefix("//?/").unwrap_or(&s);
    match s.as_bytes() {
        [d, b':', ..] if cfg!(windows) => format!("/{}{}", d.to_ascii_lowercase() as char, &s[2..]),
        _ => s.to_owned(),
    }
}

/// A path of the lab's, for the module.
#[cfg(feature = "wasm")]
fn inside(p: &str) -> String {
    named(&lab().join(p))
}

/// pandocrs in `work` with the environment given (and a user data
/// directory of the lab's); (status, stdout, stderr).
fn pandocrs(args: &[&str], input: &str, env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pandocrs"));
    cmd.args(args)
        .current_dir(lab().join("work"))
        .env_remove("LIBPANDOC_WASM_TIMEOUT")
        .env_remove("LIBPANDOC_WASM_MAX_MEMORY")
        .env_remove("LIBPANDOC_SANDBOX")
        .env_remove("LIBPANDOC_SANDBOX_WRITE")
        .env_remove("PANDOC_DATA_DIRS")
        .env("XDG_DATA_HOME", path("data"))
        .env("APPDATA", path("data"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let s = |b: Vec<u8>| String::from_utf8_lossy(&b).replace("\r\n", "\n");
    (
        out.status.code().unwrap_or(-1),
        s(out.stdout),
        s(out.stderr),
    )
}

#[cfg(feature = "wasm")]
#[test]
fn reads_only_inside() {
    let env = [("LIBPANDOC_SANDBOX", ".")];
    std::fs::write(
        lab().join("work/doc.tex"),
        "\\input{inside}\n\n\\input{../secret/secret}\n",
    )
    .unwrap();
    let (status, out, err) = pandocrs(&["doc.tex", "-t", "plain"], "", &env);
    assert_eq!(status, 0, "{err}");
    assert!(
        out.contains("INSIDE-OK") && !out.contains("CANARY"),
        "{out}"
    );
    assert!(
        err.contains("[WARNING] Could not load include file ../secret/secret"),
        "{err}"
    );
    // standard input, as pandoc reads it
    let (status, out, _) = pandocrs(&["-f", "latex", "-t", "plain"], "\\input{inside}", &env);
    assert_eq!((status, out.trim()), (0, "INSIDE-OK"));
    // an input file outside: as pandoc fails for a file it can't read
    let secret = inside("secret/secret.tex");
    let (status, out, err) = pandocrs(&[&secret, "-t", "plain"], "", &env);
    assert!(
        status == 1 && out.is_empty() && err.contains("does not exist"),
        "{err}"
    );
    // no directory at all: the input only
    let (status, out, _) = pandocrs(
        &["-f", "latex", "-t", "plain"],
        "\\input{inside} x",
        &[("LIBPANDOC_SANDBOX", "")],
    );
    assert_eq!((status, out.trim()), (0, "x"));
    // informational options run as pandoc's
    let (status, out, _) = pandocrs(&["--version"], "", &env);
    assert!(status == 0 && out.contains("pandoc"), "{out}");
}

/// Outputs go into the write list only: `-o` there works (by an absolute
/// path, and by a relative one from a writable working directory), in a
/// read-only directory it fails.
#[cfg(feature = "wasm")]
#[test]
fn writes_only_where_listed() {
    let out = path("out");
    let html = inside("out/doc.html");
    let env = [
        ("LIBPANDOC_SANDBOX", "."),
        ("LIBPANDOC_SANDBOX_WRITE", out.as_str()),
    ];
    let (status, _, err) = pandocrs(&["doc.md", "-o", &html], "", &env);
    assert_eq!(status, 0, "{err}");
    let html = std::fs::read_to_string(lab().join("out/doc.html")).unwrap();
    assert_eq!(html.replace("\r\n", "\n"), "<p><em>hi</em></p>\n");
    let (status, _, err) = pandocrs(
        &["doc.md", "-o", "doc.html"],
        "",
        &[("LIBPANDOC_SANDBOX_WRITE", ".")],
    );
    assert_eq!(status, 0, "{err}");
    assert!(lab().join("work/doc.html").exists());
    let (status, _, err) = pandocrs(
        &["doc.md", "-o", "x.html"],
        "",
        &[("LIBPANDOC_SANDBOX", ".")],
    );
    assert!(status != 0 && err.contains("permission denied"), "{err}");
    assert!(!lab().join("work/x.html").exists());
    // `..` from one mount into another: an absolute path works, this not
    let (status, _, err) = pandocrs(&["doc.md", "-o", "../out/rel.html"], "", &env);
    assert!(status != 0 && err.contains("permission denied"), "{err}");
}

/// The working directory outside the mounts: relative names find nothing,
/// and the error says why.
#[cfg(feature = "wasm")]
#[test]
fn the_working_directory_must_be_listed() {
    let out = path("out");
    let (status, _, err) = pandocrs(&["doc.md"], "", &[("LIBPANDOC_SANDBOX", out.as_str())]);
    assert!(
        status != 0 && err.contains("is not in the sandbox"),
        "{err}"
    );
}

/// The user data directory and `PANDOC_DATA_DIRS` are readable, and not
/// writable.
#[cfg(feature = "wasm")]
#[test]
fn the_data_directories() {
    let dirs = path("dirs");
    let env = [
        ("LIBPANDOC_SANDBOX", "."),
        ("PANDOC_DATA_DIRS", dirs.as_str()),
    ];
    for (template, ok) in [
        ("mine.html", "DATA-DIR-OK"),
        ("theirs.html", "DATA-DIRS-OK"),
    ] {
        let (status, out, err) = pandocrs(&["doc.md", "-s", "--template", template], "", &env);
        assert!(status == 0 && out.contains(ok), "{template}: {out} {err}");
    }
    let lua = format!(
        "function Pandoc(d)\n  for _, f in ipairs({{'{}', '{}'}}) do\n    local h, e = io.open(f, 'w')\n    if h then error('wrote ' .. f) end\n  end\nend\n",
        inside("data/pandoc/new.txt"),
        inside("dirs/new.txt")
    );
    std::fs::write(lab().join("work/write-data.lua"), lua).unwrap();
    let (status, _, err) = pandocrs(&["doc.md", "-L", "write-data.lua"], "", &env);
    assert_eq!(status, 0, "{err}");
    assert!(!lab().join("data/pandoc/new.txt").exists() && !lab().join("dirs/new.txt").exists());
}

#[cfg(feature = "wasm")]
#[test]
fn refuses_what_it_cant_confine() {
    let env = [("LIBPANDOC_SANDBOX", ".")];
    let (status, _, err) = pandocrs(
        &["-t", "plain"],
        "x",
        &[("LIBPANDOC_SANDBOX", "no-such-dir")],
    );
    assert!(status != 0 && err.contains("no-such-dir"), "{err}");
    // a program: a JSON filter, a PDF engine
    let (status, _, err) = pandocrs(&["-t", "plain", "-F", "some-program"], "x", &env);
    assert!(
        status == 83 && err.contains("runs no programs: not JSON filters"),
        "{err}"
    );
    let (status, _, err) = pandocrs(&["-t", "pdf"], "x", &env);
    assert!(status != 0 && err.contains("PDF engines"), "{err}");
    // the filters this process would run, and pandoc's subcommands
    // (an empty module is a valid one)
    std::fs::write(lab().join("work/empty.wasm"), b"\0asm\x01\0\0\0").unwrap();
    let (status, _, err) = pandocrs(&["-t", "plain", "-F", "empty.wasm"], "x", &env);
    assert!(status != 0 && err.contains("wasm filters"), "{err}");
    let (status, _, err) = pandocrs(&["lua", "-e", "print(1)"], "", &env);
    assert!(status != 0 && err.contains("subcommands"), "{err}");
}

/// The study's two documents that exhaust a conversion: Typst's endless
/// loop (3 GB in 4.5 s natively) and 100,000 nested block quotes (over 15
/// s), stopped by the limits. The time limit is checked as the conversion
/// reports it, from the module's start: starting the process and loading
/// the compiled module come before, and are not limited.
#[cfg(feature = "wasm")]
#[test]
fn the_limits_stop_denial_of_service() {
    let (status, _, err) = pandocrs(
        &["-f", "typst", "-t", "plain"],
        "#while true {}",
        &[
            ("LIBPANDOC_SANDBOX", "."),
            ("LIBPANDOC_WASM_MAX_MEMORY", "512m"),
        ],
    );
    assert!(status != 0 && err.contains("out of memory"), "{err}");
    let (status, _, err) = pandocrs(
        &["-f", "markdown", "-t", "html"],
        &">".repeat(100_000),
        &[("LIBPANDOC_SANDBOX", "."), ("LIBPANDOC_WASM_TIMEOUT", "2")],
    );
    let after: f64 = err
        .split_once("stopped after ")
        .and_then(|(_, rest)| rest.split(' ').next()?.parse().ok())
        .unwrap_or_else(|| panic!("{err}"));
    assert!(status != 0 && (2.0..4.0).contains(&after), "{err}");
    // within the same limits, an ordinary document converts
    let (status, out, err) = pandocrs(
        &["-f", "markdown", "-t", "html"],
        "*hi*",
        &[
            ("LIBPANDOC_SANDBOX", "."),
            ("LIBPANDOC_WASM_TIMEOUT", "10"),
            ("LIBPANDOC_WASM_MAX_MEMORY", "512m"),
        ],
    );
    assert_eq!((status, out.as_str()), (0, "<p><em>hi</em></p>\n"), "{err}");
}

/// Without the `wasm` feature, the setting is refused, never ignored.
#[cfg(not(feature = "wasm"))]
#[test]
fn without_wasm_the_sandbox_is_refused() {
    for var in ["LIBPANDOC_SANDBOX", "LIBPANDOC_SANDBOX_WRITE"] {
        let (status, out, err) = pandocrs(&["-t", "plain"], "x", &[(var, ".")]);
        assert!(
            status != 0 && out.is_empty() && err.contains("wasm"),
            "{var}: {err}"
        );
    }
}
