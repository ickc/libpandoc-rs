//! What a callback reads through pandoc with its conversion
//! ([`libpandoc::read_as`], [`libpandoc::read_many_as`],
//! [`libpandoc::read_options`]) is read in pandoc's own sandbox when the
//! conversion is sandboxed, however the sandbox was asked for and however
//! the filter was given; and when that can't be told (the command's
//! conversion, [`libpandoc::main`]; one made by hand or from the
//! environment).
//!
//! The probe: a callback reading LaTeX, RST and Org fragments that include
//! a canary inside a directory (`INSIDE-OK`) and one outside it (`CANARY`),
//! and saying which it got ("latex:IO": both).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use libpandoc::{Conversion, Filter};
use panir::{Ctx, Pandoc};
use serde_json::{Value, json};

const OPEN: &str = "latex:IO rst:IO org:IO";
const CONFINED: &str = "latex:-- rst:-- org:--";

/// `inside` and `outside`, each with a canary of each format; `work`, for
/// the command's input and output.
fn lab() -> &'static PathBuf {
    static LAB: OnceLock<PathBuf> = OnceLock::new();
    LAB.get_or_init(|| {
        let root =
            Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("confined-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["inside", "outside", "work"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let root = match cfg!(windows) {
            true => std::path::absolute(&root),
            false => root.canonicalize(),
        }
        .unwrap();
        for ext in ["tex", "rst", "org"] {
            std::fs::write(root.join(format!("inside/in.{ext}")), "INSIDE-OK\n").unwrap();
            std::fs::write(root.join(format!("outside/out.{ext}")), "CANARY\n").unwrap();
        }
        root
    })
}

fn dir(d: &str) -> String {
    lab().join(d).display().to_string().replace('\\', "/")
}

/// What the probe got through `conversion`.
fn seen(conversion: &Conversion) -> String {
    let (i, o) = (dir("inside"), dir("outside"));
    let fragments = [
        (
            "latex",
            format!("\\input{{{i}/in.tex}}\n\n\\input{{{o}/out.tex}}\n"),
        ),
        (
            "rst",
            format!(".. include:: {i}/in.rst\n\n.. include:: {o}/out.rst\n"),
        ),
        (
            "org",
            format!("#+INCLUDE: \"{i}/in.org\"\n\n#+INCLUDE: \"{o}/out.org\"\n"),
        ),
    ];
    fragments
        .iter()
        .map(|(fmt, text)| {
            let options = libpandoc::read_options(conversion, Some(fmt));
            let doc = libpandoc::read_many(&[text], &options)
                .unwrap()
                .all()
                .unwrap();
            let s = panir::to_string(&doc[0]);
            let has = |c: &str, m: char| if s.contains(c) { m } else { '-' };
            format!("{fmt}:{}{}", has("INSIDE-OK", 'I'), has("CANARY", 'O'))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The probe as a filter: by [`Filter::func`], [`Filter::panir`],
/// [`Filter::raw`], or a filter's own conversion (with the options to read
/// as its conversion does) whose filter reads.
fn probe<'a>(way: &str, got: &'a Mutex<Vec<String>>) -> Filter<'a> {
    let note = move |c: &Conversion| got.lock().unwrap().push(seen(c));
    match way {
        "func" => Filter::func(move |_, c| {
            note(c);
            Ok(())
        }),
        "panir" => {
            struct P<'a>(Box<dyn Fn(&Conversion) + 'a>);
            impl panir::Filter for P<'_> {
                fn pandoc(&mut self, _: &mut Pandoc, ctx: &mut Ctx) -> Option<()> {
                    (self.0)(ctx.conversion());
                    None
                }
            }
            Filter::panir(P(Box::new(note)))
        }
        "raw" => Filter::raw(move |doc, c| {
            note(c);
            Ok(doc.to_vec())
        }),
        "nested" => Filter::func(move |_, c| {
            let mut options = libpandoc::read_options(c, None);
            options["to"] = "json".into();
            libpandoc::convert_with(
                &options,
                Some(b"x"),
                vec![Filter::func(|_, inner| {
                    note(inner);
                    Ok(())
                })],
            )?;
            Ok(())
        }),
        _ => unreachable!(),
    }
}

const WAYS: [&str; 4] = ["func", "panir", "raw", "nested"];

fn with(options: Value, way: &str) -> String {
    let got = Mutex::new(Vec::new());
    let mut o = json!({"from": "markdown", "to": "plain"});
    o.as_object_mut()
        .unwrap()
        .extend(options.as_object().unwrap().clone());
    libpandoc::convert_with(&o, Some(b"x"), vec![probe(way, &got)]).unwrap();
    got.into_inner().unwrap().remove(0)
}

#[test]
fn unsandboxed_a_callback_reads_files() {
    for way in WAYS {
        assert_eq!(with(json!({}), way), OPEN, "{way}");
        assert_eq!(with(json!({"sandbox": false}), way), OPEN, "{way}");
    }
}

#[test]
fn pandocs_sandbox() {
    for way in WAYS {
        assert_eq!(with(json!({"sandbox": true}), way), CONFINED, "{way}");
    }
}

/// In process, outside the module: in pandoc's own sandbox, no files, not
/// even inside the directory listed; and a callback gets the options (with
/// the sandbox), as natively.
#[cfg(feature = "wasm")]
#[test]
fn the_directory_sandbox() {
    let sandbox = json!({"sandbox": {"read": [dir("inside")]}});
    for way in WAYS {
        assert_eq!(with(sandbox.clone(), way), CONFINED, "{way}");
    }
    let mut options = None;
    libpandoc::convert_with(
        &json!({"to": "plain", "sandbox": {"read": [dir("inside")]}}),
        Some(b"x"),
        vec![Filter::func(|_, c| {
            options = c.options.clone();
            Ok(())
        })],
    )
    .unwrap();
    let options = options.expect("the options");
    assert_eq!(options["to"], "plain");
    assert!(options["sandbox"].is_object(), "{options}");
}

/// A filter that wants the files read gives its own options: its code.
#[test]
fn the_authors_own_options_win() {
    let mut text = String::new();
    libpandoc::convert_with(
        &json!({"to": "plain", "sandbox": true}),
        Some(b"x"),
        vec![Filter::func(|_, c| {
            let mut options = libpandoc::read_options(c, Some("latex"));
            options["sandbox"] = false.into();
            let input = format!("\\input{{{}/out.tex}}", dir("outside"));
            let docs = libpandoc::read_many(&[input], &options)?.all()?;
            text = panir::to_string(&docs[0]);
            Ok(())
        })],
    )
    .unwrap();
    assert!(text.contains("CANARY"), "{text}");
}

#[test]
fn a_conversion_without_options_cant_tell() {
    let read = |c: &Conversion| libpandoc::read_options(c, None);
    assert_eq!(read(&Conversion::default())["sandbox"], true);
    let known = Conversion {
        options: Some(json!({})),
        ..Conversion::default()
    };
    assert!(read(&known).get("sandbox").is_none());
    let dirs = Conversion {
        options: Some(json!({"sandbox": {"read": []}})),
        ..Conversion::default()
    };
    assert_eq!(read(&dirs)["sandbox"], true);
    assert_eq!(seen(&known), OPEN);
    assert_eq!(seen(&Conversion::default()), CONFINED);
}

#[test]
fn read_options_in_another_format() {
    let c = Conversion {
        input_format: Some("commonmark_x".into()),
        options: Some(json!({"tab-stop": 8, "to": "html"})),
        ..Conversion::default()
    };
    assert_eq!(
        libpandoc::read_options(&c, None),
        json!({"from": "commonmark_x", "tab-stop": 8})
    );
    assert_eq!(
        libpandoc::read_options(&c, Some("latex")),
        json!({"from": "latex", "tab-stop": 8})
    );
}

/// The command ([`libpandoc::main`], pandocrs): pandoc doesn't tell its
/// filters about `--sandbox` (in the arguments or a defaults file), so a
/// callback's reads are always confined; in the directory sandbox too. Run
/// in a child process of this test program, which sets the environment.
#[test]
fn the_command_cant_tell() {
    let cases: &[(&[&str], Option<String>)] = &[
        (&[], None),
        (&["--sandbox"], None),
        #[cfg(feature = "wasm")]
        (&[], Some(dir("inside"))),
    ];
    for (args, sandbox) in cases {
        let out = lab().join(format!("out-{}-{}.txt", args.len(), sandbox.is_some()));
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "command_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .current_dir(lab().join("work"))
        .env("CONFINED_CHILD", &out)
        .env("CONFINED_ARGS", args.join(" "))
        .env_remove("LIBPANDOC_SANDBOX")
        .env_remove("LIBPANDOC_SANDBOX_WRITE");
        if let Some(d) = sandbox {
            cmd.env("LIBPANDOC_SANDBOX", d)
                .env("LIBPANDOC_SANDBOX_WRITE", lab().join("work"));
        }
        let r = cmd.output().unwrap();
        assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
        let got = std::fs::read_to_string(&out).unwrap();
        assert_eq!(got, CONFINED, "{args:?} {sandbox:?}");
    }
}

/// The child of [`the_command_cant_tell`]: `main` with the probe as its
/// filter, the result in the file `CONFINED_CHILD` names.
#[test]
fn command_child() {
    let Some(out) = std::env::var_os("CONFINED_CHILD") else {
        return;
    };
    // (relative names: the working directory is `work`, listed in the
    // directory sandbox, where an absolute name with a drive letter would
    // mean nothing)
    std::fs::write("in.md", "x").unwrap();
    let mut args = vec!["pandocrs".to_string()];
    args.extend(
        std::env::var("CONFINED_ARGS")
            .unwrap()
            .split_whitespace()
            .map(String::from),
    );
    args.extend(["in.md", "-t", "plain", "-o", "doc.txt"].map(String::from));
    let got = Mutex::new(Vec::new());
    let status = libpandoc::main(&args, Some(vec![probe("func", &got)])).unwrap();
    assert_eq!(status, 0);
    std::fs::write(out, got.into_inner().unwrap().remove(0)).unwrap();
}
