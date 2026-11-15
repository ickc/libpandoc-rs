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
//! or a conda environment).
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

/// Inside a wasm filter: calls to the host.
#[cfg(target_family = "wasm")]
#[path = "guest.rs"]
mod backend;

/// An error from pandoc: its constructor (`PandocParseError`,
/// `PandocFilterError`, ... or `Exception`) and message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: String,
    pub message: String,
}

impl Error {
    fn new(kind: &str, message: impl Into<String>) -> Self {
        Error {
            kind: kind.into(),
            message: message.into(),
        }
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

/// Calls to libpandoc itself.
#[cfg(not(target_family = "wasm"))]
mod backend {
    use super::*;

    pub(crate) fn convert(options: &str, input: Option<&[u8]>) -> Result<Output> {
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
/// defaults-file keys (`{"from": "markdown", "to": "html"}`), `input` the
/// standard input (`None`: the options' `input-files`).
///
/// With `"sandbox": {"read": ["dir", ...], "write": ["dir", ...]}` (the
/// `wasm` feature), the conversion runs in libpandoc.wasm, which sees only
/// those directories (and the user data directory): everything pandoc
/// reads or writes must be in them (see the README's "Security").
pub fn convert(options: &Value, input: Option<&[u8]>) -> Result<Output> {
    #[cfg(not(target_family = "wasm"))]
    if let Some(dirs) = sandbox::from_options(options)? {
        return sandbox::convert(options, input, &dirs, None);
    }
    backend::convert(&options.to_string(), input)
}

/// Without the `wasm` feature, a directory sandbox is refused, never run
/// without.
#[cfg(all(not(feature = "wasm"), not(target_family = "wasm")))]
mod sandbox {
    use super::{Error, Filter, Output, Result, Value};

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
        _: Option<Vec<Filter<'_>>>,
    ) -> Result<Output> {
        match *dirs {}
    }

    pub(crate) fn convert_args<S>(_: &[S], _: Option<&[u8]>, dirs: &Dirs) -> Result<Output> {
        match *dirs {}
    }

    pub(crate) fn main<S>(_: &[S], _: Option<Vec<Filter<'_>>>, dirs: &Dirs) -> Result<i32> {
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

/// Run pandoc on `n` threads from now on (at least 1, at most the number
/// of processors); the new number.
#[cfg(not(target_family = "wasm"))]
pub fn set_num_threads(n: usize) -> usize {
    // libpandoc before 1.8 neither takes turns nor limits the number
    // (GHC's runtime can crash on either), so this does both for it.
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let processors = std::thread::available_parallelism().map_or(1, |p| p.get());
    let n = n.clamp(1, processors.min(c_int::MAX as usize)) as c_int;
    // SAFETY: one call at a time and at most one thread per processor,
    // which is all that libpandoc before 1.8 asks; 1.8 asks nothing.
    (unsafe { sys::pandoc_set_num_threads(n) }) as usize
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
    Ok(read_many_as(&[input], conversion)?.remove(0))
}

/// Parse many texts, each on its own, in parallel, with the reader set up
/// once: `options` are the defaults-file keys that affect reading
/// (`{"from": "commonmark_x"}`).
pub fn read_many<S: AsRef<str>>(inputs: &[S], options: &Value) -> Result<Vec<Pandoc>> {
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
    docs.into_iter()
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
        .collect()
}

/// [`read_many`], read as `conversion` reads its input.
pub fn read_many_as<S: AsRef<str>>(inputs: &[S], conversion: &Conversion) -> Result<Vec<Pandoc>> {
    read_many(inputs, &read_options(conversion))
}

/// The options to read a fragment as `conversion` reads its input: its
/// input format and the options that affect reading.
pub fn read_options(conversion: &Conversion) -> Value {
    const READ: [&str; 10] = [
        "abbreviations",
        "data-dir",
        "default-image-extension",
        "indented-code-classes",
        "preserve-tabs",
        "resource-path",
        "sandbox",
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
    let given = conversion.options.as_ref();
    let from = conversion
        .input_format
        .clone()
        .map(Value::from)
        .or_else(|| given.and_then(|o| o.get("from").or_else(|| o.get("reader")).cloned()))
        .unwrap_or_else(|| "markdown".into());
    opts.insert("from".into(), from);
    Value::Object(opts)
}

/// Render a document, as converting it from JSON would.
pub fn write(doc: &Pandoc, to: &str) -> Result<Output> {
    write_with(doc, &json!({ "to": to }))
}

/// [`write`] with more options (defaults-file keys).
pub fn write_with(doc: &Pandoc, options: &Value) -> Result<Output> {
    let mut opts = options.clone();
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
/// A filter in a conversion: one of pandoc's own (Lua, JSON filters,
/// citeproc), or one run in this process.
pub enum Filter<'a> {
    /// An entry of pandoc's "filters" option, as is: a path, `"citeproc"`,
    /// or `{"type": "lua", "path": ...}`.
    Pandoc(Value),
    /// A filter in this process.
    Callback(RawFilter<'a>),
    /// A wasm filter, run in this process: as a callback, seeing the
    /// directories of a directory sandbox if the conversion has one.
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

    /// A panir filter, as pandoc runs Lua filters.
    pub fn panir<F: panir::Filter + 'a>(mut f: F) -> Self {
        Filter::func(move |doc, conversion| {
            panir::apply_with(doc, &mut f, conversion)?;
            Ok(())
        })
    }

    /// A function changing the document.
    pub fn func<F>(mut f: F) -> Self
    where
        F: FnMut(&mut Pandoc, &Conversion) -> Result<(), BoxError> + 'a,
    {
        Filter::raw(move |json, conversion| {
            let mut doc = panir::from_str(std::str::from_utf8(json)?)?;
            f(&mut doc, conversion)?;
            Ok(panir::to_string(&doc).into_bytes())
        })
    }

    /// A function on pandoc's JSON.
    pub fn raw<F>(f: F) -> Self
    where
        F: FnMut(&[u8], &Conversion) -> Result<Vec<u8>, BoxError> + 'a,
    {
        Filter::Callback(Box::new(f))
    }
}

#[cfg(not(target_family = "wasm"))]
/// The state of one callback: the filter, the conversion's options, and a
/// panic caught in it, raised again after pandoc returns.
struct Slot<'a, 'b> {
    f: &'b mut RawFilter<'a>,
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
        (slot.f)(doc, &conversion)
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
/// The conversion libpandoc describes to a callback.
fn conversion(context: &[u8], options: Option<Value>) -> Conversion {
    let c: Value = serde_json::from_slice(context).unwrap_or_default();
    let s = |k: &str| c.get(k).and_then(Value::as_str).map(str::to_owned);
    Conversion {
        format: s("format"),
        input_format: s("input-format"),
        output_format: s("output-format"),
        reader_options: c.get("reader-options").cloned(),
        // (libpandoc's context has none yet: when it does, they are here)
        writer_options: c.get("writer-options").cloned(),
        options,
    }
}

#[cfg(not(target_family = "wasm"))]
/// Run `call` with the callbacks among `filters` as libpandoc's filter
/// array, and the "filters" entries naming them; raise a filter's panic.
fn with_filters<T>(
    filters: Vec<Filter<'_>>,
    options: Option<Value>,
    call: impl FnOnce(Vec<Value>, &[sys::pandoc_filter]) -> T,
) -> T {
    let mut entries = Vec::new();
    let mut fns = Vec::new();
    for f in filters {
        match f {
            Filter::Pandoc(v) => entries.push(v),
            Filter::Callback(f) => {
                entries.push(json!({"type": "callback", "index": fns.len()}));
                fns.push(f);
            }
            #[cfg(feature = "wasm")]
            Filter::Wasm(w) => {
                entries.push(json!({"type": "callback", "index": fns.len()}));
                fns.push(Box::new(move |doc, conversion| w.run(doc, conversion)));
            }
        }
    }
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
/// Rust and wasm filters run in this process (a wasm filter seeing the
/// sandbox's directories), Lua filters inside the sandbox, and JSON filters
/// (programs) not at all.
pub fn convert_with(
    options: &Value,
    input: Option<&[u8]>,
    filters: Vec<Filter<'_>>,
) -> Result<Output> {
    if let Some(dirs) = sandbox::from_options(options)? {
        return sandbox::convert(options, input, &dirs, Some(filters));
    }
    let mut user = options.clone();
    if let Value::Object(m) = &mut user {
        m.remove("filters");
    }
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
/// The pandoc command, in this process: what `pandoc ARGS` would do, with
/// `args[0]` the program's name. pandoc reads standard input, writes
/// standard output and error, and answers `--version`, `--help` and the
/// like itself; the result is its exit status.
///
/// With `filters`, they replace those pandoc found in the arguments and
/// defaults files ([`parse_args`] tells which those are).
///
/// With `LIBPANDOC_SANDBOX` or `LIBPANDOC_SANDBOX_WRITE` set (the `wasm`
/// feature), the conversion runs in the directory sandbox
/// ([`convert_args`]), and this prints its output and messages; `filters`
/// are refused there.
pub fn main<S: AsRef<str>>(args: &[S], filters: Option<Vec<Filter<'_>>>) -> Result<i32> {
    if let Some(dirs) = sandbox::from_env()? {
        return sandbox::main(args, filters, &dirs);
    }
    main_native(args, filters)
}

#[cfg(not(target_family = "wasm"))]
fn main_native<S: AsRef<str>>(args: &[S], filters: Option<Vec<Filter<'_>>>) -> Result<i32> {
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
