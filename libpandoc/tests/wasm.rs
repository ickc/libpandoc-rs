//! Wasm filters (the example filters, built for wasm32-wasip1).
#![cfg(feature = "wasm")]

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use libpandoc::convert_with;
use libpandoc::wasm::WasmFilter;
use serde_json::{json, Value};

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

/// Whether libpandoc checks untrusted calls (1.7), which a wasm filter's
/// calls to pandoc need; if not, say so and skip.
fn has_untrusted() -> bool {
    let v = unsafe { libpandoc_sys::pandoc_abi_version() };
    if v < 1007 {
        eprintln!(
            "skipped: libpandoc {}.{} has no \"untrusted\"",
            v / 1000,
            v % 1000
        );
    }
    v >= 1007
}

fn wasm(name: &str) -> WasmFilter {
    WasmFilter::from_file(filters().join(format!("{name}.wasm"))).unwrap()
}

#[test]
fn a_wasm_filter_runs_in_the_conversion() {
    let out = convert_with(
        &json!({"from": "markdown", "to": "plain"}),
        Some(b"hello *world*"),
        vec![wasm("upper").into_filter()],
    )
    .unwrap();
    assert_eq!(out.text(), "HELLO WORLD\n");
}

#[test]
fn it_is_told_the_conversion() {
    let out = convert_with(
        &json!({"from": "commonmark_x", "to": "html5"}),
        Some(b"x"),
        vec![wasm("conversion").into_filter()],
    )
    .unwrap();
    let text = out.text();
    let json_text = text
        .split("<code>")
        .nth(1)
        .unwrap()
        .split("</code>")
        .next()
        .unwrap();
    let told: Value = serde_json::from_str(&json_text.replace("&quot;", "\"")).unwrap();
    assert_eq!(told["format"], "html5");
    assert!(told["input-format"]
        .as_str()
        .unwrap()
        .starts_with("commonmark_x"));
    assert!(told["output-format"].as_str().unwrap().starts_with("html5"));
    assert_eq!(told["reader-options"], true);
    assert_eq!(told["pandoc-version"], libpandoc::pandoc_version().unwrap());
}

#[test]
fn it_sees_only_what_it_is_given() {
    let dir = std::env::temp_dir().join(format!("libpandoc-wasm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("part.txt"), "included").unwrap();
    let input = b"```include\npart.txt\n```\n";
    let opts = json!({"from": "markdown", "to": "plain"});
    // the directory given, as the filter's current directory
    let f = wasm("include").no_files().preopen(&dir, ".", false);
    let out = convert_with(&opts, Some(input), vec![f.into_filter()]).unwrap();
    assert_eq!(out.text().trim(), "included");
    // no directory: the file can't be read, and the filter's failure fails the conversion
    let f = wasm("include").no_files();
    let e = convert_with(&opts, Some(input), vec![f.into_filter()]).unwrap_err();
    assert_eq!(e.kind, "PandocFilterError");
    assert!(e.message.contains("exited with status 3"), "{}", e.message);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn it_cannot_write_what_it_sees_unless_told() {
    let dir = std::env::temp_dir().join(format!("libpandoc-wasm-w-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = b"```write\nout.txt\n```\n";
    let opts = json!({"from": "markdown", "to": "plain"});
    let f = wasm("misbehave").no_files().preopen(&dir, ".", false);
    let e = convert_with(&opts, Some(input), vec![f.into_filter()]).unwrap_err();
    assert!(e.message.contains("exited with status 3"), "{}", e.message);
    assert!(!dir.join("out.txt").exists());
    let f = wasm("misbehave").no_files().preopen(&dir, ".", true);
    convert_with(&opts, Some(input), vec![f.into_filter()]).unwrap();
    assert!(dir.join("out.txt").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_filter_is_stopped_at_its_time_limit() {
    let f = wasm("misbehave").timeout(Some(Duration::from_millis(300)));
    let started = Instant::now();
    let e = convert_with(
        &json!({"from": "markdown", "to": "plain"}),
        Some(b"```spin\n```\n"),
        vec![f.into_filter()],
    )
    .unwrap_err();
    assert_eq!(e.kind, "PandocFilterError");
    assert!(e.message.contains("its time limit"), "{}", e.message);
    assert!(started.elapsed() < Duration::from_secs(10));
    // and one that finishes in time is unaffected
    let f = wasm("upper").timeout(Some(Duration::from_secs(60)));
    let out = convert_with(
        &json!({"from": "markdown", "to": "plain"}),
        Some(b"hi"),
        vec![f.into_filter()],
    );
    assert_eq!(out.unwrap().text(), "HI\n");
}

#[test]
fn a_filter_is_stopped_at_its_memory_limit() {
    let f = wasm("misbehave").max_memory(Some(64 << 20));
    let e = convert_with(
        &json!({"from": "markdown", "to": "plain"}),
        Some(b"```hog\n```\n"),
        vec![f.into_filter()],
    )
    .unwrap_err();
    assert_eq!(e.kind, "PandocFilterError");
    assert!(e.message.contains("out of memory"), "{}", e.message);
    assert!(e.message.contains("67108864 bytes"), "{}", e.message);
}

#[test]
fn limits_are_read_as_pandoc_writes_them() {
    use libpandoc::wasm::{parse_memory, parse_timeout};
    assert_eq!(parse_timeout("2"), Ok(Some(Duration::from_secs(2))));
    assert_eq!(parse_timeout("0.5"), Ok(Some(Duration::from_millis(500))));
    assert_eq!(parse_timeout(""), Ok(None));
    assert_eq!(parse_timeout("0"), Ok(None));
    assert!(parse_timeout("-1").is_err());
    assert!(parse_timeout("2s").is_err());
    assert_eq!(parse_memory("512M"), Ok(Some(512 << 20)));
    assert_eq!(parse_memory("1g"), Ok(Some(1 << 30)));
    assert_eq!(parse_memory("4096"), Ok(Some(4096)));
    assert_eq!(parse_memory("64k"), Ok(Some(64 << 10)));
    assert_eq!(parse_memory(""), Ok(None));
    assert!(parse_memory("12x").is_err());
    assert!(parse_memory("M").is_err());
}

#[test]
fn not_a_wasm_file() {
    assert!(WasmFilter::from_bytes("x", b"not wasm").is_err());
}

#[test]
fn a_filter_calls_pandoc_as_the_document_is_read() {
    if !has_untrusted() {
        return;
    }
    let input = b"```parse\n*a*\n```\n\n```parse\n# b\n```\n";
    let out = convert_with(
        &json!({"from": "markdown", "to": "html"}),
        Some(input),
        vec![wasm("parse").into_filter()],
    )
    .unwrap();
    assert_eq!(out.text(), "<p><em>a</em></p>\n<h1 id=\"b\">b</h1>\n");
}

/// The `calls` filter's answers to requests (see filters/src/bin/calls.rs).
fn calls(requests: &[Value]) -> Vec<Value> {
    let input: String = requests
        .iter()
        .map(|r| format!("```call\n{r}\n```\n\n"))
        .collect();
    let out = convert_with(
        &json!({"from": "markdown", "to": "json"}),
        Some(input.as_bytes()),
        vec![wasm("calls").into_filter()],
    )
    .unwrap();
    let doc: Value = serde_json::from_slice(&out.bytes).unwrap();
    doc["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| serde_json::from_str(b["c"][1].as_str().unwrap()).unwrap())
        .collect()
}

#[test]
fn a_filter_calls_convert_read_many_and_query() {
    if !has_untrusted() {
        return;
    }
    let a = calls(&[
        json!({"convert": [{"from": "markdown", "to": "html"}, "*x*"]}),
        json!({"read_many": [["a", "*b*"], {"from": "markdown"}]}),
        json!({"query": ["version", null]}),
    ]);
    assert_eq!(a[0], json!({"ok": "<p><em>x</em></p>\n"}));
    assert_eq!(a[1]["ok"].as_array().unwrap().len(), 2);
    assert_eq!(a[1]["ok"][1]["blocks"][0]["c"][0]["t"], "Emph");
    assert_eq!(a[2], json!({"ok": libpandoc::pandoc_version().unwrap()}));
}

#[test]
fn a_filters_calls_get_no_files_programs_or_lua() {
    if !has_untrusted() {
        return;
    }
    let a = calls(&[
        json!({"convert": [{"from": "markdown", "to": "html", "filters": ["/bin/sh"]}, "x"]}),
        json!({"convert": [{"from": "markdown", "to": "html", "output-file": "out.html"}, "x"]}),
        json!({"convert": [{"from": "markdown", "to": "writer.lua"}, "x"]}),
        json!({"convert": [{"from": "markdown", "to": "pdf"}, "x"]}),
        json!({"convert": [{"from": "markdown", "to": "html"}, null]}),
        json!({"read_many": [["x"], {"from": "markdown", "data-dir": "/"}]}),
        json!({"query": ["parse-args", {"args": ["-d", "x.yaml"]}]}),
    ]);
    for x in &a {
        assert_eq!(x["error"][0], "PandocOptionError", "{x}");
    }
    assert!(a[0]["error"][1]
        .as_str()
        .unwrap()
        .contains("not allowed for untrusted code: filters"));
}

#[test]
fn a_filters_calls_are_sandboxed() {
    if !has_untrusted() {
        return;
    }
    // LaTeX's \input reads a file: not in pandoc's sandbox
    let dir = std::env::temp_dir().join(format!("libpandoc-sandbox-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let secret = dir.join("secret.tex");
    std::fs::write(&secret, "SECRET").unwrap();
    let tex = format!("\\input{{{}}}", secret.display());
    let a = calls(&[
        json!({"read_many": [[tex], {"from": "latex"}]}),
        json!({"convert": [{"from": "latex", "to": "plain"}, tex]}),
    ]);
    assert!(!a[0].to_string().contains("SECRET"), "{}", a[0]);
    assert!(!a[1].to_string().contains("SECRET"), "{}", a[1]);
    // natively, not sandboxed, it is read
    let doc = libpandoc::read(&tex, "latex").unwrap();
    assert!(panir::to_string(&doc).contains("SECRET"));
    std::fs::remove_dir_all(&dir).unwrap();
}
