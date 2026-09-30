use libpandoc::{convert, convert_args, convert_with, read, read_many, write, Filter};
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
    let docs = read_many(&["a", "*b*", "c"], &json!({"from": "commonmark"})).unwrap();
    assert_eq!(docs.len(), 3);
    assert_eq!(
        docs[1].blocks,
        vec![Block::Para(vec![Inline::Emph(vec![Inline::Str(
            "b".into()
        )])])]
    );
}

#[test]
fn queries() {
    assert!(libpandoc::pandoc_version().unwrap().starts_with('3'));
    let formats = libpandoc::query("input-formats", json!(null)).unwrap();
    assert!(formats
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f == "commonmark_x"));
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
        ("citeproc", json!(true)),
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
