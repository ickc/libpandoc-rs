//! pandocrs with `LIBPANDOC_SANDBOX` and `LIBPANDOC_SANDBOX_WRITE`: the
//! conversion in libpandoc.wasm, which sees only the directories listed
//! (the user data directories too, if listed), within the time and memory
//! limits.

use std::io::{Read, Write};
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
/// directory of the lab's, and a time limit of 60 s unless given);
/// (status, stdout, stderr). A run past [`HANG`] is killed, and fails the
/// test with what it wrote to standard error: never a CI job of hours.
fn pandocrs(args: &[&str], input: &str, env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pandocrs"));
    cmd.args(args)
        .current_dir(lab().join("work"))
        .env("LIBPANDOC_WASM_TIMEOUT", "60")
        .env_remove("LIBPANDOC_WASM_MAX_MEMORY")
        .env_remove("LIBPANDOC_SANDBOX")
        .env_remove("LIBPANDOC_SANDBOX_WRITE")
        .env_remove("LIBPANDOC_WASM_DIRS")
        .env_remove("LIBPANDOC_WASM_ENV")
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
    // each pipe on a thread of its own: a child that stops reading its
    // input, or fills a pipe, can't block this one
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_owned();
    std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let read = |mut r: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = r.read_to_end(&mut b);
            String::from_utf8_lossy(&b).replace("\r\n", "\n")
        })
    };
    let out = read(Box::new(child.stdout.take().unwrap()));
    let err = read(Box::new(child.stderr.take().unwrap()));
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > HANG {
            // where it is stuck: every thread's stack (macOS's `sample`)
            let stacks = Command::new("sample")
                .args([&child.id().to_string(), "2", "-file", "/dev/stdout"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            eprintln!("{stacks}");
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "pandocrs {args:?} {env:?} still running after {HANG:?}: killed; its standard \
                 error:\n{}",
                err.join().unwrap()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let (out, err) = (out.join().unwrap(), err.join().unwrap());
    (status.code().unwrap_or(-1), out, err)
}

/// How long a run of pandocrs may take before it counts as hung: its own
/// time limit (60 s) and the start, loading the module, generously.
const HANG: std::time::Duration = std::time::Duration::from_secs(180);

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

/// The user data directory and `PANDOC_DATA_DIRS` are not mounted by
/// themselves: pandoc finds nothing there unless they are listed, and then
/// they are readable and not writable.
#[cfg(feature = "wasm")]
#[test]
fn the_data_directories() {
    let dirs = path("dirs");
    let templates = [
        ("mine.html", "DATA-DIR-OK"),
        ("theirs.html", "DATA-DIRS-OK"),
    ];
    // not listed: as if they were not there
    let env = [
        ("LIBPANDOC_SANDBOX", "."),
        ("PANDOC_DATA_DIRS", dirs.as_str()),
    ];
    for (template, ok) in templates {
        let (status, out, err) = pandocrs(&["doc.md", "-s", "--template", template], "", &env);
        assert!(status != 0 && !out.contains(ok), "{template}: {out} {err}");
    }
    // listed, to read
    let listed = std::env::join_paths([".".into(), path("data/pandoc"), dirs.clone()]).unwrap();
    let listed = listed.to_str().unwrap();
    let env = [
        ("LIBPANDOC_SANDBOX", listed),
        ("PANDOC_DATA_DIRS", dirs.as_str()),
    ];
    for (template, ok) in templates {
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

/// `SOURCE_DATE_EPOCH` reaches pandoc inside: two conversions to docx and
/// odt, seconds apart, are the same bytes (without it they differ).
#[cfg(feature = "wasm")]
#[test]
fn source_date_epoch_makes_the_output_reproducible() {
    let out = path("out");
    let write = [
        ("LIBPANDOC_SANDBOX", "."),
        ("LIBPANDOC_SANDBOX_WRITE", out.as_str()),
        ("SOURCE_DATE_EPOCH", "1700000000"),
    ];
    for ext in ["docx", "odt"] {
        let mut made = Vec::new();
        for n in 0..2 {
            let file = inside(&format!("out/dated-{n}.{ext}"));
            let (status, _, err) = pandocrs(&["doc.md", "-o", &file], "", &write);
            assert_eq!(status, 0, "{ext}: {err}");
            made.push(std::fs::read(lab().join(format!("out/dated-{n}.{ext}"))).unwrap());
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }
        assert!(made[0] == made[1], "{ext}: two runs differ");
    }
}

/// `..` that stays inside one mounted directory resolves; one that leaves
/// it does not, even towards another mounted directory (documented).
#[cfg(feature = "wasm")]
#[test]
fn dot_dot_inside_a_mount() {
    std::fs::create_dir_all(lab().join("work/sub")).unwrap();
    std::fs::write(lab().join("work/sub/t.html"), "DOTDOT-OK $body$").unwrap();
    let env = [("LIBPANDOC_SANDBOX", ".")];
    let (status, out, err) = pandocrs(
        &["sub/../doc.md", "-s", "--template", "sub/../sub/t.html"],
        "",
        &env,
    );
    assert!(status == 0 && out.contains("DOTDOT-OK"), "{out} {err}");
    let listed = std::env::join_paths([".".to_string(), path("out")]).unwrap();
    let env = [("LIBPANDOC_SANDBOX", listed.to_str().unwrap())];
    let (status, out, _) = pandocrs(&["../work/doc.md"], "", &env);
    assert!(status != 0 && out.is_empty(), "{out}");
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
    assert!(status == 83 && err.contains("runs no programs"), "{err}");
    let (status, _, err) = pandocrs(&["-t", "pdf"], "x", &env);
    assert!(status != 0 && err.contains("pdflatex"), "{err}");
    // pandoc's subcommands aren't in the module
    let (status, _, err) = pandocrs(&["lua", "-e", "print(1)"], "", &env);
    assert_eq!(status, 4, "{err}");
}

/// The command is the module's pandoc: its messages, as the options ask
/// (`--quiet`, `--verbose`, `--log`, `--fail-if-warnings`), and its exit
/// statuses, the native command's.
#[cfg(feature = "wasm")]
#[test]
fn pandocs_own_messages_and_statuses() {
    std::fs::write(
        lab().join("work/warns.tex"),
        "\\input{../secret/secret}\n\nx\n",
    )
    .unwrap();
    let out = path("out");
    let env = [
        ("LIBPANDOC_SANDBOX", "."),
        ("LIBPANDOC_SANDBOX_WRITE", out.as_str()),
    ];
    let warning = "[WARNING] Could not load include file";
    let (status, _, err) = pandocrs(&["warns.tex", "-t", "plain"], "", &env);
    assert!(status == 0 && err.contains(warning), "{err}");
    let (status, _, err) = pandocrs(&["warns.tex", "-t", "plain", "--quiet"], "", &env);
    assert!(status == 0 && !err.contains("WARNING"), "{err}");
    let (status, _, err) = pandocrs(&["warns.tex", "-s", "-t", "html", "--verbose"], "", &env);
    assert!(status == 0 && err.contains("[INFO]"), "{err}");
    let log = inside("out/warns.log");
    let (status, _, err) = pandocrs(&["warns.tex", "-t", "plain", "--log", &log], "", &env);
    assert_eq!(status, 0, "{err}");
    let log = std::fs::read_to_string(lab().join("out/warns.log")).unwrap();
    assert!(log.contains("CouldNotLoadIncludeFile"), "{log}");
    let (status, _, err) = pandocrs(
        &["warns.tex", "-t", "plain", "--fail-if-warnings"],
        "",
        &env,
    );
    assert!(status == 3 && err.contains(warning), "{err}");
    // as the native command's
    for args in [
        &["missing.md"][..],
        &["-f", "nope", "doc.md"],
        &["-t", "nope", "doc.md"],
        &["--no-such-option"],
    ] {
        let (native, _, _) = pandocrs(args, "", &[]);
        let (status, _, err) = pandocrs(args, "", &env);
        assert!(
            native != 0 && status == native,
            "{args:?}: {native} {status} {err}"
        );
    }
}

/// A wasm filter named on the command line runs in this process, seeing
/// its own directories (`LIBPANDOC_WASM_DIRS`'s), not the mounts.
#[cfg(feature = "wasm")]
#[test]
fn a_wasm_filter_on_the_command_line() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../filters");
    let built = Command::new(env!("CARGO"))
        .args(["build", "--release", "--target", "wasm32-wasip1", "--quiet"])
        .args(["--bin", "include", "--bin", "upper"])
        .current_dir(&root)
        .status()
        .unwrap()
        .success();
    assert!(
        built,
        "building the example filters (rustup target add wasm32-wasip1)"
    );
    let dir = root.join("target/wasm32-wasip1/release");
    let (include, upper) = (dir.join("include.wasm"), dir.join("upper.wasm"));
    let (include, upper) = (include.to_str().unwrap(), upper.to_str().unwrap());
    let doc = "```include\ninside.tex\n```\n";
    let args = ["-t", "plain", "-F", include, "-F", upper];
    let (status, out, err) = pandocrs(&args, doc, &[("LIBPANDOC_SANDBOX", ".")]);
    assert_eq!(status, 83, "{out} {err}");
    assert!(
        err.contains("(it was given no directory: LIBPANDOC_WASM_DIRS)"),
        "{err}"
    );
    let env = [("LIBPANDOC_SANDBOX", "."), ("LIBPANDOC_WASM_DIRS", ".")];
    let (status, out, err) = pandocrs(&args, doc, &env);
    assert_eq!((status, out.trim()), (0, "INSIDE-OK"), "{err}");
    let doc = "```include\n../secret/secret.tex\n```\n";
    let (status, out, err) = pandocrs(&["-t", "plain", "-F", include], doc, &env);
    assert!(status == 83 && !out.contains("CANARY"), "{out} {err}");
    // the filter's directories, not the mounts: `secret` given to the
    // filter alone is read by it, though the sandbox lists only `.`
    let env = [
        ("LIBPANDOC_SANDBOX", "."),
        ("LIBPANDOC_WASM_DIRS", "../secret"),
    ];
    let (status, out, err) = pandocrs(&["-t", "plain", "-F", include], doc, &env);
    assert_eq!((status, out.trim()), (0, "CANARY-OUTSIDE"), "{err}");
}

/// The variables `LIBPANDOC_WASM_ENV` names reach a wasm filter in the
/// directory sandbox too, and not pandoc inside it: a Lua filter there sees
/// the environment the sandbox gives (its `TMPDIR`), not them.
#[cfg(feature = "wasm")]
#[test]
fn the_variables_named_reach_wasm_filters_only() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../filters");
    let built = Command::new(env!("CARGO"))
        .args(["build", "--release", "--target", "wasm32-wasip1", "--quiet"])
        .args(["--bin", "conversion"])
        .current_dir(&root)
        .status()
        .unwrap()
        .success();
    assert!(
        built,
        "building the example filters (rustup target add wasm32-wasip1)"
    );
    let conversion = root.join("target/wasm32-wasip1/release/conversion.wasm");
    std::fs::write(
        lab().join("work/env.lua"),
        "function Pandoc(doc)\n\
         doc.meta.granted = tostring(os.getenv('LIBPANDOC_TEST_GRANTED'))\n\
         doc.meta.tmpdir = tostring(os.getenv('TMPDIR'))\n\
         return doc\n\
         end\n",
    )
    .unwrap();
    let args = [
        "-t",
        "json",
        "-L",
        "env.lua",
        "-F",
        conversion.to_str().unwrap(),
    ];
    let env = [
        ("LIBPANDOC_SANDBOX", "."),
        ("LIBPANDOC_WASM_ENV", "LIBPANDOC_TEST_GRANTED"),
        ("LIBPANDOC_TEST_GRANTED", "granted"),
    ];
    let (status, out, err) = pandocrs(&args, "x", &env);
    assert_eq!(status, 0, "{err}");
    let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(doc["meta"]["granted"]["c"], "nil", "{out}");
    assert_ne!(doc["meta"]["tmpdir"]["c"], "nil", "{out}");
    let told = doc["blocks"].as_array().unwrap().last().unwrap()["c"][1]
        .as_str()
        .unwrap();
    let told: serde_json::Value = serde_json::from_str(told).unwrap();
    assert_eq!(told["sandbox"], "true", "{told}");
    assert_eq!(
        told["test-env"]["LIBPANDOC_TEST_GRANTED"], "granted",
        "{told}"
    );
}

/// The study's two documents that exhaust a conversion, stopped by the
/// limits: Typst's endless loop (3 GB in 4.5 s natively) by the memory
/// limit, 100,000 nested block quotes (over 15 s natively) by the time
/// limit. The time is checked as the conversion reports it, from the
/// module's start: starting the process and loading the compiled module
/// come before, and are not limited. Within the same limits, an ordinary
/// document converts.
#[cfg(feature = "wasm")]
#[test]
fn the_memory_limit_stops_a_loop() {
    // A string doubled without end: every limit is reached in well under a
    // second. (Typst's bare `#while true {}` is no test of this: how fast
    // its memory grows varies from machine to machine and from run to
    // run: 512 MB in 5 s here, 13 s on CI's Linux, never within 60 s on
    // its Windows, where 128 MB was not reached either. It is the time
    // limit that stops that one.)
    let (status, _, err) = pandocrs(
        &["-f", "typst", "-t", "plain"],
        "#let s = \"xxxxxxxx\"\n#while true { s = s + s }\n",
        &[
            ("LIBPANDOC_SANDBOX", "."),
            ("LIBPANDOC_WASM_MAX_MEMORY", "128m"),
        ],
    );
    assert!(status != 0 && err.contains("out of memory"), "{err}");
    let (status, out, err) = pandocrs(
        &["-f", "markdown", "-t", "html"],
        "*hi*",
        &[
            ("LIBPANDOC_SANDBOX", "."),
            ("LIBPANDOC_WASM_MAX_MEMORY", "128m"),
        ],
    );
    assert_eq!((status, out.as_str()), (0, "<p><em>hi</em></p>\n"), "{err}");
}

/// (See above.) The document is a file: standard input has a test of its
/// own.
#[cfg(feature = "wasm")]
#[test]
fn the_time_limit_stops_nested_quotes() {
    std::fs::write(lab().join("work/quotes.md"), ">".repeat(100_000)).unwrap();
    let (status, _, err) = pandocrs(
        &["quotes.md", "-t", "html"],
        "",
        &[("LIBPANDOC_SANDBOX", "."), ("LIBPANDOC_WASM_TIMEOUT", "2")],
    );
    let after: f64 = err
        .split_once("stopped after ")
        .and_then(|(_, rest)| rest.split(' ').next()?.parse().ok())
        .unwrap_or_else(|| panic!("{err}"));
    assert!(status != 0 && (2.0..4.0).contains(&after), "{err}");
    let (status, out, err) = pandocrs(
        &["doc.md", "-t", "html"],
        "",
        &[("LIBPANDOC_SANDBOX", "."), ("LIBPANDOC_WASM_TIMEOUT", "10")],
    );
    assert_eq!((status, out.as_str()), (0, "<p><em>hi</em></p>\n"), "{err}");
}

/// A document larger than a pipe holds, on standard input, read whole.
#[cfg(feature = "wasm")]
#[test]
fn a_large_standard_input() {
    let doc = "Some *words* here.\n\n".repeat(20_000);
    let (status, out, err) = pandocrs(
        &["-f", "markdown", "-t", "plain"],
        &doc,
        &[("LIBPANDOC_SANDBOX", ".")],
    );
    assert_eq!(status, 0, "{err}");
    assert_eq!(out.matches("Some words here.").count(), 20_000);
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
