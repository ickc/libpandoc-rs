use std::time::{Duration, Instant};

use libpandoc::{Error, Filter, convert, convert_args, convert_with, read, read_many, write};
use panir::{Block, Inline};
use serde_json::json;

fn md_to(to: &str) -> serde_json::Value {
    json!({"from": "markdown", "to": to})
}

#[test]
fn converts() {
    let out = convert(&md_to("html"), Some(b"*hi*")).unwrap();
    assert_eq!(out.text(), "<p><em>hi</em></p>\n");
    let out = convert_args(&["-f", "markdown", "-t", "latex"], Some(b"*hi*")).unwrap();
    assert_eq!(out.text(), "\\emph{hi}\n");
}

/// A key whose value is null is absent (as Lua's nil in a table, as
/// libpandoc-python's None and LibPandoc.jl's nothing); a list with a null
/// in it stays an error.
#[test]
fn null_is_an_absent_option() {
    let options = json!({"from": "markdown", "to": null, "timeout": null, "sandbox": null,
                         "untrusted": null, "standalone": null, "filters": null});
    assert_eq!(
        convert(&options, Some(b"*x*")).unwrap().text(),
        "<p><em>x</em></p>\n"
    );
    assert!(convert_with(&options, Some(b"x"), vec![]).is_ok());
    let doc = read("x", "markdown").unwrap();
    assert_eq!(
        libpandoc::write_with(&doc, &json!({"to": "plain", "timeout": null}))
            .unwrap()
            .text(),
        "x\n"
    );
    let r = read_many(
        &["x"],
        &json!({"from": "markdown", "timeout": null, "sandbox": null}),
    );
    assert_eq!(r.unwrap().all().unwrap(), vec![doc]);
    assert!(convert(&json!({"filters": [null]}), Some(b"x")).is_err());
}

#[test]
fn errors_have_kinds() {
    let e = convert(&json!({"from": "nope", "to": "html"}), Some(b"")).unwrap_err();
    assert_eq!(e.kind, "PandocUnknownReaderError");
}

#[test]
fn warnings_are_logged() {
    let out = convert(&md_to("html"), Some(b"[x]\n\n[x]: a\n[x]: b\n")).unwrap();
    assert!(
        out.log
            .iter()
            .any(|m| m["type"] == "DuplicateLinkReference"),
        "{:?}",
        out.log
    );
}

#[test]
fn reads_and_writes() {
    let doc = read("# Title\n\nsome *text*", "markdown").unwrap();
    assert!(matches!(doc.blocks[0], Block::Header(..)));
    assert_eq!(
        write(&doc, "commonmark").unwrap().text(),
        "# Title\n\nsome *text*\n"
    );
    let docs = read_many(&["a", "*b*", "c"], &json!({"from": "commonmark"}))
        .unwrap()
        .all()
        .unwrap();
    assert_eq!(docs.len(), 3);
    assert_eq!(
        docs[1].blocks,
        vec![Block::Para(vec![Inline::Emph(vec![Inline::Str(
            "b".into()
        )])])]
    );
}

#[test]
fn read_many_keeps_each_inputs_result_and_the_log() {
    let read = read_many(
        &["\\input{no-such-file} x", "\\newcommand{\\a}{\\a}\\a", "y"],
        &json!({"from": "latex"}),
    )
    .unwrap();
    assert!(
        read.docs[0].is_ok() && read.docs[2].is_ok(),
        "{:?}",
        read.docs
    );
    let e = read.docs[1].as_ref().unwrap_err();
    assert!(e.message.starts_with("reading input 1:"), "{e}");
    assert!(
        read.log
            .iter()
            .any(|m| m["type"] == "CouldNotLoadIncludeFile"),
        "{:?}",
        read.log
    );
    assert!(read.all().is_err());
}

/// The study's 100,000 nested block quotes: over 15 s, natively.
fn slow() -> String {
    ">".repeat(100_000)
}

#[test]
fn a_call_has_a_time_limit() {
    let started = Instant::now();
    let e = convert(
        &json!({"from": "markdown", "to": "html", "timeout": 0.5}),
        Some(slow().as_bytes()),
    )
    .unwrap_err();
    assert!(e.is_timeout() && e.kind == Error::TIMEOUT, "{e}");
    assert!(started.elapsed() < Duration::from_secs(5));
    // in read_many, the inputs not read by then, in their places. (A longer
    // limit: the call's setup must end within it, or the whole call fails,
    // as documented; with 0.5 s it did once in libpandoc-python's twin of
    // this test, on a macOS Intel CI runner.)
    let started = Instant::now();
    let read = read_many(&["*a*", &slow(), "b"], &json!({"timeout": 3})).unwrap();
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(
        read.docs[0].is_ok() && read.docs[2].is_ok(),
        "{:?}",
        read.docs
    );
    assert!(read.docs[1].as_ref().unwrap_err().is_timeout());
    // and the next call works
    assert_eq!(
        convert(&md_to("html"), Some(b"*hi*")).unwrap().text(),
        "<p><em>hi</em></p>\n"
    );
}

#[test]
fn queries() {
    assert!(libpandoc::pandoc_version().unwrap().starts_with('3'));
    let formats = libpandoc::query("input-formats", json!(null)).unwrap();
    assert!(
        formats
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "commonmark_x")
    );
}

#[test]
fn filters_run_in_process_in_order() {
    let mut seen = Vec::new();
    let out = convert_with(
        &md_to("plain"),
        Some(b"one two"),
        vec![
            Filter::func(|doc, c| {
                seen.push(c.format.clone().unwrap());
                doc.blocks.push(Block::Para(panir::inlines("three")));
                Ok(())
            }),
            Filter::lua(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/upper.lua")),
        ],
    )
    .unwrap();
    assert_eq!(out.text(), "ONE TWO\n\nTHREE\n");
    assert_eq!(seen, ["plain"]);
}

#[test]
fn filters_see_the_conversion() {
    let mut told = None;
    convert_with(
        &json!({"from": "commonmark_x", "to": "html", "tab-stop": 2}),
        Some(b"x"),
        vec![Filter::func(|_, c| {
            told = Some(c.clone());
            Ok(())
        })],
    )
    .unwrap();
    let c = told.unwrap();
    assert_eq!(
        c.input_format
            .as_deref()
            .map(|f| f.starts_with("commonmark_x")),
        Some(true)
    );
    assert_eq!(c.options.as_ref().unwrap()["tab-stop"], 2);
    // a fragment read as the conversion reads
    let frag = libpandoc::read_as("~~x~~", &c).unwrap();
    assert!(matches!(&frag.blocks[0], Block::Para(i) if matches!(i[0], Inline::Strikeout(_))));
}

#[test]
fn filters_see_the_writer_options() {
    let mut told = None;
    convert_with(
        &json!({"from": "markdown", "to": "html", "number-sections": true, "toc-depth": 2}),
        Some(b"x"),
        vec![Filter::func(|_, c| {
            told = c.writer_options.clone();
            Ok(())
        })],
    )
    .unwrap();
    let w = told.expect("writer options (libpandoc 1.10)");
    assert_eq!(w["number-sections"], true, "{w}");
    assert_eq!(w["toc-depth"], 2, "{w}");
}

#[test]
fn the_number_of_threads() {
    let n = libpandoc::set_num_threads(2).unwrap();
    assert!(n >= 1);
    // (2, or the number in effect if pandoc's threads have grown already)
    assert_eq!(libpandoc::num_threads().unwrap(), n);
}

#[test]
fn a_filter_error_fails_the_conversion() {
    let e = convert_with(
        &md_to("html"),
        Some(b"x"),
        vec![Filter::func(|_, _| Err("no thanks".into()))],
    )
    .unwrap_err();
    assert_eq!(e.kind, "PandocFilterError");
    assert!(e.message.contains("no thanks"), "{}", e.message);
}

#[test]
#[should_panic(expected = "boom")]
fn a_filter_panic_is_raised_again() {
    let _ = convert_with(
        &md_to("html"),
        Some(b"x"),
        vec![Filter::func(|_, _| panic!("boom"))],
    );
}

// Consecutive filters on the document run in one exchange with pandoc.

/// `doc`'s blocks inside `n` block quotes, one in another.
fn nest(doc: &mut panir::Pandoc, n: usize) {
    for _ in 0..n {
        doc.blocks = vec![Block::BlockQuote(std::mem::take(&mut doc.blocks))];
    }
}

/// `doc`'s blocks out of the block quotes `nest` put them in.
fn unnest(doc: &mut panir::Pandoc) {
    while let [Block::BlockQuote(_)] = &doc.blocks[..] {
        if let Some(Block::BlockQuote(b)) = doc.blocks.pop() {
            doc.blocks = b;
        }
    }
}

/// Deeper than panir reads: what a filter of a run leaves nested so, the
/// next filter of the run gets as it is, where in an exchange of its own it
/// would fail to parse.
const DEEP: usize = panir::MAX_DEPTH + 10;

/// The same filters give the same output run together and run apart
/// (`"separate-filters": true`).
#[test]
fn a_run_of_filters_gives_what_they_give_apart() {
    let filters = || {
        vec![
            Filter::func(|doc, _| {
                doc.blocks.push(Block::Para(panir::inlines("two")));
                Ok(())
            }),
            Filter::panir(Upper),
            Filter::raw(|doc, _| {
                Ok(String::from_utf8_lossy(doc)
                    .replace("\"TWO\"", "\"Two\"")
                    .into_bytes())
            }),
            Filter::func(|doc, c| {
                let name = c.format.clone().unwrap_or_default();
                doc.blocks.push(Block::Para(panir::inlines(&name)));
                Ok(())
            }),
        ]
    };
    let together = convert_with(&md_to("plain"), Some(b"one"), filters()).unwrap();
    let mut apart = md_to("plain");
    apart["separate-filters"] = true.into();
    let apart = convert_with(&apart, Some(b"one"), filters()).unwrap();
    assert_eq!(together.text(), "ONE\n\nTwo\n\nplain\n");
    assert_eq!(together.text(), apart.text());
}

struct Upper;
impl panir::Filter for Upper {
    fn inline(&mut self, x: &mut Inline, _: &mut panir::Ctx) -> Option<Vec<Inline>> {
        if let Inline::Str(s) = x {
            *s = s.to_uppercase().into();
        }
        None
    }
}

/// One exchange for a run: the second filter gets the document the first
/// left, not one parsed again. A Lua filter between them, or
/// `"separate-filters": true`, makes two; a filter on JSON between them
/// gets the document written once, and the next parses what it wrote.
#[test]
fn a_run_is_one_exchange() {
    let run = |between: Option<Filter<'static>>, separate: bool| {
        let mut fs = vec![Filter::func(|doc, _| {
            nest(doc, DEEP);
            Ok(())
        })];
        fs.extend(between);
        fs.push(Filter::func(|doc, _| {
            unnest(doc);
            Ok(())
        }));
        let mut options = md_to("plain");
        options["separate-filters"] = separate.into();
        convert_with(&options, Some(b"x"), fs)
    };
    assert_eq!(run(None, false).unwrap().text(), "x\n");
    let lua = || Filter::lua(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/upper.lua"));
    let raw = || Filter::raw(|doc, _| Ok(doc.to_vec()));
    for (between, separate) in [(None, true), (Some(lua()), false), (Some(raw()), false)] {
        let e = run(between, separate).unwrap_err();
        assert_eq!(e.kind, "PandocFilterError");
        assert!(e.message.contains("levels deep"), "{e}");
    }
    // the switch is the options', true or false
    let mut bad = md_to("plain");
    bad["separate-filters"] = "yes".into();
    let e = convert_with(&bad, Some(b"x"), vec![]).unwrap_err();
    assert_eq!(e.kind, "PandocOptionError");
    assert!(
        e.message.contains("\"separate-filters\": true or false"),
        "{e}"
    );
    // and not pandoc's: convert takes it too
    let mut options = md_to("plain");
    options["separate-filters"] = true.into();
    assert_eq!(convert(&options, Some(b"x")).unwrap().text(), "x\n");
}

/// What a filter on JSON writes, the next filter of the run gets as it is,
/// unchecked by pandoc: a filter that can't parse it says whose it was.
#[test]
fn a_filter_on_json_hands_on_its_bytes_unchecked() {
    let garble = || Filter::raw(|_, _| Ok(b"not a document".to_vec()));
    let e = convert_with(
        &md_to("plain"),
        Some(b"x"),
        vec![garble(), Filter::func(|_, _| Ok(()))],
    )
    .unwrap_err();
    assert!(
        e.message.contains("filters[1]: ")
            && e.message.contains(
                "(its input is what filters[0] wrote, unchecked: \
                 LIBPANDOC_SEPARATE_FILTERS=1 has pandoc check it)"
            ),
        "{e}"
    );
    // apart, pandoc refuses it as soon as the first returns
    let mut apart = md_to("plain");
    apart["separate-filters"] = true.into();
    let e = convert_with(
        &apart,
        Some(b"x"),
        vec![garble(), Filter::func(|_, _| Ok(()))],
    )
    .unwrap_err();
    assert_eq!(e.kind, "PandocFilterError");
    assert!(!e.message.contains("its input is what"), "{e}");
    // and pandoc checks the run's last answer, as always
    let e = convert_with(
        &md_to("plain"),
        Some(b"x"),
        vec![garble(), Filter::raw(|doc, _| Ok(doc.to_vec()))],
    )
    .unwrap_err();
    assert_eq!(e.kind, "PandocFilterError");
}

/// The order of the filters is kept around pandoc's own: a filter on the
/// document, a Lua filter, then two more is two exchanges, the last two
/// together (they repair what only a run can carry), in the order given.
#[test]
fn a_run_ends_at_a_lua_filter() {
    let out = convert_with(
        &md_to("plain"),
        Some(b"a"),
        vec![
            Filter::func(|doc, _| {
                doc.blocks.push(Block::Para(panir::inlines("b")));
                Ok(())
            }),
            Filter::lua(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/upper.lua")),
            Filter::func(|doc, _| {
                doc.blocks.push(Block::Para(panir::inlines("c")));
                nest(doc, DEEP);
                Ok(())
            }),
            Filter::func(|doc, _| {
                unnest(doc);
                doc.blocks.push(Block::Para(panir::inlines("d")));
                Ok(())
            }),
        ],
    )
    .unwrap();
    assert_eq!(out.text(), "A\n\nB\n\nc\n\nd\n");
}

/// A filter of a run that fails is named by its place in the list given,
/// counted from 0 (pandoc names the callback, which is the run); those
/// after it don't run.
#[test]
fn the_filter_of_a_run_that_failed_is_named() {
    let mut after = false;
    let e = convert_with(
        &md_to("html"),
        Some(b"x"),
        vec![
            Filter::lua(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/upper.lua")),
            Filter::func(|_, _| Ok(())),
            Filter::func(|_, _| Err("no thanks".into())),
            Filter::func(|_, _| {
                after = true;
                Ok(())
            }),
        ],
    )
    .unwrap_err();
    assert_eq!(e.kind, "PandocFilterError");
    assert!(e.message.contains("filters[2]: no thanks"), "{e}");
    assert!(!after);
}

#[test]
#[should_panic(expected = "boom")]
fn a_panic_in_a_run_is_raised_again() {
    let _ = convert_with(
        &md_to("html"),
        Some(b"x"),
        vec![
            Filter::func(|_, _| Ok(())),
            Filter::func(|_, _| panic!("boom")),
            Filter::func(|_, _| unreachable!("after the panic")),
        ],
    );
}

#[test]
fn a_filter_may_call_pandoc() {
    let out = convert_with(
        &md_to("html"),
        Some(b"x"),
        vec![Filter::func(|doc, c| {
            doc.blocks.extend(libpandoc::read_as("*inner*", c)?.blocks);
            Ok(())
        })],
    )
    .unwrap();
    assert_eq!(out.text(), "<p>x</p>\n<p><em>inner</em></p>\n");
}

#[test]
fn many_threads() {
    let handles: Vec<_> = (0..8)
        .map(|i| {
            std::thread::spawn(move || {
                let text = format!("n{i}");
                convert_with(
                    &md_to("plain"),
                    Some(text.as_bytes()),
                    vec![Filter::func(|doc, _| {
                        doc.blocks.push(Block::Para(panir::inlines("ok")));
                        Ok(())
                    })],
                )
                .unwrap()
                .text()
                .into_owned()
            })
        })
        .collect();
    for (i, h) in handles.into_iter().enumerate() {
        assert_eq!(h.join().unwrap(), format!("n{i}\n\nok\n"));
    }
}

#[test]
fn untrusted() {
    // libpandoc's "untrusted": only options that read, write, fetch and run
    // nothing, with pandoc's sandbox on
    let ok = convert(&json!({"to": "html", "untrusted": true}), Some(b"*hi*")).unwrap();
    assert_eq!(ok.text(), "<p><em>hi</em></p>\n");
    let dir = std::env::temp_dir().join(format!("libpandoc-untrusted-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (k, v) in [
        ("filters", json!(["x.lua"])),
        ("output-file", json!(dir.join("o"))),
        ("to", json!("pdf")),
        ("data-dir", json!(dir)),
    ] {
        let mut opts = json!({"to": "html", "untrusted": true});
        opts[k] = v;
        let e = convert(&opts, Some(b"x")).unwrap_err();
        assert!(
            e.message.contains("not allowed for untrusted code"),
            "{k}: {}",
            e.message
        );
    }
    let secret = dir.join("secret.tex");
    std::fs::write(&secret, "SECRET").unwrap();
    // / on Windows too, for LaTeX
    let tex = format!(
        "\\input{{{}}}",
        secret.display().to_string().replace('\\', "/")
    );
    let trusted = convert(
        &json!({"from": "latex", "to": "plain"}),
        Some(tex.as_bytes()),
    )
    .unwrap();
    assert!(trusted.text().contains("SECRET"));
    let opts = json!({"from": "latex", "to": "plain", "untrusted": true});
    let untrusted = convert(&opts, Some(tex.as_bytes())).unwrap();
    assert!(!untrusted.text().contains("SECRET"));
    std::fs::remove_dir_all(&dir).unwrap();
}
