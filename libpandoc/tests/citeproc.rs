//! citeproc for filters ([`libpandoc::citeproc`]): natively, pandoc's
//! citeproc, in its sandbox when the conversion is sandboxed (then reading
//! only the files the options name); in a wasm filter (the example filter
//! `citeproc`), the files the metadata names read by the filter itself, in
//! its directories, and given to pandoc as their contents
//! (`"citeproc-files"`, libpandoc 1.14). A canary bibliography outside
//! them, named by the metadata, never shows.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use libpandoc::{Error, Filter, Output};
use serde_json::{Value, json};

/// `files` (a bibliography, a CSL style and abbreviations) and `secret` (a
/// canary bibliography).
fn lab() -> &'static PathBuf {
    static LAB: OnceLock<PathBuf> = OnceLock::new();
    LAB.get_or_init(|| {
        let root =
            Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("citeproc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["files", "secret"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let files = [
            (
                "files/refs.bib",
                "@article{key, title={BIB-OK}, journal={Journal of Long Names}, \
                 author={Author, A.}, year={2000}}\n",
            ),
            ("files/style.csl", STYLE),
            (
                "files/abbr.json",
                r#"{"default": {"container-title": {"Journal of Long Names": "ABBR-OK"}}}"#,
            ),
            (
                "secret/canary.bib",
                "@book{key, title={CANARY-CITEPROC-7f3a}, author={B}, year={2001}}\n",
            ),
        ];
        for (f, text) in files {
            std::fs::write(root.join(f), text).unwrap();
        }
        root
    })
}

const CANARY: &str = "CANARY-CITEPROC-7f3a";

/// A style of its own: "(STYLE-OK title in journal)", the journal's short
/// form (from the abbreviations).
const STYLE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<style xmlns="http://purl.org/net/xbiblio/csl" class="in-text" version="1.0">
  <info><title>Test</title><id>test</id><updated>2026-01-01T00:00:00+00:00</updated></info>
  <citation><layout prefix="(" suffix=")"><text value="STYLE-OK "/><text variable="title"/><text value=" in "/><text variable="container-title" form="short"/></layout></citation>
  <bibliography><layout><text variable="title"/></layout></bibliography>
</style>
"#;

/// A path of the lab's, as YAML metadata names it natively (with `/`).
fn native(p: &str) -> String {
    lab().join(p).display().to_string().replace('\\', "/")
}

/// A document citing `key`, with this metadata (YAML lines).
fn doc(meta: &[(&str, String)]) -> String {
    let meta: String = meta.iter().map(|(k, v)| format!("{k}: '{v}'\n")).collect();
    format!("---\n{meta}---\n\nSee [@key].\n")
}

/// Whether the canary shows anywhere in a result.
fn leaks(r: &Result<Output, Error>) -> bool {
    match r {
        Ok(o) => o.text().contains(CANARY),
        Err(e) => e.message.contains(CANARY),
    }
}

/// A conversion to plain text with a filter in Rust running the helper.
fn with_helper(options: Value, input: &str) -> Result<Output, Error> {
    libpandoc::convert_with(
        &options,
        Some(input.as_bytes()),
        vec![Filter::func(|doc, conversion| {
            *doc = libpandoc::citeproc(doc, conversion)?;
            Ok(())
        })],
    )
}

#[test]
fn natively_it_is_pandocs_citeproc() {
    let input = doc(&[
        ("bibliography", native("files/refs.bib")),
        ("csl", native("files/style.csl")),
        ("citation-abbreviations", native("files/abbr.json")),
    ]);
    let out = with_helper(json!({"to": "plain"}), &input).unwrap();
    let text = out.text();
    for ok in ["STYLE-OK", "BIB-OK", "ABBR-OK"] {
        assert!(text.contains(ok), "{text}");
    }
    // not sandboxed, it reads what the document names: the canary too
    let input = doc(&[("bibliography", native("secret/canary.bib"))]);
    let out = with_helper(json!({"to": "plain"}), &input).unwrap();
    assert!(out.text().contains(CANARY), "{}", out.text());
}

/// Sandboxed, it reads no file the document names, only those the options
/// name, as the conversion's own citeproc would: a filter in Rust has the
/// process's access, which would let the document out of the sandbox.
#[test]
fn natively_sandboxed_it_reads_only_what_the_options_name() {
    let canary = doc(&[("bibliography", native("secret/canary.bib"))]);
    let mut sandboxes = vec![json!(true)];
    if cfg!(feature = "wasm") {
        sandboxes.push(json!({"read": [lab().join("secret")]}));
    }
    for sandbox in sandboxes {
        let r = with_helper(json!({"to": "plain", "sandbox": sandbox}), &canary);
        assert!(r.is_err() && !leaks(&r), "{sandbox}: {r:?}");
        let r = r.unwrap_err();
        assert!(r.message.contains("not found"), "{r}");
    }
    let named = doc(&[("bibliography", native("files/refs.bib"))]);
    let r = with_helper(json!({"to": "plain", "sandbox": true}), &named);
    assert!(r.is_err(), "{r:?}");
    let options = json!({"to": "plain", "sandbox": true, "bibliography": native("files/refs.bib")});
    let out = with_helper(options, "See [@key].\n").unwrap();
    assert!(out.text().contains("BIB-OK"), "{}", out.text());
}

#[cfg(feature = "wasm")]
mod wasm {
    use super::*;
    use libpandoc::wasm::WasmFilter;

    /// The example filter `citeproc`, built for wasm.
    fn citeproc() -> WasmFilter {
        static BUILT: OnceLock<PathBuf> = OnceLock::new();
        let path = BUILT.get_or_init(|| {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../filters");
            let ok = std::process::Command::new(env!("CARGO"))
                .args(["build", "--release", "--target", "wasm32-wasip1", "--quiet"])
                .args(["--bin", "citeproc"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success();
            assert!(
                ok,
                "building the example filters (rustup target add wasm32-wasip1)"
            );
            root.join("target/wasm32-wasip1/release/citeproc.wasm")
        });
        WasmFilter::from_file(path).unwrap()
    }

    /// A path of the lab's, as a wasm filter names it (on Windows `C:\x`
    /// is `/c/x`).
    fn named(p: &str) -> String {
        let s = native(p);
        let s = s.strip_prefix("//?/").unwrap_or(&s);
        match s.as_bytes() {
            [d, b':', ..] if cfg!(windows) => {
                format!("/{}{}", d.to_ascii_lowercase() as char, &s[2..])
            }
            _ => s.to_owned(),
        }
    }

    fn run(f: WasmFilter, options: Value, input: &str) -> Result<Output, Error> {
        libpandoc::convert_with(&options, Some(input.as_bytes()), vec![f.into_filter()])
    }

    /// The conversions it runs in: plain, in pandoc's sandbox, and in the
    /// directory sandbox (listing the canary's directory: pandoc could read
    /// it there, the filter can't).
    fn conversions() -> [Value; 3] {
        [
            json!({"to": "plain"}),
            json!({"to": "plain", "sandbox": true}),
            json!({"to": "plain", "sandbox": {"read": [lab().join("secret")]}}),
        ]
    }

    #[test]
    fn a_wasm_filter_reads_them_itself() {
        let files = lab().join("files");
        // by relative names, `csl: style` looked up as `style.csl`
        let relative = doc(&[
            ("bibliography", "refs.bib".into()),
            ("csl", "style".into()),
            ("citation-abbreviations", "abbr".into()),
        ]);
        // by absolute names, given as the variable gives them
        let absolute = doc(&[
            ("bibliography", named("files/refs.bib")),
            ("csl", named("files/style.csl")),
            ("citation-abbreviations", named("files/abbr.json")),
        ]);
        for options in conversions() {
            for (f, input) in [
                (citeproc().preopen(&files, ".", false), &relative),
                (citeproc().dirs([&files]), &absolute),
            ] {
                let out = run(f, options.clone(), input).unwrap();
                let text = out.text();
                for ok in ["STYLE-OK", "BIB-OK", "ABBR-OK"] {
                    assert!(text.contains(ok), "{options}: {text}");
                }
            }
        }
    }

    #[test]
    fn without_its_directory_they_are_missing() {
        let input = doc(&[("bibliography", named("files/refs.bib"))]);
        for options in conversions() {
            let e = run(citeproc().no_files(), options.clone(), &input).unwrap_err();
            assert_eq!(e.kind, "PandocFilterError");
            assert!(
                e.message
                    .contains("exited with status 1 (it was given no directory)"),
                "{options}: {e}"
            );
        }
    }

    /// The canary, outside the filter's directory, named by the metadata
    /// (absolute, and relative through `..`): never in the output or the
    /// error.
    #[test]
    fn the_canary_never_shows() {
        let files = lab().join("files");
        for input in [
            doc(&[("bibliography", named("secret/canary.bib"))]),
            doc(&[("bibliography", "../secret/canary.bib".into())]),
        ] {
            for options in conversions() {
                for f in [
                    citeproc().dirs([&files]),
                    citeproc().preopen(&files, ".", false),
                ] {
                    let r = run(f, options.clone(), &input);
                    assert!(r.is_err() && !leaks(&r), "{options}: {r:?}");
                }
            }
        }
    }
}
