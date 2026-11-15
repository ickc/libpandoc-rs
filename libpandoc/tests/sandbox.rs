//! The directory sandbox ("sandbox": {"read": [...], "write": [...]}): the
//! attack suite of the project's sandbox study, as data, run in
//! libpandoc.wasm with one directory mounted, and the rules of the mounts.
//!
//! The lab, made once in Cargo's temporary directory (`target/tmp`): `work`
//! (mounted, and the working directory of this test program), `secret`
//! beside it (canaries, harmless files whose text says CANARY), `outside`
//! (files the options name, mounted in some tests) and `out` (written to).
#![cfg(feature = "wasm")]

use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use libpandoc::{Error, Output, convert};
use serde_json::{Value, json};

struct Lab {
    root: PathBuf,
    work: PathBuf,
    secret: PathBuf,
    outside: PathBuf,
    out: PathBuf,
    /// a local web server's address, and how many connections it got
    url: String,
}

static CONNECTIONS: AtomicUsize = AtomicUsize::new(0);

fn lab() -> &'static Lab {
    static LAB: OnceLock<Lab> = OnceLock::new();
    LAB.get_or_init(|| {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("sandbox-lab-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let names = ["work", "secret", "outside", "out"];
        for d in names {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        // the real path (macOS's /var is /private/var), but not Windows'
        // `\\?\` form, which pandoc can't take as the working directory
        let root = match cfg!(windows) {
            true => std::path::absolute(&root),
            false => root.canonicalize(),
        }
        .unwrap();
        let [work, secret, outside, out] = names.map(|d| root.join(d));
        let svg = |text: &str| {
            format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><text>{text}</text></svg>"#)
        };
        let files: [(&str, String); 16] = [
            ("secret.txt", "CANARY-TXT-OUTSIDE\n".into()),
            ("secret.tex", "CANARY-TEX-OUTSIDE\n".into()),
            ("secret2.sty", "\\newcommand{\\leak}{CANARY-STY-OUTSIDE}\n".into()),
            ("secret.svg", svg("CANARY-SVG-OUTSIDE")),
            ("probe.png", "not really a png".into()),
            ("secret.csv", "a,b\nCANARY-CSV-OUTSIDE,x\n".into()),
            ("secret.json", r#"{"a": "CANARY-JSON-OUTSIDE"}"#.into()),
            ("secret.typ", "CANARY-TYP-OUTSIDE\n".into()),
            ("secret.org", "CANARY-ORG-OUTSIDE\n".into()),
            ("secret.man", ".PP\nCANARY-MAN-OUTSIDE\n".into()),
            ("secret.t2t", "CANARY-T2T-OUTSIDE\n".into()),
            ("secret.js", "/* CANARY-JS-OUTSIDE */\n".into()),
            ("secret.css", "/* CANARY-CSS-OUTSIDE */\n".into()),
            ("secret.bib", "@book{canary, title={CANARY-BIB-OUTSIDE}, author={A}, year={2000}}\n".into()),
            ("secret.csl", "CANARY-CSL-OUTSIDE\n".into()),
            ("x/typst.toml", "CANARY-TOML-OUTSIDE\n".into()),
        ];
        for (name, text) in files {
            let p = secret.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        std::fs::write(work.join("inside.svg"), svg("INSIDE-OK")).unwrap();
        std::fs::write(work.join("inside.txt"), "INSIDE-OK\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            for ext in ["txt", "svg", "tex"] {
                symlink(format!("../secret/secret.{ext}"), work.join(format!("link-out.{ext}"))).unwrap();
            }
            symlink("../secret", work.join("link-dir")).unwrap();
            for f in ["template.html", "style.css", "refs.bib", "input.md"] {
                symlink(format!("../outside/{f}"), work.join(format!("link-{f}"))).unwrap();
            }
        }
        // what the options name: a template, a stylesheet, a bibliography,
        // outside and (`-in`) in `work`
        for dir in [&outside, &work] {
            let tag = if dir == &work { "-in" } else { "" };
            let files = [
                ("template", "html", "TEMPLATE-OK $for(css)$<link rel=\"stylesheet\" href=\"$css$\">$endfor$ $body$\n"),
                ("style", "css", "p::after { content: \"CSS-OK\" }\n"),
                ("refs", "bib", "@book{ok, title={BIB-OK}, author={A}, year={2000}}\n"),
            ];
            for (stem, ext, text) in files {
                std::fs::write(dir.join(format!("{stem}{tag}.{ext}")), text).unwrap();
            }
        }
        std::fs::write(outside.join("input.md"), "INPUT-OK").unwrap();
        // the working directory: relative paths resolve from `work`, as
        // pandoc run there would
        std::env::set_current_dir(&work).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for _ in listener.incoming() {
                CONNECTIONS.fetch_add(1, Ordering::SeqCst);
            }
        });
        Lab { root, work, secret, outside, out, url }
    })
}

/// The options, with the sandbox: `work` only.
fn sandboxed(options: Value) -> Value {
    mounted(options, &[&lab().work], &[])
}

/// The options, with the sandbox: these directories.
fn mounted(mut options: Value, read: &[&Path], write: &[&Path]) -> Value {
    options["sandbox"] = json!({"read": read, "write": write});
    options
}

/// What a conversion gave: its output, its messages (the error, or the
/// log), and everything (both, with zip entries inflated, the files
/// extracted under `media`, and runs of base64 decoded), where a canary
/// would show.
struct Seen {
    output: String,
    messages: String,
    everything: String,
}

fn seen(r: &Result<Output, Error>, media: Option<&Path>) -> Seen {
    let (output, messages) = match r {
        Ok(o) => (
            String::from_utf8_lossy(&o.bytes).into_owned(),
            Value::from(o.log.clone()).to_string(),
        ),
        Err(e) => (String::new(), format!("{}: {}", e.kind, e.message)),
    };
    let mut all = format!("{output}\n{messages}\n");
    if let Ok(o) = r {
        for entry in unzip(&o.bytes) {
            all += &String::from_utf8_lossy(&entry);
        }
    }
    for f in media.map(walk).unwrap_or_default() {
        all += &String::from_utf8_lossy(&std::fs::read(f).unwrap());
    }
    let decoded = base64_runs(&all);
    Seen {
        output,
        messages,
        everything: all + &decoded,
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

/// The entries of a zip file (docx, odt, epub), inflated; none if it isn't
/// one.
fn unzip(b: &[u8]) -> Vec<Vec<u8>> {
    let u16_at = |i: usize| {
        b.get(i..i + 2)
            .map(|x| u16::from_le_bytes([x[0], x[1]]) as usize)
    };
    let u32_at = |i: usize| {
        b.get(i..i + 4)
            .map(|x| u32::from_le_bytes(x.try_into().unwrap()) as usize)
    };
    let Some(end) = (0..b.len().saturating_sub(21))
        .rev()
        .find(|&i| u32_at(i) == Some(0x0605_4b50))
    else {
        return Vec::new();
    };
    let (count, mut at) = (u16_at(end + 10).unwrap(), u32_at(end + 16).unwrap());
    let mut out = Vec::new();
    for _ in 0..count {
        if u32_at(at) != Some(0x0201_4b50) {
            break;
        }
        let (method, size) = (u16_at(at + 10).unwrap(), u32_at(at + 20).unwrap());
        let (name, extra, comment) = (
            u16_at(at + 28).unwrap(),
            u16_at(at + 30).unwrap(),
            u16_at(at + 32).unwrap(),
        );
        let local = u32_at(at + 42).unwrap();
        let data = local + 30 + u16_at(local + 26).unwrap() + u16_at(local + 28).unwrap();
        let raw = &b[data..data + size];
        let mut entry = Vec::new();
        if method == 8 {
            flate2::read::DeflateDecoder::new(raw)
                .read_to_end(&mut entry)
                .unwrap();
        } else {
            entry = raw.to_vec();
        }
        out.push(entry);
        at += 46 + name + extra + comment;
    }
    out
}

/// Runs of base64 (as in `data:` URIs), decoded.
fn base64_runs(s: &str) -> String {
    let value = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let mut out = Vec::new();
    for run in s.as_bytes().split(|&c| value(c).is_none()) {
        if run.len() < 24 {
            continue;
        }
        for chunk in run.chunks(4) {
            let v: Vec<u32> = chunk.iter().map(|&c| value(c).unwrap() as u32).collect();
            let n = v.iter().fold(0u32, |a, &x| a << 6 | x) << (6 * (4 - v.len()));
            out.extend(&n.to_be_bytes()[1..v.len()]);
        }
        out.push(b'\n');
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// An attack: a document (with `{P}` where the path it tries goes, `{S}`
/// the secret directory, `{URL}` the local server), its extension, the
/// path, and the conversion's options.
struct Case {
    name: &'static str,
    ext: &'static str,
    doc: &'static str,
    path: &'static str,
    options: Value,
}

#[rustfmt::skip]
fn cases() -> Vec<Case> {
    let c = |name, ext, doc, path, options: Value| Case { name, ext, doc, path, options };
    let latex = || json!({"from": "latex", "to": "plain"});
    let embed = |from: &str| json!({"from": from, "to": "html", "embed-resources": true});
    let md = |to: &str| json!({"from": "markdown", "to": to});
    let mut v = vec![
        c("latex-input-rel", "tex", r"\input{{P}}", "../secret/secret", latex()),
        c("latex-input-abs", "tex", r"\input{{P}}", "{S}/secret.tex", latex()),
        c("latex-include", "tex", r"\include{{P}}", "../secret/secret", latex()),
        c("latex-subfile", "tex", r"\subfile{{P}}", "../secret/secret.tex", latex()),
        c("latex-lstinput", "tex", r"\lstinputlisting{{P}}", "../secret/secret.txt", latex()),
        c("latex-lst-abs", "tex", r"\lstinputlisting{{P}}", "{S}/secret.txt", latex()),
        c("latex-minted", "tex", r"\inputminted{text}{{P}}", "../secret/secret.txt", latex()),
        c("latex-usepackage", "tex", r"\usepackage{{P}}\leak", "../secret/secret2", latex()),
        c("latex-graphics-probe", "tex", r"\includegraphics{{P}}", "../secret/probe", json!({"from": "latex", "to": "native"})),
        c("latex-graphics-embed", "tex", r"\includegraphics{{P}}", "../secret/secret.svg", embed("latex")),
        c("latex-bibliography", "tex", r"\bibliography{{P}}\nocite{*}", "../secret/secret", json!({"from": "latex", "to": "plain", "citeproc": true})),
        c("latex-graphicspath", "tex", r"\graphicspath{{{P}}}\includegraphics{secret.svg}", "../secret/", embed("latex")),
        c("rst-include", "rst", ".. include:: {P}", "../secret/secret.txt", json!({"from": "rst", "to": "plain"})),
        c("rst-include-abs", "rst", ".. include:: {P}", "{S}/secret.txt", json!({"from": "rst", "to": "plain"})),
        c("rst-include-literal", "rst", ".. include:: {P}\n   :literal:", "../secret/secret.txt", json!({"from": "rst", "to": "plain"})),
        c("rst-raw-file", "rst", ".. raw:: html\n   :file: {P}", "../secret/secret.txt", json!({"from": "rst", "to": "html"})),
        c("rst-csv-file", "rst", ".. csv-table:: T\n   :file: {P}", "../secret/secret.csv", json!({"from": "rst", "to": "plain"})),
        c("rst-csv-url", "rst", ".. csv-table:: T\n   :url: {P}", "{URL}/x.csv", json!({"from": "rst", "to": "plain"})),
        c("rst-literalinclude", "rst", ".. literalinclude:: {P}", "../secret/secret.txt", json!({"from": "rst", "to": "plain"})),
        c("rst-image-embed", "rst", ".. image:: {P}", "../secret/secret.svg", embed("rst")),
        c("org-include", "org", "#+INCLUDE: \"{P}\" src text", "../secret/secret.txt", json!({"from": "org", "to": "plain"})),
        c("org-include-plain", "org", "#+INCLUDE: \"{P}\"", "../secret/secret.org", json!({"from": "org", "to": "plain"})),
        c("org-setupfile", "org", "#+SETUPFILE: {P}\n\ntext", "../secret/secret.org", json!({"from": "org", "to": "plain"})),
        c("adoc-include", "adoc", "include::{P}[]", "../secret/secret.txt", json!({"from": "asciidoc", "to": "plain"})),
        c("adoc-include-abs", "adoc", "include::{P}[]", "{S}/secret.txt", json!({"from": "asciidoc", "to": "plain"})),
        c("man-so", "man", ".TH T 1\n.so {P}\n", "../secret/secret.man", json!({"from": "man", "to": "plain"})),
        c("man-so-abs", "man", ".TH T 1\n.so {P}\n", "{S}/secret.man", json!({"from": "man", "to": "plain"})),
        c("t2t-include", "t2t", "\n\n\n%!include: {P}\n", "../secret/secret.t2t", json!({"from": "t2t", "to": "plain"})),
        c("typst-include", "typ", "#include \"{P}\"", "../secret/secret.typ", json!({"from": "typst", "to": "plain"})),
        c("typst-read", "typ", "#read(\"{P}\")", "../secret/secret.txt", json!({"from": "typst", "to": "plain"})),
        c("typst-read-abs", "typ", "#read(\"{P}\")", "{S}/secret.txt", json!({"from": "typst", "to": "plain"})),
        c("typst-json", "typ", "#json(\"{P}\").a", "../secret/secret.json", json!({"from": "typst", "to": "plain"})),
        c("typst-csv", "typ", "#csv(\"{P}\").at(1).at(0)", "../secret/secret.csv", json!({"from": "typst", "to": "plain"})),
        c("typst-image", "typ", "#image(\"{P}\")", "../secret/secret.svg", embed("typst")),
        c("typst-import-pkg", "typ", "#import \"@preview/../../../../../../../../../../../..{P}:1\": *\n#leak", "{S}/x", json!({"from": "typst", "to": "plain"})),
        c("djot-image", "dj", "![x]({P})", "../secret/secret.svg", embed("djot")),
        c("mediawiki-transclude", "wiki", "{{:{P}}}", "../secret/secret.txt", json!({"from": "mediawiki", "to": "plain"})),
        c("dokuwiki-include", "dw", "{{page>{P}}}", "../secret/secret.txt", json!({"from": "dokuwiki", "to": "plain"})),
        c("creole-image", "creole", "{{{P}|x}}", "../secret/secret.svg", embed("creole")),
        c("md-image-embed", "md", "![x]({P})", "../secret/secret.svg", embed("markdown")),
        c("md-image-abs", "md", "![x]({P})", "{S}/secret.svg", embed("markdown")),
        c("md-image-abs-dotdot", "md", "![x]({P})", "{W}/../secret/secret.svg", embed("markdown")),
        c("md-image-file-url", "md", "![x](file://{P})", "{S}/secret.svg", embed("markdown")),
        c("md-image-pct", "md", "![x]({P})", "..%2Fsecret%2Fsecret.svg", embed("markdown")),
        c("md-image-http", "md", "![x]({P})", "{URL}/x.svg", embed("markdown")),
        c("md-image-docx", "md", "![x]({P})", "../secret/secret.svg", md("docx")),
        c("md-image-odt", "md", "![x]({P})", "../secret/secret.svg", md("odt")),
        c("md-image-epub", "md", "![x]({P})", "../secret/secret.svg", md("epub")),
        c("md-image-rtf", "md", "![x]({P})", "../secret/secret.svg", md("rtf")),
        c("md-image-ipynb", "md", "![x]({P})", "../secret/secret.svg", md("ipynb")),
        c("md-image-fb2", "md", "![x]({P})", "../secret/secret.svg", md("fb2")),
        c("md-image-icml", "md", "![x]({P})", "../secret/secret.svg", md("icml")),
        c("md-extract-media", "md", "![x]({P})", "../secret/secret.svg", json!({"from": "markdown", "to": "html", "extract-media": "media-md-extract-media"})),
        c("md-raw-html", "md", "<script src=\"{P}.js\"></script><link rel=\"stylesheet\" href=\"{P}.css\"><img src=\"{P}.svg\">", "../secret/secret", json!({"from": "markdown", "to": "html", "embed-resources": true, "standalone": true, "metadata": {"title": "x"}})),
        c("html-embed", "html", "<p><img src=\"{P}.svg\"></p><script src=\"{P}.js\"></script><link rel=\"stylesheet\" href=\"{P}.css\">", "../secret/secret", json!({"from": "html", "to": "html", "embed-resources": true, "standalone": true, "metadata": {"title": "x"}})),
        c("html-iframe-http", "html", "<iframe src=\"{P}\"></iframe>", "{URL}/x.html", json!({"from": "html", "to": "plain"})),
        c("html-iframe-file", "html", "<iframe src=\"file://{P}\"></iframe>", "{S}/secret.txt", json!({"from": "html", "to": "plain"})),
        c("html-iframe-rel", "html", "<iframe src=\"{P}\"></iframe>", "../secret/secret.txt", json!({"from": "html", "to": "plain"})),
        c("docbook-xxe", "xml", "<?xml version=\"1.0\"?><!DOCTYPE article [<!ENTITY x SYSTEM \"file://{P}\">]><article><para>&x;</para></article>", "{S}/secret.txt", json!({"from": "docbook", "to": "plain"})),
        c("docbook-xinclude", "xml", "<article xmlns:xi=\"http://www.w3.org/2001/XInclude\"><para><xi:include href=\"{P}\" parse=\"text\"/></para></article>", "../secret/secret.txt", json!({"from": "docbook", "to": "plain"})),
        c("jats-xxe", "xml", "<?xml version=\"1.0\"?><!DOCTYPE article [<!ENTITY x SYSTEM \"file://{P}\">]><article><body><p>&x;</p></body></article>", "{S}/secret.txt", json!({"from": "jats", "to": "plain"})),
        c("opml-ext", "opml", "<?xml version=\"1.0\"?><opml version=\"2.0\"><head/><body><outline text=\"a\" url=\"{P}\"/></body></opml>", "../secret/secret.txt", json!({"from": "opml", "to": "plain"})),
        c("meta-bibliography", "md", "---\nbibliography: {P}\nnocite: \"@*\"\n---\n\nText.\n", "../secret/secret.bib", json!({"from": "markdown", "to": "plain", "citeproc": true})),
        c("meta-bibliography-abs", "md", "---\nbibliography: {P}\nnocite: \"@*\"\n---\n\nText.\n", "{S}/secret.bib", json!({"from": "markdown", "to": "plain", "citeproc": true})),
        c("meta-bibliography-http", "md", "---\nbibliography: {P}\nnocite: \"@*\"\n---\n\nText.\n", "{URL}/x.bib", json!({"from": "markdown", "to": "plain", "citeproc": true})),
        c("meta-csl", "md", "---\ncsl: {P}\n---\n\nText [@x].\n", "../secret/secret.csl", json!({"from": "markdown", "to": "plain", "citeproc": true})),
        c("meta-abbrev", "md", "---\ncitation-abbreviations: {P}\nreferences: [{id: x, title: T, type: book}]\n---\n\nText [@x].\n", "../secret/secret.txt", json!({"from": "markdown", "to": "plain", "citeproc": true})),
        c("meta-lang", "md", "---\nlang: {P}\n---\n\nText.\n", "../../secret/secret", json!({"from": "markdown", "to": "latex", "standalone": true})),
        c("meta-header-includes", "md", "---\nheader-includes: {P}\ntitle: x\n---\n\nText.\n", "../secret/secret.txt", json!({"from": "markdown", "to": "html", "standalone": true})),
        c("meta-bib-in-latex", "tex", r"\addbibresource{{P}}\nocite{*}", "../secret/secret.bib", json!({"from": "latex", "to": "plain", "citeproc": true})),
    ];
    if cfg!(unix) {
        v.extend([
            c("latex-input-symlink", "tex", r"\input{{P}}", "link-out", latex()),
            c("latex-input-symlinkdir", "tex", r"\input{{P}}", "link-dir/secret", latex()),
            c("typst-read-symlink", "typ", "#read(\"{P}\")", "link-out.txt", json!({"from": "typst", "to": "plain"})),
            c("md-image-symlink", "md", "![x]({P})", "link-out.svg", embed("markdown")),
            c("md-image-symlink-abs", "md", "![x]({P})", "{W}/link-out.svg", embed("markdown")),
        ]);
    }
    v
}

/// An absolute path as the module names it: as it is, with `/`; on
/// Windows `C:\x` is `/c/x` (a path with a drive letter means nothing in
/// the module: documented).
fn named(p: &Path) -> String {
    let s = p.display().to_string().replace('\\', "/");
    let s = s.strip_prefix("//?/").unwrap_or(&s);
    match s.as_bytes() {
        [d, b':', ..] if cfg!(windows) => format!("/{}{}", d.to_ascii_lowercase() as char, &s[2..]),
        _ => s.to_owned(),
    }
}

/// The path, the lab's in it.
fn fill(s: &str) -> String {
    let lab = lab();
    s.replace("{S}", &named(&lab.secret))
        .replace("{W}", &named(&lab.work))
        .replace("{URL}", &lab.url)
}

/// A path that doesn't exist, inside the directory, as long as the
/// attack's and with its extension (its other characters `q`): what a
/// refusal must look like.
fn missing(attack: &str) -> String {
    let lab = lab();
    let w = format!("{}/", named(&lab.work));
    let (prefix, rest) = if let Some(r) = attack.strip_prefix(&lab.url) {
        (lab.url.clone(), r)
    } else if attack.starts_with('/') || attack.contains(":/") {
        (w.clone(), &attack[w.len().min(attack.len())..])
    } else {
        (String::new(), attack)
    };
    let ext = rest
        .rsplit('/')
        .next()
        .and_then(|n| n.rfind('.').map(|i| &n[i..]))
        .unwrap_or("");
    let stem = &rest[..rest.len() - ext.len()];
    let q: String = stem
        .chars()
        .map(|c| if c == '/' { c } else { 'q' })
        .collect();
    format!("{prefix}{q}{ext}")
}

/// Convert the case with the document naming `p`; what came out, with `p`
/// written `{P}`.
fn attempt(case: &Case, p: &str, tag: &str) -> Seen {
    let doc = case.doc.replace("{P}", p);
    let stem = format!("doc-{}-{tag}", case.name);
    let file = format!("{stem}.{}", case.ext);
    std::fs::write(lab().work.join(&file), &doc).unwrap();
    let mut options = sandboxed(case.options.clone());
    options["input-files"] = json!([file]);
    let media = case.options.get("extract-media").map(|m| {
        let m = format!("{}-{tag}", m.as_str().unwrap());
        options["extract-media"] = m.clone().into();
        lab().work.join(m)
    });
    let r = convert(&options, None);
    let s = seen(&r, media.as_deref());
    let general = |s: String| {
        s.replace(p, "{P}")
            .replace(&p.replace("%2F", "/"), "{P}")
            .replace(&stem, "{FILE}")
    };
    Seen {
        output: general(s.output),
        messages: general(s.messages),
        everything: s.everything,
    }
}

const NOT_PERMITTED: &str = "permission denied (Operation not permitted)";
const DOES_NOT_EXIST: &str = "does not exist (No such file or directory)";

/// The study's 80 cases: nothing of `secret` comes out, no connection is
/// made, and a refusal reads as a missing file does.
#[test]
fn the_attacks_leak_nothing() {
    let lab = lab();
    let cases = cases();
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get().min(8));
    let results: Vec<(String, Vec<String>)> = std::thread::scope(|s| {
        let chunks: Vec<_> = cases.chunks(cases.len().div_ceil(threads)).collect();
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                s.spawn(move || {
                    chunk
                        .iter()
                        .map(|case| {
                            let mut problems = Vec::new();
                            let path = fill(case.path);
                            let attack = attempt(case, &path, "attack");
                            if attack.everything.contains("CANARY") {
                                problems.push(format!("LEAK: {}", attack.everything));
                            }
                            let control = attempt(case, &missing(&path), "missing");
                            // an EPUB or docx has a fresh identifier or date:
                            // their messages only
                            let output = ["epub", "docx"]
                                .contains(&case.options["to"].as_str().unwrap_or(""))
                                || attack.output == control.output;
                            // `..` or a symbolic link leading out of the
                            // directory is "not permitted" in WASI, a file
                            // outside every directory "does not exist":
                            // documented (README, "Security"), not hidden
                            let errno = attack.messages.replace(NOT_PERMITTED, DOES_NOT_EXIST);
                            if output && attack.messages == control.messages {
                            } else if output && errno == control.messages {
                                problems.push("ERRNO".into());
                            } else {
                                problems.push(format!(
                                    "DISTINGUISHABLE:\n  refused: {}\n{}\n  missing: {}\n{}",
                                    attack.messages,
                                    attack.output,
                                    control.messages,
                                    control.output
                                ));
                            }
                            (case.name.to_owned(), problems)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });
    let mut report = String::new();
    let mut errno = Vec::new();
    for (name, problems) in &results {
        if problems.iter().any(|p| p.starts_with("ERRNO")) {
            errno.push(name.as_str());
        }
        let problems: Vec<_> = problems
            .iter()
            .filter(|p| !p.starts_with("ERRNO"))
            .collect();
        if !problems.is_empty() {
            report += &format!(
                "{name}:\n  {}\n",
                problems
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join("\n  ")
            );
        }
    }
    eprintln!("refused as \"not permitted\", not \"does not exist\": {errno:?}");
    eprintln!("{} cases; root {}", results.len(), lab.root.display());
    if let Ok(f) = std::env::var("SANDBOX_REPORT") {
        std::fs::write(f, &report).unwrap();
    }
    assert_eq!(
        CONNECTIONS.load(Ordering::SeqCst),
        0,
        "a connection was made"
    );
    assert!(report.is_empty(), "{report}");
}

#[test]
fn an_image_inside_is_embedded() {
    let r = convert(
        &sandboxed(json!({"from": "markdown", "to": "html", "embed-resources": true})),
        Some(b"![x](inside.svg)"),
    );
    assert!(seen(&r, None).everything.contains("INSIDE-OK"), "{r:?}");
}

/// The canaries are found where they are: without the sandbox, the same
/// documents leak (so that the suite's "nothing leaked" means something).
#[test]
fn without_the_sandbox_they_leak() {
    lab();
    for name in [
        "latex-input-rel",
        "md-image-embed",
        "md-image-docx",
        "rst-include",
    ] {
        let case = cases().into_iter().find(|c| c.name == name).unwrap();
        let file = format!("doc-{name}-native.{}", case.ext);
        std::fs::write(
            lab().work.join(&file),
            case.doc.replace("{P}", &fill(case.path)),
        )
        .unwrap();
        let mut options = case.options.clone();
        options["input-files"] = json!([file]);
        let r = convert(&options, None);
        assert!(
            seen(&r, None).everything.contains("CANARY"),
            "{name}: {r:?}"
        );
    }
}

/// A message as a missing file's would read: "not permitted" (WASI's
/// answer for a path leaving a mount) as "does not exist".
fn as_missing(m: &str) -> String {
    m.replace(NOT_PERMITTED, DOES_NOT_EXIST)
}

/// The files the options name are read only inside the mounts: a template,
/// a stylesheet, a bibliography, an input file. Outside, each reads as a
/// missing file named the same way does, or (for an input file reached by
/// `..` or a symbolic link) "not permitted": documented.
#[test]
fn what_the_options_name_must_be_mounted() {
    let lab = lab();
    let options = |template: &str, css: &str, bib: &str| {
        json!({
            "from": "markdown", "to": "html", "standalone": true, "embed-resources": true,
            "citeproc": true, "template": template, "css": [css], "bibliography": [bib],
        })
    };
    let doc: &[u8] = b"---\nnocite: \"@*\"\n---\n\nText.";
    let ok = |r: &Result<Output, Error>| {
        let all = seen(r, None).everything;
        ["TEMPLATE-OK", "CSS-OK", "BIB-OK"]
            .iter()
            .all(|x| all.contains(x))
    };
    let abs = |f: &str| named(&lab.outside.join(f));
    // inside: relative to the working directory, and absolute
    let r = convert(
        &sandboxed(options("template-in.html", "style-in.css", "refs-in.bib")),
        Some(doc),
    );
    assert!(ok(&r), "{r:?}");
    let both = |o| mounted(o, &[&lab.work, &lab.outside], &[]);
    let r = convert(
        &both(options(
            &abs("template.html"),
            &abs("style.css"),
            &abs("refs.bib"),
        )),
        Some(doc),
    );
    assert!(ok(&r), "{r:?}");
    // outside, each way of naming it, against a missing file inside (`qqq`
    // for the directory); `..` from one mount into another fails too
    let mut table = Vec::new();
    for (what, file) in [
        ("template", "template.html"),
        ("css", "style.css"),
        ("bib", "refs.bib"),
        ("input", "input.md"),
    ] {
        let o = |p: &str| match what {
            "template" => {
                json!({"from": "markdown", "to": "html", "standalone": true, "template": p})
            }
            "css" => {
                json!({"from": "markdown", "to": "html", "standalone": true, "embed-resources": true, "css": [p], "metadata": {"title": "t"}})
            }
            "bib" => {
                json!({"from": "markdown", "to": "plain", "citeproc": true, "bibliography": [p]})
            }
            _ => json!({"from": "markdown", "to": "plain", "input-files": [p]}),
        };
        let mut ways = vec![
            (
                "relative",
                format!("../outside/{file}"),
                format!("qqqqqqqqqq/{file}"),
            ),
            (
                "absolute",
                abs(file),
                named(&lab.work.join("qqqqqqq").join(file)),
            ),
            (
                "dotdot-between",
                format!("../outside/{file}"),
                format!("qqqqqqqqqq/{file}"),
            ),
        ];
        if cfg!(unix) {
            ways.push(("symlink", format!("link-{file}"), format!("qqqqq{file}")));
        }
        for (way, path, missing) in ways {
            let mounts = |o| {
                if way == "dotdot-between" {
                    both(o)
                } else {
                    sandboxed(o)
                }
            };
            let input = (what != "input").then_some(doc);
            let refused = convert(&mounts(o(&path)), input);
            let s = seen(&refused, None);
            assert!(
                !s.everything.contains("-OK"),
                "{what} {way}: read: {refused:?}"
            );
            let a = s.messages.replace(&path, "{P}");
            let b = seen(&convert(&mounts(o(&missing)), input), None)
                .messages
                .replace(&missing, "{P}");
            let how = if a == b {
                "missing"
            } else if as_missing(&a) == b {
                "not permitted"
            } else {
                "other"
            };
            table.push(format!("{what} {way}: {how}"));
        }
    }
    let expected = [
        "template relative: missing",
        "template absolute: missing",
        "template dotdot-between: missing",
        "template symlink: missing",
        "css relative: missing",
        "css absolute: missing",
        "css dotdot-between: missing",
        "css symlink: missing",
        "bib relative: missing",
        "bib absolute: missing",
        "bib dotdot-between: missing",
        "bib symlink: missing",
        "input relative: not permitted",
        "input absolute: missing",
        "input dotdot-between: not permitted",
        "input symlink: not permitted",
    ];
    let expected: Vec<_> = expected
        .into_iter()
        .filter(|e| cfg!(unix) || !e.contains("symlink"))
        .collect();
    assert_eq!(table, expected);
}

/// pandoc writes only inside the write list: `output-file`,
/// `extract-media` and `log-file` there work; in a read-only mount or
/// outside every mount they fail, and nothing is written.
#[test]
fn outputs_go_only_into_writable_mounts() {
    let lab = lab();
    let o = |out: &Path, media: &Path| {
        json!({
            "from": "markdown", "to": "html", "standalone": true, "metadata": {"title": "t"},
            "output-file": named(&out.join("doc.html")),
            "extract-media": named(&media.join("media")),
            "log-file": named(&out.join("log.json")),
        })
    };
    let doc: &[u8] = b"![x](inside.svg)";
    let dir = lab.out.join("writable");
    std::fs::create_dir_all(&dir).unwrap();
    let r = convert(&mounted(o(&dir, &dir), &[&lab.work], &[&dir]), Some(doc)).unwrap();
    assert!(r.bytes.is_empty());
    let html = std::fs::read_to_string(dir.join("doc.html")).unwrap();
    assert!(html.contains("media/"), "{html}");
    assert!(
        std::fs::read_to_string(dir.join("log.json"))
            .unwrap()
            .starts_with('[')
    );
    let media = walk(&dir.join("media"));
    assert_eq!(media.len(), 1, "{media:?}");
    assert!(
        std::fs::read_to_string(&media[0])
            .unwrap()
            .contains("INSIDE-OK")
    );
    // relative to the working directory, when it is writable
    let r = convert(
        &mounted(
            json!({"to": "html", "output-file": "doc-relative.html"}),
            &[],
            &[&lab.work],
        ),
        Some(b"x"),
    );
    assert!(
        r.is_ok() && lab.work.join("doc-relative.html").exists(),
        "{r:?}"
    );
    // a read-only mount, and a directory that isn't mounted
    let ro = lab.out.join("read-only");
    let unmounted = lab.out.join("unmounted");
    for d in [&ro, &unmounted] {
        std::fs::create_dir_all(d).unwrap();
    }
    let fails = |out: &Path, media: &Path, read: &[&Path], why: &str| {
        let e = convert(&mounted(o(out, media), read, &[&dir]), Some(doc)).unwrap_err();
        assert!(e.message.contains(why), "{e}");
    };
    fails(&ro, &dir, &[&lab.work, &ro], NOT_PERMITTED);
    fails(&dir, &ro, &[&lab.work, &ro], NOT_PERMITTED);
    fails(&unmounted, &dir, &[&lab.work], DOES_NOT_EXIST);
    fails(&dir, &unmounted, &[&lab.work], DOES_NOT_EXIST);
    assert!(walk(&ro).is_empty() && walk(&unmounted).is_empty());
}

#[test]
fn a_notebook_attachment_stays_inside_the_media_directory() {
    let doc = r#"{"cells":[{"cell_type":"markdown","metadata":{},"attachments":{"../../../x.svg":{"image/svg+xml":"PHN2Zy8+"}},"source":["![x](attachment:../../../x.svg)"]}],"metadata":{},"nbformat":4,"nbformat_minor":5}"#;
    let dir = lab().out.join("ipynb");
    std::fs::create_dir_all(&dir).unwrap();
    let r = convert(
        &mounted(
            json!({"from": "ipynb", "to": "html", "extract-media": named(&dir.join("media"))}),
            &[],
            &[&dir],
        ),
        Some(doc.as_bytes()),
    );
    assert!(r.is_ok(), "{r:?}");
    let files = walk(&dir);
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(files[0].starts_with(dir.join("media")));
}

#[test]
fn what_is_refused() {
    let lab = lab();
    let refused = |sandbox: Value, why: &str| {
        let r = convert(&json!({"to": "html", "sandbox": sandbox}), Some(b"x"));
        let e = r.unwrap_err();
        assert!(
            e.kind == "PandocOptionError" && e.message.contains(why),
            "{e}"
        );
    };
    refused(json!({"read": ["no-such-directory"]}), "no-such-directory");
    refused(json!({"write": ["no-such-directory"]}), "no-such-directory");
    refused(
        json!({"read": [lab.work.join("inside.txt")]}),
        "not a directory",
    );
    refused(json!({"read": [], "network": true}), "\"network\"");
    refused(json!({"read": "."}), "lists directories");
    refused(json!({"read": [1]}), "lists directories");
    let r = libpandoc::read_many(&["x"], &json!({"sandbox": {"read": []}}));
    assert!(r.is_err());
}

/// A PDF needs a program: it fails as one does, saying so.
#[test]
fn pdf_needs_a_program() {
    let e = convert(
        &sandboxed(json!({"from": "markdown", "to": "pdf"})),
        Some(b"x"),
    )
    .unwrap_err();
    assert!(e.message.contains("runs no programs"), "{e}");
}

/// No directory at all: the input given, and nothing else.
#[test]
fn no_directory_at_all() {
    lab();
    let options = json!({"from": "markdown", "to": "html", "embed-resources": true, "sandbox": {}});
    let r = convert(&options, Some(b"*hi* ![x](inside.svg)")).unwrap();
    assert!(
        r.text().contains("<em>hi</em>") && !seen(&Ok(r), None).everything.contains("INSIDE-OK")
    );
}

/// Relative names resolve from the process's working directory when it is
/// mounted; when it isn't they find nothing, and the error says why.
#[test]
fn the_working_directory() {
    let lab = lab();
    let o = json!({"from": "markdown", "to": "plain", "input-files": ["inside.txt"]});
    let r = convert(&sandboxed(o.clone()), None).unwrap();
    assert_eq!(r.text(), "INSIDE-OK\n");
    let e = convert(&mounted(o, &[&lab.outside], &[]), None).unwrap_err();
    assert!(e.message.contains("does not exist"), "{e}");
    assert!(e.message.contains("is not in the sandbox"), "{e}");
}

/// A Lua filter runs inside, confined as the document is: it reads and
/// writes only in the mounts, runs no program, and is found only inside.
#[test]
fn a_lua_filter_is_confined() {
    let lab = lab();
    let dir = lab.out.join("lua");
    std::fs::create_dir_all(&dir).unwrap();
    let mark = |name: &str| named(&dir.join(name));
    let lua = format!(
        r#"function Pandoc(doc)
  local out = {{}}
  local function read(f)
    local h = io.open(f)
    out[#out + 1] = f .. "=" .. (h and h:read("a"):sub(1, 9) or "none")
  end
  local function write(f)
    local h = io.open(f, "w")
    out[#out + 1] = f .. "=" .. (h and "written" or "refused")
    if h then h:close() end
  end
  read("inside.txt")
  read("../secret/secret.txt")
  read("{secret}")
  write("written-by-lua.txt")
  write("{written}")
  os.execute("touch {executed}")
  pcall(io.popen, "touch {popen}")
  pcall(pandoc.pipe, "touch", {{"{pipe}"}}, "")
  return pandoc.Pandoc({{pandoc.Para(table.concat(out, " "))}})
end
"#,
        secret = named(&lab.secret.join("secret.txt")),
        written = mark("written.txt"),
        executed = mark("executed"),
        popen = mark("popen"),
        pipe = mark("pipe"),
    );
    std::fs::write(lab.work.join("confined.lua"), lua).unwrap();
    let r = convert(
        &mounted(
            json!({"from": "markdown", "to": "plain", "filters": ["confined.lua"], "wrap": "none"}),
            &[&lab.work],
            &[&dir],
        ),
        Some(b"x"),
    )
    .unwrap();
    let text = r.text();
    assert!(!text.contains("CANARY"), "{text}");
    for x in [
        "inside.txt=INSIDE-OK",
        "../secret/secret.txt=none",
        "secret.txt=none",
        "written-by-lua.txt=refused",
        "written.txt=written",
    ] {
        assert!(text.contains(x), "{x}: {text}");
    }
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    names.sort();
    assert_eq!(names, ["written.txt"]);
    // a Lua filter outside the mounts isn't found
    std::fs::copy(
        lab.work.join("confined.lua"),
        lab.outside.join("confined.lua"),
    )
    .unwrap();
    let lua = lab.outside.join("confined.lua");
    let r = convert(
        &sandboxed(json!({"from": "markdown", "to": "plain", "filters": [lua]})),
        Some(b"x"),
    );
    assert!(r.is_err(), "{r:?}");
}

/// Filters in Rust run in this process, within the conversion.
#[test]
fn a_callback_filter_runs() {
    use libpandoc::Filter;
    let r = libpandoc::convert_with(
        &sandboxed(json!({"from": "markdown", "to": "plain"})),
        Some(b"hello"),
        vec![Filter::raw(|doc, c| {
            assert_eq!(c.format.as_deref(), Some("plain"));
            Ok(String::from_utf8_lossy(doc)
                .replace("hello", "HELLO")
                .into_bytes())
        })],
    )
    .unwrap();
    assert_eq!(r.text(), "HELLO\n");
    let failed = libpandoc::convert_with(
        &sandboxed(json!({"from": "markdown", "to": "plain"})),
        Some(b"hello"),
        vec![Filter::raw(|_, _| Err("no".into()))],
    );
    assert_eq!(failed.unwrap_err().kind, "PandocFilterError");
}

/// A JSON filter (a program) is refused: wasm starts no process, and the
/// message says so, whether the program is a file or on PATH.
#[test]
fn a_json_filter_is_refused() {
    let lab = lab();
    let f = lab.work.join("filter.sh");
    let mark = lab.out.join("json-filter-ran");
    std::fs::write(&f, format!("#!/bin/sh\ntouch '{}'\ncat\n", mark.display())).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    for filter in [f.display().to_string(), "cat".into()] {
        let e = convert(
            &sandboxed(json!({"from": "markdown", "to": "plain", "filters": [filter]})),
            Some(b"hello"),
        )
        .unwrap_err();
        assert_eq!(e.kind, "PandocFilterError", "{e}");
        assert!(
            e.message.contains("runs no programs: not JSON filters"),
            "{e}"
        );
    }
    assert!(!mark.exists());
}

/// A wasm filter sees the sandbox's mounts and nothing else.
#[test]
fn a_wasm_filter_sees_the_mounts() {
    let lab = lab();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../filters");
    let status = std::process::Command::new(env!("CARGO"))
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-wasip1",
            "--quiet",
            "--bin",
            "include",
        ])
        .current_dir(&root)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "building the example filters (rustup target add wasm32-wasip1)"
    );
    let include = root.join("target/wasm32-wasip1/release/include.wasm");
    let run = |file: &str| {
        let filter = libpandoc::wasm::WasmFilter::from_file(&include).unwrap();
        libpandoc::convert_with(
            &mounted(
                json!({"from": "markdown", "to": "plain"}),
                &[&lab.work],
                &[&lab.out],
            ),
            Some(format!("```{{.include}}\n{file}\n```\n").as_bytes()),
            vec![filter.into_filter()],
        )
    };
    assert!(run("inside.txt").unwrap().text().contains("INSIDE-OK"));
    let input = lab.outside.join("input.md");
    let copy = lab.out.join("input.md");
    std::fs::copy(&input, &copy).unwrap();
    assert!(run(&named(&copy)).unwrap().text().contains("INPUT-OK"));
    for outside in [
        "../secret/secret.txt".into(),
        named(&lab.secret.join("secret.txt")),
        named(&input),
    ] {
        let out = run(&outside);
        assert!(
            out.is_err() && !format!("{out:?}").contains("CANARY"),
            "{out:?}"
        );
    }
}
