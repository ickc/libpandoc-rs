//! pandoc as a Rust library, in process: no `pandoc` executable, no
//! subprocess. Documents are [`panir`] values.
//!
//! ```
//! use serde_json::json;
//!
//! let html = libpandoc::convert(&json!({"from": "markdown", "to": "html"}), Some(b"*hi*")).unwrap();
//! assert_eq!(html.text(), "<p><em>hi</em></p>\n");
//!
//! let doc = libpandoc::read("# Title", "markdown").unwrap();
//! assert_eq!(libpandoc::write(&doc, "commonmark").unwrap().text(), "# Title\n");
//! ```
//!
//! Filters written in Rust run inside a conversion, as `--filter` would, but
//! in this process and within one pandoc run ([`convert_with`]):
//!
//! ```
//! use libpandoc::Filter;
//! use panir::{Ctx, Inline};
//! use serde_json::json;
//!
//! struct Upper;
//! impl panir::Filter for Upper {
//!     fn inline(&mut self, x: &mut Inline, _: &mut Ctx) -> Option<Vec<Inline>> {
//!         if let Inline::Str(s) = x { *s = s.to_uppercase().into(); }
//!         None
//!     }
//! }
//!
//! let out = libpandoc::convert_with(
//!     &json!({"from": "markdown", "to": "plain"}),
//!     Some(b"hello world"),
//!     vec![Filter::panir(Upper)],
//! ).unwrap();
//! assert_eq!(out.text(), "HELLO WORLD\n");
//! ```
//!
//! The library is found at build time by `libpandoc-sys` (`$LIBPANDOC_PREFIX`
//! or a conda environment), and must be libpandoc 1.15 or a later 1.x:
//! checked once, at the first call, which otherwise fails naming both
//! versions (and so is libpandoc.wasm, for the directory sandbox).
//!
//! Time: `"timeout": seconds` in the options of [`convert`], [`convert_with`]
//! and [`read_many`] stops a call that runs longer, with an [`Error`] whose
//! [`Error::is_timeout`]. It stops pandoc's own code, not code in a callback
//! (a filter in Rust runs to its end, and the call fails when it returns)
//! nor Lua (filters, custom readers and writers), which nothing stops. A
//! [`CancelToken`] (`"cancel-token"`) stops the same from another thread,
//! with an [`Error::is_cancelled`]; Ctrl-C ends the program (the library
//! handles no signal: [`CancelToken`] says how to cancel on it instead).
//! There is no memory limit for a call natively: running out of memory ends
//! the process. For documents from someone you don't trust, the directory
//! sandbox (the `wasm` feature) has both limits (the README's "Security").
//!
//! Built for WebAssembly, in a wasm filter, the same functions ([`read`],
//! [`read_many`], [`write`], [`convert`], [`query`]) call the program that
//! runs the filter, which has pandoc: pandocrs, or libpandoc.wasm in a
//! browser (see `guest.rs`). What runs filters or the pandoc command
//! ([`convert_with`], [`main`]) is only native.

use std::fmt;
#[cfg(not(target_family = "wasm"))]
use std::{
    any::Any,
    ffi::{CStr, CString, c_char, c_int, c_void},
    panic::{self, AssertUnwindSafe},
};

#[cfg(not(target_family = "wasm"))]
use libpandoc_sys as sys;
pub use panir::{Conversion, Pandoc};
use serde_json::{Value, json};

#[cfg(all(feature = "wasm", not(target_family = "wasm")))]
pub mod wasm;

#[cfg(all(feature = "wasm", not(target_family = "wasm")))]
mod sandbox;

#[cfg(not(target_family = "wasm"))]
mod cancel;
#[cfg(not(target_family = "wasm"))]
pub use cancel::CancelToken;

/// Inside a wasm filter: calls to the host.
#[cfg(target_family = "wasm")]
#[path = "guest.rs"]
mod backend;

/// An error from pandoc: its constructor (`PandocParseError`,
/// `PandocFilterError`, ...), `Timeout` (a time limit), `Cancelled` (a
/// [`CancelToken`]), or `Exception` (anything else), and its message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: String,
    pub message: String,
}

impl Error {
    /// The kind of an error that is a time limit's: a call's `"timeout"`,
    /// or the directory sandbox's (`"timeout"` in it, or
    /// `LIBPANDOC_WASM_TIMEOUT`).
    pub const TIMEOUT: &'static str = "Timeout";
    /// The kind of an error that is a [`CancelToken`]'s (`"cancel-token"`).
    pub const CANCELLED: &'static str = "Cancelled";

    fn new(kind: &str, message: impl Into<String>) -> Self {
        Error {
            kind: kind.into(),
            message: message.into(),
        }
    }

    /// Whether a time limit stopped the call ([`Error::TIMEOUT`]).
    pub fn is_timeout(&self) -> bool {
        self.kind == Self::TIMEOUT
    }

    /// Whether a [`CancelToken`] stopped the call ([`Error::CANCELLED`]).
    pub fn is_cancelled(&self) -> bool {
        self.kind == Self::CANCELLED
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// What a conversion produced: the output (what pandoc writes to standard
/// output; empty with an output file) and pandoc's log messages, as
/// `--log` writes them (warnings and info).
#[derive(Clone, Debug)]
pub struct Output {
    pub bytes: Vec<u8>,
    pub log: Vec<Value>,
}

impl Output {
    /// The output as text (lossily, for a binary format).
    pub fn text(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.bytes)
    }
}

/// The ABI this crate needs: `libpandoc-sys`'s.
#[cfg(not(target_family = "wasm"))]
const ABI: i32 = sys::LIBPANDOC_ABI_VERSION;

/// Whether `what`, of ABI version `found`, is one this crate works with
/// (`libpandoc.h`'s rule: the same major, a minor at least as large).
#[cfg(not(target_family = "wasm"))]
pub(crate) fn compatible(what: &str, found: i32) -> Result<()> {
    if found / 1000 == ABI / 1000 && found % 1000 >= ABI % 1000 {
        return Ok(());
    }
    Err(Error::new(
        "PandocAppError",
        format!(
            "{what} {}.{} is loaded, and this program needs {} {}.{} or a later {}.x",
            found / 1000,
            found % 1000,
            what,
            ABI / 1000,
            ABI % 1000,
            ABI / 1000
        ),
    ))
}

/// The library's version, checked once, before the first call into it.
#[cfg(not(target_family = "wasm"))]
fn loaded() -> Result<()> {
    static CHECKED: std::sync::OnceLock<Result<()>> = std::sync::OnceLock::new();
    CHECKED
        .get_or_init(|| compatible("libpandoc", sys::pandoc_abi_version()))
        .clone()
}

/// Calls to libpandoc itself.
#[cfg(not(target_family = "wasm"))]
mod backend {
    use super::*;

    pub(crate) fn convert(options: &str, input: Option<&[u8]>) -> Result<Output> {
        loaded()?;
        let (ip, il) = ptr(input);
        // SAFETY: `options` and `input` (or NULL) are given with their
        // lengths and borrowed for the call; `take` gets the result fresh.
        unsafe {
            take(sys::pandoc_convert(
                options.as_ptr().cast(),
                options.len(),
                ip,
                il,
            ))
        }
    }

    pub(crate) fn read_many(request: &str) -> Result<Output> {
        loaded()?;
        // SAFETY: `request` is given with its length and borrowed for the
        // call; `take` gets the result fresh.
        unsafe {
            take(sys::pandoc_read_many(
                request.as_ptr().cast(),
                request.len(),
            ))
        }
    }

    pub(crate) fn query(query: &str) -> Result<Output> {
        loaded()?;
        // SAFETY: as `read_many`.
        unsafe { take(sys::pandoc_query(query.as_ptr().cast(), query.len())) }
    }
}

/// Take a result from libpandoc and free it.
///
/// # Safety
///
/// `r` is what a libpandoc call returned (NULL panics), not yet freed, and
/// not used after.
#[cfg(not(target_family = "wasm"))]
unsafe fn take(r: *mut sys::pandoc_result) -> Result<Output> {
    assert!(!r.is_null(), "libpandoc returned no result");
    // SAFETY: a result from libpandoc, not freed (the caller's promise),
    // until `pandoc_result_free` below.
    let res = unsafe { &*r };
    let cstr = |p: *mut c_char| {
        // SAFETY: the result's strings are NUL-terminated or NULL
        // (libpandoc.h), and live until it is freed below.
        (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    };
    let out = if res.status == 0 {
        let bytes = if res.output.is_null() {
            Vec::new()
        } else {
            // SAFETY: `output` holds `output_len` bytes (libpandoc.h), until
            // the result is freed below; copied.
            unsafe { std::slice::from_raw_parts(res.output.cast::<u8>(), res.output_len) }.to_vec()
        };
        let log = cstr(res.log)
            .and_then(|l| serde_json::from_str(&l).ok())
            .unwrap_or_default();
        Ok(Output { bytes, log })
    } else {
        Err(Error {
            kind: cstr(res.error_kind).unwrap_or_else(|| "Exception".into()),
            message: cstr(res.error_message).unwrap_or_default(),
        })
    };
    // SAFETY: freed once, and nothing borrowed from it is left (`out` owns
    // copies).
    unsafe { sys::pandoc_result_free(r) };
    out
}

#[cfg(not(target_family = "wasm"))]
fn ptr(b: Option<&[u8]>) -> (*const c_char, usize) {
    match b {
        Some(b) => (b.as_ptr().cast(), b.len()),
        None => (std::ptr::null(), 0),
    }
}

/// Convert, as `pandoc` with a defaults file: `options` is a JSON object of
/// defaults-file keys (`{"from": "markdown", "to": "html"}`; a key whose
/// value is `null` is absent, in the sandbox's object too), `input` the
/// standard input (`None`: the options' `input-files`).
///
/// With `"timeout": seconds` (libpandoc's, not pandoc's), a conversion
/// still running after that long fails with an [`Error::is_timeout`] (the
/// crate's documentation says what it stops and what not); with
/// `"cancel-token": token.id()` (untrusted options: inside `"untrusted"`,
/// [`untrusted`]), one stops when the [`CancelToken`] is cancelled, with an
/// [`Error::is_cancelled`].
///
/// With `"sandbox": {"read": ["dir", ...], "write": ["dir", ...]}` (the
/// `wasm` feature), the conversion runs in libpandoc.wasm, which sees only
/// those directories (not even the user data directory, unless listed):
/// everything pandoc reads or writes must be in them (see the README's
/// "Security"). Its limits, `"timeout"` (seconds) and `"max-memory"`
/// (bytes) in it, else `LIBPANDOC_WASM_TIMEOUT` and
/// `LIBPANDOC_WASM_MAX_MEMORY`.
pub fn convert(options: &Value, input: Option<&[u8]>) -> Result<Output> {
    let mut options = present(options);
    if let Value::Object(m) = &mut options {
        // (as convert_with's options, though there are no filters here)
        m.remove("separate-filters");
    }
    let options = &options;
    #[cfg(not(target_family = "wasm"))]
    if let Some(dirs) = sandbox::from_options(options)? {
        return sandbox::convert(options, input, &dirs, None);
    }
    backend::convert(&options.to_string(), input)
}

/// The options without the keys whose value is `null`, at the top and in
/// a `"sandbox"` object: a value that is nothing is an absent option (as
/// Lua's `nil` in a table), so that a sandbox's limit of `null` is the
/// environment's. A list with a `null` in it stays as it is (an error).
fn present(options: &Value) -> Value {
    let strip = |m: &serde_json::Map<String, Value>| -> serde_json::Map<String, Value> {
        m.iter()
            .filter(|(_, v)| !v.is_null())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    };
    let Value::Object(m) = options else {
        return options.clone();
    };
    let mut m = strip(m);
    if let Some(Value::Object(s)) = m.get("sandbox") {
        let s = strip(s);
        m.insert("sandbox".into(), Value::Object(s));
    }
    Value::Object(m)
}

/// Without the `wasm` feature, a directory sandbox is refused, never run
/// without.
#[cfg(all(not(feature = "wasm"), not(target_family = "wasm")))]
mod sandbox {
    use super::{Callbacks, Error, Output, Result, Value};

    /// No directories: a sandbox can't be set up.
    pub(crate) enum Dirs {}

    fn no_wasm() -> Error {
        Error::new(
            "PandocOptionError",
            "a directory sandbox (\"sandbox\": {\"read\": [...]}, LIBPANDOC_SANDBOX) runs \
             pandoc in libpandoc.wasm, which needs libpandoc's `wasm` feature",
        )
    }

    pub(crate) fn from_options(options: &Value) -> Result<Option<Dirs>> {
        match options.get("sandbox") {
            Some(Value::Object(_)) => Err(no_wasm()),
            _ => Ok(None),
        }
    }

    pub(crate) fn from_env() -> Result<Option<Dirs>> {
        match ["LIBPANDOC_SANDBOX", "LIBPANDOC_SANDBOX_WRITE"]
            .iter()
            .any(|k| std::env::var_os(k).is_some())
        {
            true => Err(no_wasm()),
            false => Ok(None),
        }
    }

    pub(crate) fn convert(
        _: &Value,
        _: Option<&[u8]>,
        dirs: &Dirs,
        _: Option<Callbacks<'_>>,
    ) -> Result<Output> {
        match *dirs {}
    }

    pub(crate) fn convert_args<S>(_: &[S], _: Option<&[u8]>, dirs: &Dirs) -> Result<Output> {
        match *dirs {}
    }

    pub(crate) fn main<S>(_: &[S], _: Option<Callbacks<'_>>, dirs: &Dirs) -> Result<i32> {
        match *dirs {}
    }
}

/// Keep CStrings alive and give their pointers as an argv.
#[cfg(not(target_family = "wasm"))]
struct Argv {
    _strings: Vec<CString>,
    ptrs: Vec<*const c_char>,
}

#[cfg(not(target_family = "wasm"))]
impl Argv {
    fn new<S: AsRef<str>>(args: &[S]) -> Result<Argv> {
        let strings = args
            .iter()
            .map(|a| {
                CString::new(a.as_ref())
                    .map_err(|_| Error::new("Exception", "an argument contains NUL"))
            })
            .collect::<Result<Vec<_>>>()?;
        let ptrs = strings.iter().map(|s| s.as_ptr()).collect();
        Ok(Argv {
            _strings: strings,
            ptrs,
        })
    }
    fn argc(&self) -> c_int {
        self.ptrs.len() as c_int
    }
}

/// Convert, as `pandoc ARGS`: `args` are pandoc's arguments (not the
/// program's name), `input` the standard input. Informational options
/// (`--version`, `--list-*`) are rejected: use [`query`].
///
/// With `LIBPANDOC_SANDBOX` or `LIBPANDOC_SANDBOX_WRITE` set (the `wasm`
/// feature), in the directory sandbox, as [`convert`]'s `"sandbox"`: the
/// arguments go to pandoc as they are.
#[cfg(not(target_family = "wasm"))]
pub fn convert_args<S: AsRef<str>>(args: &[S], input: Option<&[u8]>) -> Result<Output> {
    if let Some(dirs) = sandbox::from_env()? {
        return sandbox::convert_args(args, input, &dirs);
    }
    loaded()?;
    let argv = Argv::new(args)?;
    let (ip, il) = ptr(input);
    // SAFETY: `argv` holds `argc` pointers to NUL-terminated strings, which
    // it keeps alive past the call, as `input` (or NULL) with its length;
    // `take` gets the result fresh.
    unsafe {
        take(sys::pandoc_convert_args(
            argv.argc(),
            argv.ptrs.as_ptr(),
            ip,
            il,
        ))
    }
}

/// Ask pandoc something (`libpandoc.h` lists the queries): `name` with
/// the other keys of `params` (an object, or `Value::Null`).
pub fn query(name: &str, params: Value) -> Result<Value> {
    let mut q = json!({ "query": name });
    if let Value::Object(m) = params {
        q.as_object_mut().unwrap().extend(m);
    }
    let q = q.to_string();
    let out = backend::query(&q)?;
    serde_json::from_slice(&out.bytes).map_err(|e| Error::new("Exception", e.to_string()))
}

/// The version of pandoc inside, e.g. `3.11`.
pub fn pandoc_version() -> Result<String> {
    Ok(query("version", Value::Null)?
        .as_str()
        .unwrap_or_default()
        .to_owned())
}

/// The pandoc-types API version of its AST, e.g. `[1, 23, 1]`.
pub fn pandoc_api_version() -> Result<Vec<u64>> {
    let v = query("api-version", Value::Null)?;
    Ok(v.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .collect())
}

/// How many threads pandoc runs on.
pub fn num_threads() -> Result<usize> {
    Ok(query("num-threads", Value::Null)?.as_u64().unwrap_or(1) as usize)
}

/// The number of threads pandoc is to run on: at least 1, at most one per
/// processor the process may use (libpandoc clamps it); the number in
/// effect.
///
/// pandoc starts on one thread, and grows to this number once, the first
/// time it runs something in parallel (a call while another thread's is
/// running, or [`read_many`] of several inputs). This sets the number only
/// until then: after, it changes nothing and returns the number in effect
/// (libpandoc 1.9; changed while conversions ran, the number hung GHC's
/// runtime now and then). So a program choosing it does so before it first
/// calls pandoc from two threads at once; `LIBPANDOC_NUM_THREADS` does the
/// same from the environment. May be called from any thread.
#[cfg(not(target_family = "wasm"))]
pub fn set_num_threads(n: usize) -> Result<usize> {
    loaded()?;
    match sys::pandoc_set_num_threads(c_int::try_from(n).unwrap_or(c_int::MAX)) {
        n if n > 0 => Ok(n as usize),
        _ => Err(Error::new(
            "PandocAppError",
            "pandoc's runtime isn't running",
        )),
    }
}

/// Parse text into a document.
pub fn read(input: &str, from: &str) -> Result<Pandoc> {
    let out = convert(&json!({"from": from, "to": "json"}), Some(input.as_bytes()))?;
    parse(&out.bytes)
}

fn parse(json: &[u8]) -> Result<Pandoc> {
    let s = std::str::from_utf8(json).map_err(|e| Error::new("Exception", e.to_string()))?;
    panir::from_str(s).map_err(|e| Error::new("Exception", e.to_string()))
}

/// Parse text as a filter's conversion reads its input (its input format
/// and reading options): for filters that parse fragments.
pub fn read_as(input: &str, conversion: &Conversion) -> Result<Pandoc> {
    let mut docs = read_many_as(&[input], conversion)?.docs.into_iter();
    docs.next()
        .unwrap_or_else(|| Err(Error::new("Exception", "pandoc read no document")))
}

/// What [`read_many`] read: each input's document, or why it couldn't be
/// read, in the order of the inputs; and pandoc's messages (the reader's
/// warnings, such as an include file not found), as a conversion's
/// [`Output::log`]: the setup's, then each input's, in order.
#[derive(Clone, Debug)]
pub struct ReadMany {
    pub docs: Vec<Result<Pandoc>>,
    pub log: Vec<Value>,
}

impl ReadMany {
    /// Every document, or the first input's error.
    pub fn all(self) -> Result<Vec<Pandoc>> {
        self.docs.into_iter().collect()
    }
}

/// Parse many texts, each on its own, in parallel, with the reader set up
/// once: `options` are the defaults-file keys that affect reading
/// (`{"from": "commonmark_x"}`).
///
/// With `"timeout": seconds` in `options`, the call returns within about
/// that long: an input not read by then is an [`Error::is_timeout`] in its
/// place, and the others are kept (the call itself fails so only if setting
/// up the reader took that long).
pub fn read_many<S: AsRef<str>>(inputs: &[S], options: &Value) -> Result<ReadMany> {
    let options = &present(options);
    if let Some(Value::Object(_)) = options.get("sandbox") {
        return Err(Error::new(
            "PandocOptionError",
            "read_many: a directory sandbox is for conversions (\"sandbox\": true reads no files)",
        ));
    }
    let inputs: Vec<&str> = inputs.iter().map(AsRef::as_ref).collect();
    let req = json!({"options": options, "inputs": inputs}).to_string();
    let out = backend::read_many(&req)?;
    let docs: Vec<Value> =
        serde_json::from_slice(&out.bytes).map_err(|e| Error::new("Exception", e.to_string()))?;
    let docs = docs
        .into_iter()
        .enumerate()
        .map(|(i, d)| match d.get("error") {
            Some(e) => Err(Error {
                kind: e["kind"].as_str().unwrap_or("Exception").into(),
                message: format!(
                    "reading input {i}: {}",
                    e["message"].as_str().unwrap_or_default()
                ),
            }),
            None => parse(d.to_string().as_bytes()),
        })
        .collect();
    Ok(ReadMany { docs, log: out.log })
}

/// [`read_many`], read as `conversion` reads its input.
pub fn read_many_as<S: AsRef<str>>(inputs: &[S], conversion: &Conversion) -> Result<ReadMany> {
    read_many(inputs, &read_options(conversion, None))
}

/// The options to read a fragment as `conversion` reads its input: its
/// input format (or `format`) and the options that affect reading; not
/// filters, templates, metadata or the output.
///
/// The input format is the one pandoc decided on when known, else the
/// options' `from`, else markdown.
///
/// When the conversion is sandboxed, or when that can't be told, a
/// fragment is read in pandoc's own sandbox (`"sandbox": true`: it reads
/// no files, as LaTeX's `\input` or an RST or Org include would). A
/// conversion is sandboxed when its `sandbox` is `Some(true)` (pandoc's
/// sandbox is on, however it was set: libpandoc 1.13 tells its filters; a
/// directory sandbox, whose callbacks run in this process outside it, says
/// so too), or its options ask for a sandbox (`"sandbox"`: `true` or a
/// directory sandbox); it can't be told when its `sandbox` is `None` and
/// it has no options (one made by hand, or from the environment under
/// plain pandoc, which tells its filters nothing). And the text is
/// read as given, its tabs kept (`"preserve-tabs": true`), as Lua's
/// `pandoc.read` reads it: the `pandoc` command first replaces them by
/// spaces. A filter that wants the files read, or the tabs expanded, sets
/// `"sandbox"` or `"preserve-tabs"` to `false` in what this returns (its
/// code, its choice).
pub fn read_options(conversion: &Conversion, format: Option<&str>) -> Value {
    const READ: [&str; 8] = [
        "abbreviations",
        "data-dir",
        "default-image-extension",
        "indented-code-classes",
        "resource-path",
        "strip-comments",
        "tab-stop",
        "track-changes",
    ];
    const READER: [&str; 4] = [
        "default-image-extension",
        "indented-code-classes",
        "strip-comments",
        "tab-stop",
    ];
    let mut opts = serde_json::Map::new();
    if let Some(Value::Object(o)) = &conversion.options {
        opts.extend(
            o.iter()
                .filter(|(k, _)| READ.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone())),
        );
    } else if let Some(Value::Object(ro)) = &conversion.reader_options {
        opts.extend(
            ro.iter()
                .filter(|(k, _)| READER.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        let tc = match ro.get("track-changes").and_then(Value::as_str) {
            Some("accept-changes") => Some("accept"),
            Some("reject-changes") => Some("reject"),
            Some("all-changes") => Some("all"),
            _ => None,
        };
        if let Some(tc) = tc {
            opts.insert("track-changes".into(), tc.into());
        }
    }
    if sandboxed(conversion) {
        opts.insert("sandbox".into(), true.into());
    }
    opts.insert("preserve-tabs".into(), true.into());
    let given = conversion.options.as_ref();
    let from = format
        .map(Value::from)
        .or_else(|| conversion.input_format.clone().map(Value::from))
        .or_else(|| given.and_then(|o| o.get("from").or_else(|| o.get("reader")).cloned()))
        .unwrap_or_else(|| "markdown".into());
    opts.insert("from".into(), from);
    Value::Object(opts)
}

/// Whether a conversion is sandboxed, for what a filter reads through it
/// ([`read_options`]): told so, or its options ask for it, or nothing can
/// tell (no `sandbox`, no options): failing closed.
fn sandboxed(conversion: &Conversion) -> bool {
    let asked = conversion.options.as_ref().is_some_and(|o| {
        !matches!(
            o.get("sandbox"),
            None | Some(Value::Null | Value::Bool(false))
        )
    });
    conversion.sandbox == Some(true)
        || asked
        || (conversion.sandbox.is_none() && conversion.options.is_none())
}

/// Run citeproc on a document, as a filter does: its citations rendered
/// and its bibliography added, from the files its metadata names
/// (`bibliography`, a name or a list; `csl` or `citation-style`;
/// `citation-abbreviations`) or the references in it.
///
/// Where the files are read, `conversion` decides, as for [`read_as`]:
///
/// - In a wasm filter, every call to pandoc reads no file, so this reads
///   them itself, with the access the filter has (the directories its host
///   gave it), and hands them to pandoc as their contents (libpandoc 1.14's
///   `"citeproc-files"`), each under the name pandoc looks up: a
///   bibliography as the metadata names it, `csl: style` as `style.csl`,
///   `citation-abbreviations: abbr` as `abbr.json` (a name with a dot as
///   it is). A file it can't read (outside its directories, missing, not
///   UTF-8, a URL) is left out, and pandoc then reports it missing as it
///   would. Not a dependent style's parent, nor the data directory's
///   `csl/`.
/// - Natively, when the conversion is sandboxed (or nothing can tell): in
///   pandoc's sandbox, reading no file the document names, only those the
///   conversion's options name (`bibliography`, `csl`,
///   `citation-abbreviations`), as the conversion's own citeproc would. A
///   filter in Rust runs with the process's access, and reading what the
///   document names would let it out of the sandbox.
/// - Natively otherwise: pandoc's citeproc as it is, reading the files.
pub fn citeproc(doc: &Pandoc, conversion: &Conversion) -> Result<Pandoc> {
    let mut opts = json!({"from": "json", "to": "json", "citeproc": true});
    #[cfg(target_family = "wasm")]
    {
        let _ = conversion;
        opts["sandbox"] = true.into();
        opts["citeproc-files"] = citeproc_files(&doc.meta).into();
    }
    #[cfg(not(target_family = "wasm"))]
    if sandboxed(conversion) {
        opts["sandbox"] = true.into();
        if let Some(Value::Object(o)) = &conversion.options {
            for k in ["bibliography", "csl", "citation-abbreviations"] {
                if let Some(v) = o.get(k) {
                    opts[k] = v.clone();
                }
            }
        }
    }
    let out = convert(&opts, Some(panir::to_string(doc).as_bytes()))?;
    parse(&out.bytes)
}

/// The files citeproc looks up for `meta`, as libpandoc's
/// `"citeproc-files"`: each name it looks up, with the text of the file of
/// that name if it can be read.
#[cfg(target_family = "wasm")]
fn citeproc_files(meta: &panir::Meta) -> serde_json::Map<String, Value> {
    citeproc_names(meta)
        .into_iter()
        .filter_map(|name| {
            let text = std::fs::read_to_string(&name).ok()?;
            Some((name, text.into()))
        })
        .collect()
}

/// The names citeproc looks up the files of `meta` by.
#[cfg(any(target_family = "wasm", test))]
fn citeproc_names(meta: &panir::Meta) -> Vec<String> {
    // pandoc adds the extension to a name without a dot
    let style = |v: &panir::MetaValue, ext: &str| {
        let name = panir::stringify(v);
        match name.contains('.') {
            true => name,
            false => name + ext,
        }
    };
    let bibliographies = panir::items(meta.get("bibliography"))
        .iter()
        .map(panir::stringify);
    let csl = meta
        .get("csl")
        .or(meta.get("citation-style"))
        .map(|v| style(v, ".csl"));
    let abbreviations = meta
        .get("citation-abbreviations")
        .map(|v| style(v, ".json"));
    bibliographies.chain(csl).chain(abbreviations).collect()
}

/// Options from code you don't trust, as a host gives them to pandoc (as a
/// wasm filter's calls are given): `options` with `"untrusted"` written
/// over whatever the code wrote, `true`, or with `token`,
/// `{"cancel-token": token.id()}` (libpandoc 1.14), so that cancelling the
/// token stops the call. libpandoc then refuses the options that read or
/// write files, fetch or run anything, a `"cancel-token"` of the code's own
/// among them (the README's "Untrusted options"). Options that aren't an
/// object are refused. The code may name the include step
/// (`"filters": ["panclude"]`), which then reads nothing:
/// [`untrusted_with_roots`] gives it directories.
#[cfg(not(target_family = "wasm"))]
pub fn untrusted(options: Value, token: Option<&CancelToken>) -> Result<Value> {
    host_writes(options, token, None)
}

/// [`untrusted`], and the include step's roots, which the host gives the
/// code (`"untrusted": {"panclude-roots": [...]}`, libpandoc 1.15): the
/// code's `"filters": ["panclude"]` then reads files inside them, and only
/// there. The code can't name roots itself (libpandoc refuses them).
#[cfg(not(target_family = "wasm"))]
pub fn untrusted_with_roots<S: Into<String>>(
    options: Value,
    token: Option<&CancelToken>,
    panclude_roots: impl IntoIterator<Item = S>,
) -> Result<Value> {
    let roots = panclude_roots.into_iter().map(Into::into).collect();
    host_writes(options, token, Some(roots))
}

/// `options` with the host's `"untrusted"` written over the code's.
#[cfg(not(target_family = "wasm"))]
fn host_writes(
    options: Value,
    token: Option<&CancelToken>,
    roots: Option<Vec<String>>,
) -> Result<Value> {
    let Value::Object(mut o) = options else {
        return Err(Error::new(
            "PandocOptionError",
            "not allowed for untrusted code: options that aren't an object",
        ));
    };
    let mut host = serde_json::Map::new();
    if let Some(t) = token {
        host.insert("cancel-token".into(), t.id().into());
    }
    if let Some(r) = roots {
        host.insert("panclude-roots".into(), r.into());
    }
    let value = match host.is_empty() {
        true => true.into(),
        false => Value::Object(host),
    };
    o.insert("untrusted".into(), value);
    Ok(Value::Object(o))
}

/// Render a document, as converting it from JSON would.
pub fn write(doc: &Pandoc, to: &str) -> Result<Output> {
    write_with(doc, &json!({ "to": to }))
}

/// [`write`] with more options (defaults-file keys).
pub fn write_with(doc: &Pandoc, options: &Value) -> Result<Output> {
    let mut opts = present(options);
    opts["from"] = "json".into();
    convert(&opts, Some(panir::to_string(doc).as_bytes()))
}

#[cfg(not(target_family = "wasm"))]
type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[cfg(not(target_family = "wasm"))]
/// A filter implemented in this process, on pandoc's JSON: gets the
/// document and the conversion it runs in, returns the new document.
pub type RawFilter<'a> = Box<dyn FnMut(&[u8], &Conversion) -> Result<Vec<u8>, BoxError> + 'a>;

#[cfg(not(target_family = "wasm"))]
/// A filter implemented in this process, on the document: changes it in
/// place, given the conversion it runs in.
pub type FuncFilter<'a> = Box<dyn FnMut(&mut Pandoc, &Conversion) -> Result<(), BoxError> + 'a>;

#[cfg(not(target_family = "wasm"))]
/// A filter in a conversion: one of pandoc's own (Lua, JSON filters,
/// citeproc), or one run in this process.
///
/// pandoc hands a filter in this process the whole document as its JSON
/// and reads back what it returns: an exchange. Consecutive filters of
/// this process ([`Filter::Func`], [`Filter::Raw`], [`Filter::Wasm`])
/// share one, unless `"separate-filters": true` (or
/// `LIBPANDOC_SEPARATE_FILTERS=1`) makes each its own; one of pandoc's own
/// ends a run (the crate's README: "Consecutive filters").
pub enum Filter<'a> {
    /// An entry of pandoc's "filters" option, as is: a path, `"citeproc"`,
    /// `"panclude"`, or `{"type": "lua", "path": ...}`.
    Pandoc(Value),
    /// A filter in this process, on the document ([`Filter::func`],
    /// [`Filter::panir`]).
    Func(FuncFilter<'a>),
    /// A filter in this process, on pandoc's JSON ([`Filter::raw`]).
    Raw(RawFilter<'a>),
    /// A wasm filter, run in this process: as a callback, seeing its own
    /// directories (none by default), in a directory sandbox too.
    #[cfg(feature = "wasm")]
    Wasm(wasm::WasmFilter),
}

#[cfg(not(target_family = "wasm"))]
impl<'a> Filter<'a> {
    pub fn lua(path: &str) -> Self {
        Filter::Pandoc(json!({"type": "lua", "path": path}))
    }

    /// A JSON filter: a program pandoc runs.
    pub fn json(path: &str) -> Self {
        Filter::Pandoc(json!({"type": "json", "path": path}))
    }

    pub fn citeproc() -> Self {
        Filter::Pandoc(json!({"type": "citeproc"}))
    }

    /// The include step built into libpandoc (1.15),
    /// [panclude](https://github.com/ickc/panclude): the `include` attribute
    /// of code blocks and divs, filled from files read only inside its
    /// roots, here the main document's directory (the working directory
    /// when the input is given), none under pandoc's sandbox. Put first,
    /// so that the filters after it see the files' content as if the
    /// document held it: a wasm filter given no directory too. One of
    /// pandoc's own, it ends a run of filters of this process (the README's
    /// "The include step").
    pub fn panclude() -> Self {
        Filter::Pandoc(json!("panclude"))
    }

    /// [`Filter::panclude`] reading only inside `roots`, under pandoc's
    /// sandbox too: directories, relative ones from the working directory
    /// (in the directory sandbox, as the module sees them: the same paths
    /// on Unix). One that isn't a directory fails the conversion.
    pub fn panclude_roots<S: Into<String>>(roots: impl IntoIterator<Item = S>) -> Self {
        let roots: Vec<String> = roots.into_iter().map(Into::into).collect();
        Filter::Pandoc(json!({"type": "panclude", "roots": roots}))
    }

    /// A panir filter, as pandoc runs Lua filters.
    pub fn panir<F: panir::Filter + 'a>(mut f: F) -> Self {
        Filter::func(move |doc, conversion| {
            panir::apply_with(doc, &mut f, conversion)?;
            Ok(())
        })
    }

    /// A function changing the document.
    pub fn func<F>(f: F) -> Self
    where
        F: FnMut(&mut Pandoc, &Conversion) -> Result<(), BoxError> + 'a,
    {
        Filter::Func(Box::new(f))
    }

    /// A function on pandoc's JSON.
    pub fn raw<F>(f: F) -> Self
    where
        F: FnMut(&[u8], &Conversion) -> Result<Vec<u8>, BoxError> + 'a,
    {
        Filter::Raw(Box::new(f))
    }
}

/// The environment variable that makes each filter of this process its
/// own exchange with pandoc (`1`), when the options don't say
/// (`"separate-filters"`).
pub const SEPARATE_FILTERS_VAR: &str = "LIBPANDOC_SEPARATE_FILTERS";

#[cfg(not(target_family = "wasm"))]
/// Whether each filter is its own exchange: `options`' `"separate-filters"`
/// (taken out of them: it is this crate's, not pandoc's), else
/// [`SEPARATE_FILTERS_VAR`]: `1` yes; unset, empty or `0` no; anything else
/// an error, as a safety switch is not to be ignored.
fn separate_filters(options: Option<&mut Value>) -> Result<bool> {
    if let Some(Value::Object(m)) = options
        && let Some(v) = m.remove("separate-filters")
    {
        return v.as_bool().ok_or_else(|| {
            Error::new(
                "PandocOptionError",
                format!("\"separate-filters\": true or false, not {v}"),
            )
        });
    }
    match std::env::var_os(SEPARATE_FILTERS_VAR) {
        None => Ok(false),
        Some(v) if v.is_empty() || v == "0" => Ok(false),
        Some(v) if v == "1" => Ok(true),
        Some(v) => Err(Error::new(
            "PandocOptionError",
            format!(
                "{SEPARATE_FILTERS_VAR}: 1 (each filter its own exchange with pandoc), or 0 or \
                 empty, not {v:?}"
            ),
        )),
    }
}

#[cfg(not(target_family = "wasm"))]
/// What one callback of libpandoc's runs, one exchange with pandoc:
/// consecutive filters of this process (never [`Filter::Pandoc`]), each
/// with its place in the list given.
pub(crate) struct Callback<'a>(Vec<(usize, Filter<'a>)>);

#[cfg(not(target_family = "wasm"))]
impl Callback<'_> {
    /// Run on the document pandoc gave, in `conversion`; the new document.
    /// From one filter to the next: to a [`Filter::Func`] after another the
    /// document as it is, after a filter on JSON parsed once; to a filter on
    /// JSON after a `Func` written once, after another filter on JSON its
    /// bytes as they are, unchecked. A failure names the filter, and the
    /// one that wrote its input when that went unchecked.
    pub(crate) fn run(&mut self, doc: &[u8], conversion: &Conversion) -> Result<Vec<u8>, BoxError> {
        /// The document: JSON (pandoc's, or what the filter at a place
        /// wrote), or parsed.
        enum Doc<'d> {
            Json(std::borrow::Cow<'d, [u8]>, Option<usize>),
            Parsed(Pandoc),
        }
        let parse = |b: &[u8]| -> Result<Pandoc, BoxError> {
            Ok(panir::from_str(std::str::from_utf8(b)?)?)
        };
        let mut d = Doc::Json(doc.into(), None);
        for (k, (i, f)) in self.0.iter_mut().enumerate() {
            // between two: pandoc stops a cancelled conversion only when
            // the callback returns
            if k > 0 && cancelled(conversion) {
                return Err("the call was cancelled".into());
            }
            let i = *i;
            let failed = |e: BoxError, from: Option<usize>| -> BoxError {
                let hint = from.map_or(String::new(), |j| {
                    format!(
                        " (its input is what filters[{j}] wrote, unchecked: \
                         {SEPARATE_FILTERS_VAR}=1 has pandoc check it)"
                    )
                });
                format!("filters[{i}]: {e}{hint}").into()
            };
            d = match f {
                Filter::Func(f) => {
                    let mut p = match d {
                        Doc::Parsed(p) => p,
                        Doc::Json(b, from) => parse(&b).map_err(|e| failed(e, from))?,
                    };
                    f(&mut p, conversion).map_err(|e| failed(e, None))?;
                    Doc::Parsed(p)
                }
                f => {
                    let (b, from) = match d {
                        Doc::Json(b, from) => (b, from),
                        Doc::Parsed(p) => (panir::to_string(&p).into_bytes().into(), None),
                    };
                    let out = match f {
                        Filter::Raw(f) => f(&b, conversion),
                        #[cfg(feature = "wasm")]
                        Filter::Wasm(w) => w.run(&b, conversion),
                        Filter::Func(_) | Filter::Pandoc(_) => unreachable!("not in a run"),
                    };
                    Doc::Json(out.map_err(|e| failed(e, from))?.into(), Some(i))
                }
            };
        }
        Ok(match d {
            Doc::Json(b, _) => b.into_owned(),
            Doc::Parsed(p) => panir::to_string(&p).into_bytes(),
        })
    }
}

#[cfg(not(target_family = "wasm"))]
/// Whether the conversion's cancel token (a [`CancelToken`] of this
/// process's, in its options, or inside their `"untrusted"`) was cancelled.
fn cancelled(conversion: &Conversion) -> bool {
    let Some(o) = &conversion.options else {
        return false;
    };
    o.get("cancel-token")
        .or_else(|| o.get("untrusted").and_then(|u| u.get("cancel-token")))
        .and_then(Value::as_i64)
        .and_then(cancel::flag)
        .is_some_and(|f| f.load(std::sync::atomic::Ordering::SeqCst))
}

#[cfg(not(target_family = "wasm"))]
/// The "filters" entries for some filters, and the callbacks they name by
/// index.
pub(crate) type Callbacks<'a> = (Vec<Value>, Vec<Callback<'a>>);

#[cfg(not(target_family = "wasm"))]
/// The "filters" entries for `filters`, those of this process as the
/// callbacks they name by index: consecutive ones as one, or with
/// `separate` each its own.
pub(crate) fn callbacks(filters: Vec<Filter<'_>>, separate: bool) -> Callbacks<'_> {
    let mut entries = Vec::new();
    let mut callbacks: Vec<Callback> = Vec::new();
    // whether the last entry is a callback that takes the next filter
    let mut open = false;
    for (i, f) in filters.into_iter().enumerate() {
        if let Filter::Pandoc(v) = f {
            entries.push(v);
            open = false;
            continue;
        }
        match callbacks.last_mut() {
            Some(c) if open => c.0.push((i, f)),
            _ => {
                entries.push(json!({"type": "callback", "index": callbacks.len()}));
                callbacks.push(Callback(vec![(i, f)]));
            }
        }
        open = !separate;
    }
    (entries, callbacks)
}

#[cfg(not(target_family = "wasm"))]
/// The state of one callback: what it runs, the conversion's options, and
/// a panic caught in it, raised again after pandoc returns.
struct Slot<'a, 'b> {
    f: &'b mut Callback<'a>,
    options: Option<Value>,
    panic: Option<Box<dyn Any + Send>>,
}

/// The `pandoc_filter_fn` of every callback filter.
///
/// # Safety
///
/// Called by libpandoc, during the conversion `with_filters` started, with
/// the `userdata` it gave for this function and the arguments
/// `libpandoc.h` describes.
#[cfg(not(target_family = "wasm"))]
unsafe extern "C" fn trampoline(
    userdata: *mut c_void,
    doc: *const c_char,
    doc_len: usize,
    context: *const c_char,
    context_len: usize,
    out: *mut sys::pandoc_buffer,
) -> c_int {
    // SAFETY: `userdata` is the `Slot` `with_filters` gave libpandoc with this
    // function, alive and not otherwise borrowed while libpandoc runs (a
    // filter that calls libpandoc again starts a conversion with slots of
    // its own); `doc` and `context` hold `doc_len` and `context_len` bytes
    // for this call (libpandoc.h), and are not written during it.
    let (slot, doc, context) = unsafe {
        (
            &mut *userdata.cast::<Slot>(),
            bytes(doc, doc_len),
            bytes(context, context_len),
        )
    };
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        let conversion = conversion(context, slot.options.clone());
        slot.f.run(doc, &conversion)
    }));
    let (status, answer) = match result {
        Ok(Ok(doc)) => (0, doc),
        Ok(Err(e)) => (1, e.to_string().into_bytes()),
        Err(p) => {
            let msg = panic_message(&p);
            slot.panic = Some(p);
            (1, format!("the filter panicked: {msg}").into_bytes())
        }
    };
    // SAFETY: `out` is the buffer libpandoc passed for this call (NULL is
    // ignored); `answer` is given with its length, and copied.
    unsafe { sys::pandoc_buffer_set(out, answer.as_ptr().cast(), answer.len()) };
    status
}

/// `len` bytes at `p`; empty for NULL or 0 (libpandoc passes neither, but
/// `from_raw_parts` on NULL would be undefined behaviour even for 0).
///
/// # Safety
///
/// Unless NULL, `p` is readable for `len` bytes, unchanged for `'a`.
#[cfg(not(target_family = "wasm"))]
unsafe fn bytes<'a>(p: *const c_char, len: usize) -> &'a [u8] {
    if p.is_null() || len == 0 {
        return &[];
    }
    // SAFETY: not NULL, and the caller's promise.
    unsafe { std::slice::from_raw_parts(p.cast::<u8>(), len) }
}

#[cfg(not(target_family = "wasm"))]
fn panic_message(p: &Box<dyn Any + Send>) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(no message)".into())
}

#[cfg(not(target_family = "wasm"))]
/// The conversion libpandoc describes to a callback, in its context (JSON
/// text: what isn't JSON says nothing).
fn conversion(context: &[u8], options: Option<Value>) -> Conversion {
    Conversion::from_context(
        &serde_json::from_slice(context).unwrap_or_default(),
        options,
    )
}

#[cfg(not(target_family = "wasm"))]
/// Run `call` with the callbacks among `filters` as libpandoc's filter
/// array, and the "filters" entries naming them; raise a filter's panic.
fn with_filters<T>(
    (entries, mut fns): Callbacks<'_>,
    options: Option<Value>,
    call: impl FnOnce(Vec<Value>, &[sys::pandoc_filter]) -> T,
) -> T {
    let mut slots: Vec<Slot> = fns
        .iter_mut()
        .map(|f| Slot {
            f,
            options: options.clone(),
            panic: None,
        })
        .collect();
    let array: Vec<sys::pandoc_filter> = slots
        .iter_mut()
        .map(|s| sys::pandoc_filter {
            fn_: Some(trampoline),
            userdata: (s as *mut Slot).cast(),
        })
        .collect();
    let result = call(entries, &array);
    if let Some(p) = slots.iter_mut().find_map(|s| s.panic.take()) {
        panic::resume_unwind(p);
    }
    result
}

#[cfg(not(target_family = "wasm"))]
/// Convert with filters: `filters` replace the options' "filters", in
/// order, and those in Rust run in this process, within this conversion. A
/// filter's error fails the conversion (`PandocFilterError`); its panic is
/// raised again here. In a directory sandbox ([`convert`]), the filters in
/// Rust and wasm filters run in this process (a wasm filter seeing its own
/// directories, not the sandbox's), Lua filters inside the sandbox, and
/// JSON filters (programs) not at all.
///
/// Consecutive filters of this process are one exchange with pandoc
/// ([`Filter`]); `"separate-filters": true` in the options (this crate's,
/// not passed on to pandoc; by default [`SEPARATE_FILTERS_VAR`]) makes
/// each its own, so that pandoc reads and checks the document between any
/// two.
pub fn convert_with(
    options: &Value,
    input: Option<&[u8]>,
    filters: Vec<Filter<'_>>,
) -> Result<Output> {
    let mut options = present(options);
    let filters = callbacks(filters, separate_filters(Some(&mut options))?);
    let options = &options;
    if let Some(dirs) = sandbox::from_options(options)? {
        return sandbox::convert(options, input, &dirs, Some(filters));
    }
    loaded()?;
    let user = user_options(options);
    with_filters(filters, Some(user), |entries, array| {
        let mut opts = options.clone();
        opts["filters"] = Value::Array(entries);
        let opts = opts.to_string();
        let (ip, il) = ptr(input);
        // SAFETY: `opts` and `input` (or NULL) are given with their lengths;
        // `array` is `with_filters`'s: each entry the trampoline with a
        // `Slot` that lives (as do `opts`, `input`) until the call returns;
        // `take` gets the result fresh.
        unsafe {
            take(sys::pandoc_convert_filters(
                opts.as_ptr().cast(),
                opts.len(),
                ip,
                il,
                array.as_ptr(),
                array.len(),
            ))
        }
    })
}

#[cfg(not(target_family = "wasm"))]
/// The options a callback's conversion has: the conversion's, but its
/// filters.
fn user_options(options: &Value) -> Value {
    let mut user = options.clone();
    if let Value::Object(m) = &mut user {
        m.remove("filters");
    }
    user
}

#[cfg(not(target_family = "wasm"))]
/// The pandoc command, in this process: what `pandoc ARGS` would do, with
/// `args[0]` the program's name. pandoc reads standard input, writes
/// standard output and error, and answers `--version`, `--help` and the
/// like itself; the result is its exit status.
///
/// With `filters`, they replace those pandoc found in the arguments and
/// defaults files ([`parse_args`] tells which those are); consecutive ones
/// of this process are one exchange with pandoc, unless
/// [`SEPARATE_FILTERS_VAR`] is `1`.
///
/// With `LIBPANDOC_SANDBOX` or `LIBPANDOC_SANDBOX_WRITE` set (the `wasm`
/// feature), pandoc runs in the directory sandbox (libpandoc.wasm's
/// command, on this process's standard input, output and error), and
/// `filters` in this process, as [`convert_with`]'s in a sandbox.
pub fn main<S: AsRef<str>>(args: &[S], filters: Option<Vec<Filter<'_>>>) -> Result<i32> {
    let filters = match filters {
        Some(f) => Some(callbacks(f, separate_filters(None)?)),
        None => None,
    };
    if let Some(dirs) = sandbox::from_env()? {
        return sandbox::main(args, filters, &dirs);
    }
    loaded()?;
    let argv = Argv::new(args)?;
    let Some(filters) = filters else {
        // SAFETY: `argv` holds `argc` pointers to NUL-terminated strings,
        // alive past the call; no filters.
        return Ok(unsafe {
            sys::pandoc_main(
                argv.argc(),
                argv.ptrs.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
            )
        });
    };
    Ok(with_filters(filters, None, |entries, array| {
        let fj = Value::Array(entries).to_string();
        // SAFETY: as above, with `fj` given with its length and `array` as
        // in `convert_with`, alive until the call returns.
        unsafe {
            sys::pandoc_main(
                argv.argc(),
                argv.ptrs.as_ptr(),
                fj.as_ptr().cast(),
                fj.len(),
                array.as_ptr(),
                array.len(),
            )
        }
    }))
}

/// What pandoc makes of command-line arguments (without the program's
/// name): `{"filters": [...]}`, `{"informational": ...}` or
/// `{"subcommand": ...}`.
pub fn parse_args<S: AsRef<str>>(args: &[S]) -> Result<Value> {
    let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
    query("parse-args", json!({ "args": args }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_abi_rule() {
        assert!(compatible("x", ABI).is_ok());
        assert!(compatible("x", ABI + 1).is_ok());
        for older in [ABI - 1, ABI + 1000, ABI - 1000, 0] {
            let e = compatible("libpandoc", older).unwrap_err();
            let (have, need) = (
                format!("{}.{}", older / 1000, older % 1000),
                format!("{}.{}", ABI / 1000, ABI % 1000),
            );
            assert!(
                e.message.contains(&have) && e.message.contains(&need),
                "{e}"
            );
        }
        assert!(loaded().is_ok());
    }

    #[test]
    fn the_names_citeproc_looks_up() {
        let doc = read(
            "---\nbibliography: [a.bib, sub/b.json, /x/c.yaml]\n\
             csl: style\ncitation-abbreviations: abbr\n---\n",
            "markdown",
        )
        .unwrap();
        let names = citeproc_names(&doc.meta);
        assert_eq!(
            names,
            ["a.bib", "sub/b.json", "/x/c.yaml", "style.csl", "abbr.json"]
        );
        let doc = read(
            "---\nbibliography: refs.bib\ncitation-style: dir.d/style\n\
             citation-abbreviations: abbr.yaml\n---\n",
            "markdown",
        )
        .unwrap();
        let names = citeproc_names(&doc.meta);
        assert_eq!(names, ["refs.bib", "dir.d/style", "abbr.yaml"]);
        assert!(citeproc_names(&Pandoc::default().meta).is_empty());
    }

    #[test]
    fn api_version_matches_panir() {
        let v = pandoc_api_version().unwrap();
        assert_eq!(
            v[..2],
            panir::PANDOC_API_VERSION[..2]
                .iter()
                .map(|&x| x as u64)
                .collect::<Vec<_>>()[..]
        );
    }
}
