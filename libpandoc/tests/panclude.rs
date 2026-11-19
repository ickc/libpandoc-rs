//! The include step built into libpandoc (1.15, panclude): `Filter::panclude`
//! and its roots, natively, under pandoc's sandbox, for untrusted options
//! and in the directory sandbox; and a wasm filter given no directory after
//! it, which sees the files' content, while its own calls to pandoc read
//! nothing.

use std::path::{Path, PathBuf};

use libpandoc::{Filter, Output, convert_with};
use serde_json::{Value, json};

/// A tree: `doc/` (the document's directory: `doc.md`, `part.txt`, and on
/// Unix `link.txt`, a symbolic link to the canary), `other/other.txt`, and
/// the canary `secret.txt` beside them.
fn tree(name: &str) -> PathBuf {
    let base =
        std::env::temp_dir().join(format!("libpandoc-panclude-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    for d in ["doc", "other"] {
        std::fs::create_dir_all(base.join(d)).unwrap();
    }
    for (f, text) in [
        ("doc/doc.md", DOC),
        ("doc/part.txt", "hello *from the part*\n"),
        ("other/other.txt", "OTHER-OK\n"),
        ("secret.txt", "CANARY-PANCLUDE\n"),
    ] {
        std::fs::write(base.join(f), text).unwrap();
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink("../secret.txt", base.join("doc/link.txt")).unwrap();
    // the real path (macOS's /var is /private/var), but not Windows' `\\?\`
    match cfg!(windows) {
        true => base,
        false => base.canonicalize().unwrap(),
    }
}

/// Code blocks naming a file inside the document's directory, one outside
/// it, and (on Unix) a link inside it that leads out.
const DOC: &str = "```{include=\"part.txt\"}\nOWN-PART\n```\n\n\
                   ```{include=\"../secret.txt\"}\nOWN-OUT\n```\n\n\
                   ```{include=\"link.txt\"}\nOWN-LINK\n```\n";

/// A path as a document names it natively: with `/`.
fn native(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

/// The step's warnings, as the log has them.
fn warnings(out: &Output) -> Vec<String> {
    out.log
        .iter()
        .filter(|m| m["type"] == "ScriptingWarning")
        .map(|m| m["message"].as_str().unwrap().to_owned())
        .collect()
}

/// `doc/doc.md` converted to plain text with `filters`; the output and the
/// step's warnings. Never the canary.
fn run(base: &Path, options: Value, filters: Vec<Filter<'_>>) -> (String, Vec<String>) {
    let mut options = options;
    options["input-files"] = json!([base.join("doc/doc.md")]);
    options["to"] = "plain".into();
    let out = convert_with(&options, None, filters).unwrap();
    let text = out.text().into_owned();
    let warned = warnings(&out);
    assert!(
        !text.contains("CANARY") && !format!("{warned:?}").contains("CANARY"),
        "{text}"
    );
    (text, warned)
}

/// The warning for an include that failed, as the specification words it.
fn cannot(path: &str, base: &Path, reason: &str) -> String {
    let doc = base.join("doc/doc.md").display().to_string();
    format!("panclude: cannot include \"{path}\" in {doc}: {reason}")
}

#[test]
fn by_default_inside_the_documents_directory() {
    let base = tree("default");
    let (text, warned) = run(&base, json!({}), vec![Filter::panclude()]);
    assert!(text.contains("hello *from the part*"), "{text}");
    assert!(text.contains("OWN-OUT"), "{text}");
    assert!(
        warned.contains(&cannot("../secret.txt", &base, "outside the roots")),
        "{warned:?}"
    );
    #[cfg(unix)]
    {
        assert!(text.contains("OWN-LINK"), "{text}");
        let why = "a symbolic link leads outside the roots";
        assert!(
            warned.contains(&cannot("link.txt", &base, why)),
            "{warned:?}"
        );
    }
    // off unless named
    let (text, warned) = run(&base, json!({}), vec![]);
    assert!(
        text.contains("OWN-PART") && !text.contains("hello") && warned.is_empty(),
        "{text} {warned:?}"
    );
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn roots_given() {
    let base = tree("roots");
    let doc = base.join("doc").display().to_string();
    // the document's directory and its parent: the canary is inside
    let parent = base.display().to_string();
    let (text, _) = {
        let mut options = json!({"input-files": [base.join("doc/doc.md")], "to": "plain"});
        options["filters"] = json!([{"type": "panclude", "roots": [&parent]}]);
        let out = libpandoc::convert(&options, None).unwrap();
        (out.text().into_owned(), warnings(&out))
    };
    assert!(
        text.contains("CANARY-PANCLUDE"),
        "a root given is read: {text}"
    );
    // another directory only: the document's own is no root then
    let other = base.join("other").display().to_string();
    let (text, warned) = run(&base, json!({}), vec![Filter::panclude_roots([&other])]);
    assert!(
        text.contains("OWN-PART") && !text.contains("hello"),
        "{text}"
    );
    assert!(
        warned.contains(&cannot("part.txt", &base, "outside the roots")),
        "{warned:?}"
    );
    let (text, _) = run(
        &base,
        json!({}),
        vec![Filter::panclude_roots([&doc, &other])],
    );
    assert!(text.contains("hello *from the part*"), "{text}");
    // none: nothing
    let none: [&str; 0] = [];
    let (text, warned) = run(&base, json!({}), vec![Filter::panclude_roots(none)]);
    assert!(
        text.contains("OWN-PART") && !text.contains("hello"),
        "{text}"
    );
    let why = "there are no roots, so no file is included";
    assert!(
        warned.contains(&cannot("part.txt", &base, why)),
        "{warned:?}"
    );
    // a root that isn't a directory: an error
    let file = base.join("secret.txt").display().to_string();
    let e = convert_with(
        &json!({"input-files": [base.join("doc/doc.md")], "to": "plain"}),
        None,
        vec![Filter::panclude_roots([file])],
    )
    .unwrap_err();
    assert_eq!(e.kind, "PandocOptionError", "{e}");
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn under_pandocs_sandbox_only_roots_named() {
    let base = tree("sandbox");
    let sandbox = json!({"sandbox": true});
    let (text, warned) = run(&base, sandbox.clone(), vec![Filter::panclude()]);
    assert!(
        text.contains("OWN-PART") && !text.contains("hello"),
        "{text}"
    );
    let why = "there are no roots, so no file is included";
    assert!(
        warned.contains(&cannot("part.txt", &base, why)),
        "{warned:?}"
    );
    let doc = base.join("doc").display().to_string();
    let (text, warned) = run(&base, sandbox, vec![Filter::panclude_roots([&doc])]);
    assert!(text.contains("hello *from the part*"), "{text}");
    assert!(
        warned.contains(&cannot("../secret.txt", &base, "outside the roots")),
        "{warned:?}"
    );
    std::fs::remove_dir_all(&base).unwrap();
}

/// Untrusted options may name the step, which reads only inside the roots
/// the host gives (`untrusted_with_roots`), and without them nothing; their
/// own roots, at the top or inside `"untrusted"`, are refused or written
/// over.
#[test]
fn untrusted_options_read_only_the_hosts_roots() {
    let base = tree("untrusted");
    let doc = base.join("doc");
    let input = "```{include=\"part.txt\"}\nOWN-PART\n```\n";
    let code = |extra: Value| {
        let mut o = json!({"from": "markdown", "to": "plain", "filters": ["panclude"]});
        o.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        o
    };
    // (the input is given, so the default would be the working directory:
    // a path naming the document's directory)
    let part = format!(
        "```{{include=\"{}\"}}\nOWN-PART\n```\n",
        native(&doc.join("part.txt"))
    );
    for input in [input, part.as_str()] {
        let opts = libpandoc::untrusted(code(json!({})), None).unwrap();
        let out = libpandoc::convert(&opts, Some(input.as_bytes())).unwrap();
        assert!(
            out.text().contains("OWN-PART") && !out.text().contains("hello"),
            "{}",
            out.text()
        );
        assert!(warnings(&out)[0].ends_with("there are no roots, so no file is included"));
        // the code's own "untrusted" is written over
        let theirs = code(json!({"untrusted": {"panclude-roots": [doc.display().to_string()]}}));
        let opts = libpandoc::untrusted(theirs, None).unwrap();
        let out = libpandoc::convert(&opts, Some(input.as_bytes())).unwrap();
        assert!(
            out.text().contains("OWN-PART") && !out.text().contains("hello"),
            "{}",
            out.text()
        );
    }
    // the host's roots
    let roots = [doc.display().to_string()];
    let opts = libpandoc::untrusted_with_roots(code(json!({})), None, &roots).unwrap();
    assert_eq!(opts["untrusted"], json!({"panclude-roots": roots}));
    let out = libpandoc::convert(&opts, Some(part.as_bytes())).unwrap();
    assert!(
        out.text().contains("hello *from the part*"),
        "{}",
        out.text()
    );
    // with a token too
    let t = libpandoc::CancelToken::new().unwrap();
    let opts = libpandoc::untrusted_with_roots(code(json!({})), Some(&t), &roots).unwrap();
    assert_eq!(
        opts["untrusted"],
        json!({"cancel-token": t.id(), "panclude-roots": roots})
    );
    let out = libpandoc::convert(&opts, Some(part.as_bytes())).unwrap();
    assert!(
        out.text().contains("hello *from the part*"),
        "{}",
        out.text()
    );
    // the code's own roots: refused
    for theirs in [
        code(json!({"panclude-roots": roots})),
        json!({"to": "plain", "filters": [{"type": "panclude", "roots": roots}]}),
        json!({"to": "plain", "filters": ["panclude", "other.lua"]}),
    ] {
        let opts = libpandoc::untrusted(theirs, None).unwrap();
        let e = libpandoc::convert(&opts, Some(part.as_bytes())).unwrap_err();
        assert_eq!(e.kind, "PandocOptionError", "{e}");
    }
    // citeproc may be named too (libpandoc 1.15)
    let opts = libpandoc::untrusted(json!({"to": "plain", "filters": ["citeproc"]}), None).unwrap();
    assert!(libpandoc::convert(&opts, Some(b"x")).is_ok());
    std::fs::remove_dir_all(&base).unwrap();
}

/// One of pandoc's own entries: it ends a run of filters of this process,
/// so the filter before it sees the element as the reader made it, and the
/// one after it the file's text.
#[test]
fn it_ends_a_run() {
    let base = tree("run");
    let seen = std::cell::RefCell::new(Vec::new());
    let look = || {
        Filter::func(|doc, _| {
            let json = panir::to_string(doc);
            seen.borrow_mut().push(json.contains("from the part"));
            Ok(())
        })
    };
    run(
        &base,
        json!({}),
        vec![look(), Filter::panclude(), look(), look()],
    );
    assert_eq!(*seen.borrow(), [false, true, true]);
    std::fs::remove_dir_all(&base).unwrap();
}

#[cfg(feature = "wasm")]
mod wasm {
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::OnceLock;

    use libpandoc::Filter;
    use libpandoc::wasm::WasmFilter;
    use serde_json::{Value, json};

    use std::path::Path;

    use super::{native, run, tree};

    /// A path as the module of the directory sandbox sees it: with `/`; on
    /// Windows `C:\x` is `/c/x`.
    fn named(p: &Path) -> String {
        let s = p.display().to_string().replace('\\', "/");
        let s = s.strip_prefix("//?/").unwrap_or(&s);
        match s.as_bytes() {
            [d, b':', ..] if cfg!(windows) => {
                format!("/{}{}", d.to_ascii_lowercase() as char, &s[2..])
            }
            _ => s.to_owned(),
        }
    }

    /// The example filters' directory of .wasm files, built once.
    fn filters() -> &'static PathBuf {
        static DIR: OnceLock<PathBuf> = OnceLock::new();
        DIR.get_or_init(|| {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../filters");
            let status = Command::new(env!("CARGO"))
                .args(["build", "--release", "--target", "wasm32-wasip1", "--quiet"])
                .current_dir(&root)
                .status()
                .expect("cargo");
            assert!(
                status.success(),
                "building the example filters (rustup target add wasm32-wasip1)"
            );
            root.join("target/wasm32-wasip1/release")
        })
    }

    /// An example filter given no directory.
    fn wasm(name: &str) -> Filter<'static> {
        WasmFilter::from_file(filters().join(format!("{name}.wasm")))
            .unwrap()
            .no_files()
            .into_filter()
    }

    /// The step first, then wasm filters given no directory: `parse` reads
    /// the code block's text as the document was read, `upper` upper-cases
    /// it. They see the file's content, after the block's own; without
    /// the step, never.
    #[test]
    fn a_wasm_filter_given_no_directory_sees_the_content() {
        let base = tree("wasm");
        let doc = "```{.parse include=\"part.txt\"}\nbefore the part:\n```\n";
        std::fs::write(base.join("doc/doc.md"), doc).unwrap();
        let filters = |step: bool| {
            let mut f = vec![wasm("parse"), wasm("upper")];
            if step {
                f.insert(0, Filter::panclude());
            }
            f
        };
        let (text, _) = run(&base, json!({}), filters(true));
        assert_eq!(text, "BEFORE THE PART: HELLO FROM THE PART\n");
        let (text, _) = run(&base, json!({}), filters(false));
        assert_eq!(text, "BEFORE THE PART:\n");
        // in pandoc's sandbox, with the root named; in the directory sandbox
        let root = base.join("doc").display().to_string();
        let sandbox = json!({"sandbox": true});
        let (text, _) = run(&base, sandbox, {
            let mut f = filters(false);
            f.insert(0, Filter::panclude_roots([root]));
            f
        });
        assert_eq!(text, "BEFORE THE PART: HELLO FROM THE PART\n");
        let mounts = json!({"sandbox": {"read": [base.join("doc")]}});
        let mut options = mounts;
        options["input-files"] = json!([named(&base.join("doc/doc.md"))]);
        options["to"] = "plain".into();
        let out = libpandoc::convert_with(&options, None, filters(true)).unwrap();
        assert_eq!(out.text(), "BEFORE THE PART: HELLO FROM THE PART\n");
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// In the directory sandbox the step reads inside its roots as the
    /// module sees them, and nothing outside the mounted directories: not
    /// by a path leaving them, nor through a link; a root wider than them is
    /// no directory there, an error.
    #[test]
    fn in_the_directory_sandbox_nothing_outside_the_mounts() {
        let base = tree("mounts");
        let doc = base.join("doc");
        let options = |filters: Value| {
            json!({
                "input-files": [named(&doc.join("doc.md"))],
                "to": "plain",
                "filters": filters,
                "sandbox": {"read": [&doc]},
            })
        };
        let convert = |filters: Value| {
            let out = libpandoc::convert(&options(filters), None).unwrap();
            let text = out.text().into_owned();
            let warned = super::warnings(&out);
            assert!(!text.contains("CANARY") && !format!("{warned:?}").contains("CANARY"));
            (text, warned)
        };
        // by default the document's directory, as the module names it
        let (text, warned) = convert(json!(["panclude"]));
        assert!(text.contains("hello *from the part*"), "{text}");
        let doc_md = named(&doc.join("doc.md"));
        let outside =
            format!("panclude: cannot include \"../secret.txt\" in {doc_md}: outside the roots");
        assert!(warned.contains(&outside), "{warned:?}");
        // a root named as the module sees it: not a path outside it, by
        // its absolute name or a link
        let secret = named(&base.join("secret.txt"));
        std::fs::write(
            doc.join("doc.md"),
            format!(
                "```{{include=\"{secret}\"}}\nOWN-ABS\n```\n\n{}",
                super::DOC
            ),
        )
        .unwrap();
        let (text, warned) = convert(json!([{"type": "panclude", "roots": [named(&doc)]}]));
        assert!(
            text.contains("OWN-ABS") && text.contains("OWN-OUT"),
            "{text}"
        );
        assert!(text.contains("hello *from the part*"), "{text}");
        assert!(warned.iter().any(|w| w.contains(&secret)), "{warned:?}");
        #[cfg(unix)]
        assert!(text.contains("OWN-LINK"), "{text}");
        // a root wider than the mounts is no directory there: an error
        let e = libpandoc::convert(
            &options(json!([{"type": "panclude", "roots": [named(&base)]}])),
            None,
        )
        .unwrap_err();
        assert_eq!(e.kind, "PandocOptionError", "{e}");
        assert!(e.message.contains("is not a directory"), "{e}");
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// A wasm filter's own calls to pandoc are untrusted, and the host
    /// gives them no roots: a call naming the step reads nothing, whatever
    /// it writes, natively and in the directory sandbox; roots of its own
    /// are refused.
    #[test]
    fn a_wasm_filters_calls_read_nothing() {
        let base = tree("calls");
        let doc = base.join("doc/doc.md");
        // (paths natively, and as the module sees them)
        for sandboxed in [false, true] {
            let path = |p: &Path| if sandboxed { named(p) } else { native(p) };
            let secret = path(&base.join("secret.txt"));
            let part = format!("```{{include=\"{secret}\"}}\nOWN\n```\n");
            let root = path(&base);
            let requests = [
                json!({"convert": [{"to": "plain", "filters": ["panclude"]}, part]}),
                json!({"convert": [{"to": "plain", "filters": ["panclude"],
                                    "untrusted": {"panclude-roots": [root]}}, part]}),
                json!({"convert": [{"to": "plain", "filters": ["panclude"], "untrusted": false}, part]}),
                json!({"convert": [{"to": "plain", "filters": ["panclude"], "panclude-roots": [root]}, part]}),
                json!({"convert": [{"to": "plain", "filters": [{"type": "panclude", "roots": [root]}]}, part]}),
            ];
            let input: String = requests
                .iter()
                .map(|r| format!("```call\n{r}\n```\n\n"))
                .collect();
            std::fs::write(&doc, input).unwrap();
            let mut options = json!({"input-files": [path(&doc)], "to": "json"});
            if sandboxed {
                options["sandbox"] = json!({"read": [&base]});
            }
            // the conversion's own step has a root holding the canary
            let step = Filter::panclude_roots([root.clone()]);
            let out = libpandoc::convert_with(&options, None, vec![step, wasm("calls")]).unwrap();
            assert!(!out.text().contains("CANARY"), "{options}: {}", out.text());
            let doc: Value = serde_json::from_slice(&out.bytes).unwrap();
            let a: Vec<Value> = doc["blocks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| serde_json::from_str(b["c"][1].as_str().unwrap()).unwrap())
                .collect();
            for x in &a[..3] {
                assert_eq!(
                    x["ok"].as_str().map(str::trim),
                    Some("OWN"),
                    "{options}: {x}"
                );
            }
            for x in &a[3..] {
                assert_eq!(x["error"][0], "PandocOptionError", "{options}: {x}");
            }
        }
        std::fs::remove_dir_all(&base).unwrap();
    }
}
