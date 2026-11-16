//! Conversions in a directory sandbox: the whole of pandoc runs inside
//! libpandoc.wasm (pandoc built for WebAssembly), under wasmtime, which
//! gives it the directories the caller lists and nothing else, as a
//! container's mounts do.
//!
//! The setting: `"sandbox": {"read": [dirs], "write": [dirs]}` in a
//! conversion's options ([`crate::convert`], [`crate::convert_with`]), with
//! if given `"timeout"` (seconds) and `"max-memory"` (bytes) for this
//! conversion alone, or
//! `LIBPANDOC_SANDBOX=dir:dir` and `LIBPANDOC_SANDBOX_WRITE=dir:dir` (`;`
//! on Windows) for the argument forms ([`crate::convert_args`],
//! [`crate::main`]). `"sandbox": true` keeps meaning pandoc's own sandbox
//! in the native library.
//!
//! What the module sees:
//!
//! - each listed directory at its own absolute path (on Windows `C:\x` is
//!   `/c/x`), read-only, or writable if in the write list;
//! - nothing else: not the user data directory, not `PANDOC_DATA_DIRS`,
//!   unless the caller lists them (pandoc inside then finds them where it
//!   looks);
//! - a private, writable temporary directory (libpandoc's temporary
//!   files), removed after;
//! - as its working directory, the process's, if that is inside a listed
//!   directory (else an empty one: relative names find nothing);
//! - no network (libpandoc.wasm is built without HTTP), no programs.
//!
//! The options and arguments go to pandoc untouched: what they name must
//! be inside the listed directories, as for the document. Lua filters run
//! inside; filters that are programs (JSON filters) fail, as a PDF engine
//! does; wasm filters and callbacks (given to [`crate::convert_with`] or
//! [`crate::main`]) run in this process, a wasm filter seeing the same
//! directories. [`crate::main`] is the module's pandoc command (its
//! `pandoc_main`), on this process's standard input, output and error. Limits:
//! `LIBPANDOC_WASM_TIMEOUT` and `LIBPANDOC_WASM_MAX_MEMORY` (or the
//! sandbox's `"timeout"` and `"max-memory"`), as for wasm filters (wasm32's
//! 4 GiB at most anyway), counted from the module's start, not its
//! compilation.
//!
//! The module: `$LIBPANDOC_WASM`, else the one libpandoc-sys found or
//! downloaded at build time. Compiled once per process, and cached on disk
//! by wasmtime's cache (the first compilation takes seconds to minutes).

use std::io::Write;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wasmtime::{Caller, InstancePre, Linker, Module, Store, Trap};
use wasmtime_wasi::p1::{self, WasiP1Ctx};
use wasmtime_wasi::{FsPerms, I32Exit, WasiCtxBuilder};

use crate::wasm::{
    Limits, MAX_MEMORY_VAR, TIMEOUT_VAR, WasmFilter, deadline, engine, parse_memory, parse_timeout,
    seconds, ticking,
};
use crate::{BoxError, Error, Filter, Output, RawFilter, Result};

/// The environment variables setting the sandbox for the argument forms.
pub(crate) const READ_VAR: &str = "LIBPANDOC_SANDBOX";
pub(crate) const WRITE_VAR: &str = "LIBPANDOC_SANDBOX_WRITE";
/// The environment variable naming libpandoc.wasm.
pub(crate) const WASM_VAR: &str = "LIBPANDOC_WASM";

/// The host's own directories in the module: the private temporary
/// directory, and an empty one, read-only, as the working directory when
/// the process's isn't mounted (pandoc's command needs one: it makes sure
/// `.` exists before writing to standard output).
const OWN: &str = "/.libpandoc";
const TMP: &str = "/.libpandoc/tmp";
const NOWHERE: &str = "/.libpandoc/nowhere";

fn refused(msg: impl Into<String>) -> Error {
    Error::new("PandocOptionError", msg)
}

/// A sandbox: the directories to read, those to write (and read), and its
/// own limits if it gives them (`Some(None)`: none), else the environment's.
#[derive(Debug, Default)]
pub(crate) struct Dirs {
    read: Vec<PathBuf>,
    write: Vec<PathBuf>,
    timeout: Option<Option<Duration>>,
    max_memory: Option<Option<usize>>,
}

impl Dirs {
    /// The limits: the sandbox's, else the environment's.
    fn limits(&self) -> Result<(Option<Duration>, Option<usize>)> {
        let var = |k| std::env::var(k).unwrap_or_default();
        Ok((
            match self.timeout {
                Some(t) => t,
                None => parse_timeout(&var(TIMEOUT_VAR)).map_err(refused)?,
            },
            match self.max_memory {
                Some(m) => m,
                None => parse_memory(&var(MAX_MEMORY_VAR)).map_err(refused)?,
            },
        ))
    }

    /// Where the time limit was given, for its error.
    fn timeout_given(&self) -> &'static str {
        match self.timeout {
            Some(_) => "the sandbox's \"timeout\"",
            None => TIMEOUT_VAR,
        }
    }

    /// Where the memory limit was given, for its error.
    fn max_memory_given(&self) -> &'static str {
        match self.max_memory {
            Some(_) => "the sandbox's \"max-memory\"",
            None => MAX_MEMORY_VAR,
        }
    }
}

/// The sandbox of `"sandbox": {"read": [...], "write": [...], "timeout": s,
/// "max-memory": bytes}`, if the options have one. The limits are a wasm
/// filter's: 0 or more (0: none), seconds a number, bytes an integer.
pub(crate) fn from_options(options: &Value) -> Result<Option<Dirs>> {
    let Some(Value::Object(s)) = options.get("sandbox") else {
        return Ok(None);
    };
    let mut dirs = Dirs::default();
    for (k, v) in s {
        let list = match k.as_str() {
            "read" => &mut dirs.read,
            "write" => &mut dirs.write,
            "timeout" => {
                let t = v.as_f64().filter(|t| *t >= 0.0).ok_or_else(|| {
                    refused(format!(
                        "sandbox: \"timeout\": seconds, 0 or more (0: no limit), not {v}"
                    ))
                })?;
                dirs.timeout = Some(seconds(t).filter(|_| t > 0.0));
                continue;
            }
            "max-memory" => {
                let n = v.as_u64().ok_or_else(|| {
                    refused(format!(
                        "sandbox: \"max-memory\": bytes, 0 or more (0: no limit), not {v}"
                    ))
                })?;
                // (more than a usize holds: none in practice)
                dirs.max_memory = Some((n > 0).then(|| usize::try_from(n).unwrap_or(usize::MAX)));
                continue;
            }
            _ => {
                return Err(refused(format!(
                    "sandbox: {k:?}: only \"read\", \"write\", \"timeout\" and \"max-memory\""
                )));
            }
        };
        *list = v
            .as_array()
            .and_then(|ds| ds.iter().map(|d| d.as_str().map(PathBuf::from)).collect())
            .ok_or_else(|| refused(format!("sandbox: {k:?} lists directories as strings")))?;
    }
    Ok(Some(dirs))
}

/// The directories of `LIBPANDOC_SANDBOX` and `LIBPANDOC_SANDBOX_WRITE`,
/// if either is set (set but empty: no directory; empty entries are
/// skipped).
pub(crate) fn from_env() -> Result<Option<Dirs>> {
    let var = |k| {
        std::env::var_os(k).map(|v| {
            std::env::split_paths(&v)
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        })
    };
    Ok(match (var(READ_VAR), var(WRITE_VAR)) {
        (None, None) => None,
        (r, w) => Some(Dirs {
            read: r.unwrap_or_default(),
            write: w.unwrap_or_default(),
            ..Dirs::default()
        }),
    })
}

// ---------------------------------------------------------------------------
// The mounts

/// An absolute host path as the module names it: `/` and its normal
/// components (`C:\x` is `/c/x`).
fn guest(p: &Path) -> Result<String> {
    let mut parts: Vec<String> = Vec::new();
    for c in p.components() {
        match c {
            Component::Prefix(pre) => match pre.kind() {
                Prefix::Disk(d) | Prefix::VerbatimDisk(d) => {
                    parts.push((d as char).to_ascii_lowercase().into())
                }
                _ => {
                    return Err(refused(format!(
                        "{}: not supported in a sandbox",
                        p.display()
                    )));
                }
            },
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(s) => parts.push(
                s.to_str()
                    .ok_or_else(|| refused(format!("{}: not UTF-8", p.display())))?
                    .to_owned(),
            ),
        }
    }
    Ok(format!("/{}", parts.join("/")))
}

/// Whether the module's path `p` is `dir` or inside it.
fn within(p: &str, dir: &str) -> bool {
    dir == "/" || p == dir || p.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

/// A private directory of this host's, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<Self> {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let dir = std::env::temp_dir().join(format!(
            "libpandoc-sandbox-{}-{n}-{nanos}",
            std::process::id()
        ));
        let mut b = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        b.create(&dir)
            .map_err(|e| Error::new("PandocIOError", format!("{}: {e}", dir.display())))?;
        let made = TempDir(dir);
        for d in ["tmp", "nowhere"] {
            b.create(made.0.join(d))
                .map_err(|e| Error::new("PandocIOError", format!("{}: {e}", made.0.display())))?;
        }
        Ok(made)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A directory the module sees: the host's, the module's name for it,
/// and whether it may write there.
struct Mount {
    host: PathBuf,
    guest: String,
    writable: bool,
}

/// What one conversion's module sees.
struct Mounts {
    tmp: TempDir,
    mounts: Vec<Mount>,
    env: Vec<(&'static str, String)>,
    /// the working directory, as the module names it
    cwd: String,
    /// whether it is inside a mount: Some(writable)
    cwd_mounted: Option<bool>,
}

impl Mounts {
    fn new(dirs: &Dirs) -> Result<Self> {
        let mut m = Mounts {
            tmp: TempDir::new()?,
            mounts: Vec::new(),
            env: vec![("TMPDIR", TMP.into())],
            cwd: String::new(),
            cwd_mounted: None,
        };
        // pandoc's own way to a reproducible docx, odt or epub (the time it
        // stamps); nothing else of the environment goes in
        if let Ok(v) = std::env::var("SOURCE_DATE_EPOCH") {
            m.env.push(("SOURCE_DATE_EPOCH", v));
        }
        for d in &dirs.read {
            m.add(d, false)?;
        }
        for d in &dirs.write {
            m.add(d, true)?;
        }
        // The user data directory and pandoc 3.14's PANDOC_DATA_DIRS are
        // not mounted by themselves: pandoc inside finds them only if the
        // caller lists them, and is told where they are only then.
        if let Some(d) = default_data_dir() {
            let d = std::path::absolute(&d).map_err(|e| io(&d, e))?;
            if m.mounted(&d)? {
                let var = match d.file_name() {
                    Some(n) if n == ".pandoc" => "HOME",
                    _ => "XDG_DATA_HOME",
                };
                m.env.push((var, guest(d.parent().unwrap_or(&d))?));
            }
        }
        if let Some(v) = std::env::var_os("PANDOC_DATA_DIRS") {
            // (by their names inside, `:`-separated, as pandoc in wasm reads it)
            let mut inside = Vec::new();
            for d in std::env::split_paths(&v).filter(|d| d.is_absolute() && d.is_dir()) {
                if m.mounted(&d)? {
                    inside.push(guest(&d)?);
                }
            }
            if !inside.is_empty() {
                m.env.push(("PANDOC_DATA_DIRS", inside.join(":")));
            }
        }
        let here = std::env::current_dir().map_err(|e| io(Path::new("."), e))?;
        m.cwd = guest(&here.canonicalize().unwrap_or(here))?;
        m.cwd_mounted = m
            .mounts
            .iter()
            .filter(|x| within(&m.cwd, &x.guest))
            .max_by_key(|x| x.guest.len())
            .map(|x| x.writable);
        Ok(m)
    }

    /// What a failure says when the working directory isn't mounted.
    fn note(&self) -> Option<String> {
        self.cwd_mounted.is_none().then(|| {
            format!(
                "(the working directory, {}, is not in the sandbox: relative names find nothing)",
                self.cwd
            )
        })
    }

    /// Whether `dir` is inside a directory the caller listed.
    fn mounted(&self, dir: &Path) -> Result<bool> {
        let real = guest(&dir.canonicalize().unwrap_or(dir.to_path_buf()))?;
        let named = guest(dir)?;
        Ok(self
            .mounts
            .iter()
            .any(|x| within(&real, &x.guest) || within(&named, &x.guest)))
    }

    /// A directory at its own absolute path (its real one, and as named if
    /// that differs, as macOS's `/tmp` for `/private/tmp`); writable if
    /// listed so anywhere.
    fn add(&mut self, dir: &Path, writable: bool) -> Result<()> {
        let real = dir
            .canonicalize()
            .map_err(|e| refused(format!("sandbox: {}: {e}", dir.display())))?;
        if !real.is_dir() {
            return Err(refused(format!(
                "sandbox: {}: not a directory",
                dir.display()
            )));
        }
        let named = std::path::absolute(dir).map_err(|e| io(dir, e))?;
        for g in [guest(&real)?, guest(&named)?] {
            if within(&g, OWN) {
                return Err(refused(format!("sandbox: {}: reserved", dir.display())));
            }
            match self.mounts.iter_mut().find(|x| x.guest == g) {
                Some(x) => x.writable |= writable,
                None => self.mounts.push(Mount {
                    host: real.clone(),
                    guest: g,
                    writable,
                }),
            }
        }
        Ok(())
    }
}

fn io(p: &Path, e: std::io::Error) -> Error {
    Error::new("PandocIOError", format!("{}: {e}", p.display()))
}

/// pandoc's default user data directory, if it exists: `$XDG_DATA_HOME/pandoc`
/// (`~/.local/share/pandoc`), else `~/.pandoc`; `%APPDATA%\pandoc` on
/// Windows.
fn default_data_dir() -> Option<PathBuf> {
    let var = |k| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if cfg!(windows) {
        return var("APPDATA")
            .map(|d| d.join("pandoc"))
            .filter(|d| d.is_dir());
    }
    let home = var("HOME");
    let xdg = var("XDG_DATA_HOME")
        .or_else(|| home.as_ref().map(|h| h.join(".local/share")))
        .map(|d| d.join("pandoc"));
    let legacy = home.map(|h| h.join(".pandoc"));
    xdg.filter(|d| d.is_dir()).or(legacy.filter(|d| d.is_dir()))
}

// ---------------------------------------------------------------------------
// Filters run here, on the calling thread

/// A filter of [`crate::convert_with`]'s, which the module calls back by
/// its index.
pub(crate) enum Hook<'a> {
    Callback(RawFilter<'a>),
    Wasm(WasmFilter),
}

impl Hook<'_> {
    /// Run the filter; `options` are the conversion's (none for a command
    /// line's), for a callback's conversion.
    fn run(
        &mut self,
        doc: &[u8],
        context: &[u8],
        m: &Mounts,
        options: Option<&Value>,
    ) -> Result<Vec<u8>, BoxError> {
        let c = crate::conversion(context, options.cloned());
        match self {
            Hook::Callback(f) => f(doc, &c),
            Hook::Wasm(w) => {
                // the same directories, and the working directory as `.`
                let mut w = w.clone().no_files();
                for x in &m.mounts {
                    w = w.preopen(&x.host, &x.guest, x.writable);
                }
                if let Some(writable) = m.cwd_mounted {
                    w = w.preopen(".", ".", writable);
                }
                w.run(doc, &c)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The module

/// The module, compiled once (and cached on disk by wasmtime).
fn instance_pre() -> Result<&'static InstancePre<Host>> {
    static PRE: OnceLock<Result<InstancePre<Host>, String>> = OnceLock::new();
    PRE.get_or_init(|| {
        let path = std::env::var_os(WASM_VAR)
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .or(option_env!("LIBPANDOC_WASM_BUILT").map(PathBuf::from))
            .ok_or_else(|| {
                format!(
                    "a sandbox runs pandoc in libpandoc.wasm, and there is none: \
                     set {WASM_VAR}, or build with libpandoc-sys's `download` feature"
                )
            })?;
        let module = Module::from_file(engine(), &path)
            .map_err(|e| format!("{}: not libpandoc.wasm: {e}", path.display()))?;
        let mut linker: Linker<Host> = Linker::new(engine());
        p1::add_to_linker_sync(&mut linker, |h| &mut h.wasi).map_err(|e| e.to_string())?;
        linker
            .func_wrap("libpandoc", "filter", filter_import)
            .map_err(|e| e.to_string())?;
        linker
            .instantiate_pre(&module)
            .map_err(|e| format!("{}: {e}", path.display()))
    })
    .as_ref()
    .map_err(|e| Error::new("PandocAppError", e.clone()))
}

/// The module's instance: its WASI context, limits, and the way to the
/// filters on the calling thread.
pub(crate) struct Host {
    wasi: WasiP1Ctx,
    limits: Limits,
    hooks: mpsc::Sender<Request>,
}

/// A filter's call: its index, the document and context, and where to
/// answer (status, bytes).
struct Request {
    index: u32,
    doc: Vec<u8>,
    context: Vec<u8>,
    answer: mpsc::Sender<(i32, Vec<u8>)>,
}

type Ctx<'a> = Caller<'a, Host>;

/// `libpandoc.filter(userdata, doc, doc_len, context, context_len, out)`:
/// the filter numbered `userdata`, run on the calling thread; its answer
/// is given with `pandoc_buffer_set`.
fn filter_import(
    mut c: Ctx,
    index: u32,
    doc: u32,
    doc_len: u32,
    ctx: u32,
    ctx_len: u32,
    out: u32,
) -> wasmtime::Result<i32> {
    let mem = c
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| wasmtime::Error::msg("libpandoc.wasm exports no memory"))?;
    let read = |c: &Ctx, p: u32, n: u32| -> wasmtime::Result<Vec<u8>> {
        let (p, n) = (p as usize, n as usize);
        mem.data(c)
            .get(p..p + n)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| wasmtime::Error::msg("a filter's arguments out of memory"))
    };
    let (doc, context) = (read(&c, doc, doc_len)?, read(&c, ctx, ctx_len)?);
    let (tx, rx) = mpsc::channel();
    let request = Request {
        index,
        doc,
        context,
        answer: tx,
    };
    let (status, answer) = match c.data().hooks.send(request) {
        Ok(()) => rx
            .recv()
            .unwrap_or((1, b"the filter's thread is gone".to_vec())),
        Err(_) => (1, b"no filters".to_vec()),
    };
    let p = func::<u32, u32>(&mut c, "malloc")?.call(&mut c, answer.len().max(1) as u32)?;
    if p == 0 {
        return Err(wasmtime::Error::msg("libpandoc.wasm is out of memory"));
    }
    mem.write(&mut c, p as usize, &answer)?;
    let set = func::<(u32, u32, u32), ()>(&mut c, "pandoc_buffer_set")?;
    set.call(&mut c, (out, p, answer.len() as u32))?;
    func::<u32, ()>(&mut c, "free")?.call(&mut c, p)?;
    Ok(status)
}

fn func<P: wasmtime::WasmParams, R: wasmtime::WasmResults>(
    c: &mut Ctx,
    name: &str,
) -> wasmtime::Result<wasmtime::TypedFunc<P, R>> {
    c.get_export(name)
        .and_then(|e| e.into_func())
        .ok_or_else(|| wasmtime::Error::msg(format!("libpandoc.wasm exports no {name}")))?
        .typed(&*c)
}

/// What to call: `pandoc_convert_filters` (options), `pandoc_convert_args`
/// (arguments), or `pandoc_main` (the command: its arguments, the program's
/// name first, and its filters as `filters_json`).
enum Call {
    Options(String),
    Args(Vec<String>),
    Main(Vec<String>, Option<String>),
}

/// A call's result as the module gives it, the log as its bytes (for
/// `pandoc_main`, its status only).
#[derive(Default)]
struct Raw {
    status: i32,
    output: Vec<u8>,
    kind: Option<String>,
    message: Option<String>,
    log: Vec<u8>,
}

/// Run the call in a fresh instance of the module, on a thread of its own
/// (the filters run on this one, where they live, given `options`).
fn run(
    m: &Mounts,
    dirs: &Dirs,
    call: Call,
    input: Option<&[u8]>,
    (hooks, options): (&mut [Hook], Option<&Value>),
) -> Result<Raw> {
    let (timeout, max_memory) = dirs.limits()?;
    let pre = instance_pre()?;
    let nhooks = hooks.len();
    let (tx, rx) = mpsc::channel::<Request>();
    let mut caught = None;
    let started = Instant::now();
    let raw = std::thread::scope(|s| {
        let module = std::thread::Builder::new()
            .name("libpandoc-wasm".into())
            .stack_size(16 << 20)
            .spawn_scoped(s, || {
                instance(m, pre, tx, (call, nhooks), input, timeout, max_memory)
            })
            .map_err(|e| Error::new("PandocAppError", e.to_string()))?;
        for r in rx {
            let result = match hooks.get_mut(r.index as usize) {
                Some(h) => {
                    panic::catch_unwind(AssertUnwindSafe(|| h.run(&r.doc, &r.context, m, options)))
                }
                None => Ok(Err("no such filter".into())),
            };
            let answer = match result {
                Ok(Ok(doc)) => (0, doc),
                Ok(Err(e)) => (1, e.to_string().into_bytes()),
                Err(p) => {
                    let msg = format!("the filter panicked: {}", crate::panic_message(&p));
                    caught.get_or_insert(p);
                    (1, msg.into_bytes())
                }
            };
            let _ = r.answer.send(answer);
        }
        module.join().unwrap_or_else(|p| panic::resume_unwind(p))
    });
    if let Some(p) = caught {
        panic::resume_unwind(p);
    }
    raw.map_err(|e| stopped(e, dirs, timeout, max_memory, started))?
}

/// Why the module stopped (a trap, or its runtime exited), as an error.
fn stopped(
    e: wasmtime::Error,
    dirs: &Dirs,
    timeout: Option<Duration>,
    max_memory: Option<usize>,
    started: Instant,
) -> Error {
    if let (Some(t), Some(Trap::Interrupt)) = (timeout, e.downcast_ref()) {
        return Error::new(
            Error::TIMEOUT,
            format!(
                "the conversion was stopped after {:.1} s, its time limit ({}: {})",
                started.elapsed().as_secs_f64(),
                dirs.timeout_given(),
                t.as_secs_f64()
            ),
        );
    }
    let given = dirs.max_memory_given();
    let message = if let Some(m) = max_memory.filter(|_| e.downcast_ref::<OutOfMemory>().is_some())
    {
        format!("the conversion ran out of memory, its limit being {m} bytes ({given})")
    } else if let Some(I32Exit(n)) = e.downcast_ref() {
        let limit = max_memory.map_or("wasm's 4 GiB".into(), |m| format!("{m} bytes, {given}"));
        format!(
            "libpandoc.wasm stopped: its runtime exited with status {n}, most likely out of \
             memory (its limit: {limit})"
        )
    } else {
        format!("libpandoc.wasm stopped: {e:#}")
    };
    Error::new("Exception", message)
}

/// The memory limit was reached (a marker in the error of the call).
#[derive(Debug)]
struct OutOfMemory;

impl std::fmt::Display for OutOfMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the memory limit was reached")
    }
}

impl std::error::Error for OutOfMemory {}

/// One instance of the module, seeing the mounts, running `call`, on this
/// process's standard input (unless `input` is given), output and error.
fn instance(
    m: &Mounts,
    pre: &InstancePre<Host>,
    hooks: mpsc::Sender<Request>,
    call: (Call, usize),
    input: Option<&[u8]>,
    timeout: Option<Duration>,
    max_memory: Option<usize>,
) -> wasmtime::Result<Result<Raw>> {
    let mut wasi = WasiCtxBuilder::new();
    wasi.arg("libpandoc")
        .inherit_stdout()
        .inherit_stderr()
        .allow_blocking_current_thread(true);
    for (k, v) in &m.env {
        wasi.env(k, v);
    }
    if input.is_none() {
        // pandoc reads standard input when it has no input file, as the
        // command does
        wasi.inherit_stdin();
    }
    wasi.preopened_dir(m.tmp.0.join("tmp"), TMP, FsPerms::ReadWrite)?;
    wasi.preopened_dir(m.tmp.0.join("nowhere"), NOWHERE, FsPerms::ReadOnly)?;
    for x in &m.mounts {
        let perms = match x.writable {
            true => FsPerms::ReadWrite,
            false => FsPerms::ReadOnly,
        };
        wasi.preopened_dir(&x.host, &x.guest, perms)?;
    }
    let host = Host {
        wasi: wasi.build_p1(),
        limits: Limits {
            max_memory,
            refused: false,
        },
        hooks,
    };
    let mut store = Store::new(engine(), host);
    store.limiter(|h| &mut h.limits);
    if timeout.is_some() {
        ticking();
    }
    store.set_epoch_deadline(deadline(timeout));
    let cwd = m.cwd_mounted.map_or(NOWHERE, |_| m.cwd.as_str());
    let r = call_in(&mut store, pre, call, input, cwd);
    match r {
        Err(e) if store.data().limits.refused => Err(e.context(OutOfMemory)),
        r => r,
    }
}

/// `call`, with this many filters of this process's.
fn call_in(
    store: &mut Store<Host>,
    pre: &InstancePre<Host>,
    (call, nhooks): (Call, usize),
    input: Option<&[u8]>,
    cwd: &str,
) -> wasmtime::Result<Result<Raw>> {
    let instance = pre.instantiate(&mut *store)?;
    let f = |store: &mut Store<Host>, name: &str| {
        instance
            .get_func(&mut *store, name)
            .ok_or_else(|| wasmtime::Error::msg(format!("libpandoc.wasm exports no {name}")))
    };
    f(store, "_initialize")?
        .typed::<(), ()>(&*store)?
        .call(&mut *store, ())?;
    let abi = f(store, "pandoc_abi_version")?
        .typed::<(), i32>(&*store)?
        .call(&mut *store, ())?;
    if let Err(e) = crate::compatible("libpandoc.wasm", abi) {
        return Ok(Err(e));
    }
    if f(store, "pandoc_init")?
        .typed::<(), i32>(&*store)?
        .call(&mut *store, ())?
        != 0
    {
        return Err(wasmtime::Error::msg("libpandoc.wasm: pandoc didn't start"));
    }
    let malloc = f(store, "malloc")?.typed::<u32, u32>(&*store)?;
    let mem = instance
        .get_memory(&mut *store, "memory")
        .ok_or_else(|| wasmtime::Error::msg("libpandoc.wasm exports no memory"))?;
    let put = |store: &mut Store<Host>, b: &[u8]| -> wasmtime::Result<u32> {
        let p = malloc.call(&mut *store, b.len().max(1) as u32)?;
        if p == 0 {
            return Err(wasmtime::Error::msg("libpandoc.wasm is out of memory"));
        }
        mem.write(&mut *store, p as usize, b)?;
        Ok(p)
    };
    let cstr = |s: &str| -> wasmtime::Result<Vec<u8>> {
        if s.contains('\0') {
            return Err(wasmtime::Error::msg("an argument contains NUL"));
        }
        Ok([s.as_bytes(), b"\0"].concat())
    };
    let argv = |store: &mut Store<Host>, args: &[String]| -> wasmtime::Result<u32> {
        let mut argv = Vec::new();
        for a in args {
            argv.extend_from_slice(&put(store, &cstr(a)?)?.to_le_bytes());
        }
        put(store, &argv)
    };
    // the module's callback filters are all the host's: pandoc_filter
    // {fn = NULL, userdata = i} (8 bytes on wasm32)
    let filters = |store: &mut Store<Host>| -> wasmtime::Result<u32> {
        let mut filters = Vec::new();
        for i in 0..nhooks as u32 {
            filters.extend_from_slice(&0u32.to_le_bytes());
            filters.extend_from_slice(&i.to_le_bytes());
        }
        put(store, &filters)
    };
    let p = put(store, &cstr(cwd)?)?;
    f(store, "chdir")?
        .typed::<u32, i32>(&*store)?
        .call(&mut *store, p)?;
    let (ip, il) = match input {
        Some(b) => (put(store, b)?, b.len() as u32),
        None => (0, 0),
    };
    type Six = (u32, u32, u32, u32, u32, u32);
    let r = match call {
        Call::Options(o) => {
            let fp = filters(store)?;
            let op = put(store, o.as_bytes())?;
            f(store, "pandoc_convert_filters")?
                .typed::<Six, u32>(&*store)?
                .call(&mut *store, (op, o.len() as u32, ip, il, fp, nhooks as u32))?
        }
        Call::Args(args) => {
            let ap = argv(store, &args)?;
            f(store, "pandoc_convert_args")?
                .typed::<(u32, u32, u32, u32), u32>(&*store)?
                .call(&mut *store, (args.len() as u32, ap, ip, il))?
        }
        Call::Main(args, filters_json) => {
            let ap = argv(store, &args)?;
            let (jp, jl, fp, fl) = match filters_json {
                Some(j) => (
                    put(store, j.as_bytes())?,
                    j.len() as u32,
                    filters(store)?,
                    nhooks,
                ),
                None => (0, 0, 0, 0),
            };
            let status = f(store, "pandoc_main")?
                .typed::<Six, i32>(&*store)?
                .call(&mut *store, (args.len() as u32, ap, jp, jl, fp, fl as u32))?;
            return Ok(Ok(Raw {
                status,
                ..Raw::default()
            }));
        }
    };
    if r == 0 {
        return Err(wasmtime::Error::msg("libpandoc.wasm returned no result"));
    }
    // pandoc_result on wasm32: status, output, output_len, error_kind,
    // error_message, log; 4 bytes each
    let data = mem.data(&*store);
    let word = |i: usize| -> wasmtime::Result<u32> {
        let at = r as usize + 4 * i;
        let b = data
            .get(at..at + 4)
            .ok_or_else(|| wasmtime::Error::msg("libpandoc.wasm's result out of memory"))?;
        Ok(u32::from_le_bytes(b.try_into().expect("4 bytes")))
    };
    let cstring = |p: u32| -> Option<Vec<u8>> {
        let rest = data.get(p as usize..).filter(|_| p != 0)?;
        Some(rest[..rest.iter().position(|&b| b == 0)?].to_vec())
    };
    let text = |b: Option<Vec<u8>>| b.map(|b| String::from_utf8_lossy(&b).into_owned());
    let (out, out_len) = (word(1)? as usize, word(2)? as usize);
    Ok(Ok(Raw {
        status: word(0)? as i32,
        output: data.get(out..out + out_len).unwrap_or_default().to_vec(),
        kind: text(cstring(word(3)?)),
        message: text(cstring(word(4)?)),
        log: cstring(word(5)?).unwrap_or_else(|| b"[]".to_vec()),
    }))
}

// ---------------------------------------------------------------------------
// The entry points

/// The "filters" entries for `filters`, those of this process as callbacks
/// (hooks, which the module calls by their index).
fn hooks<'a>(filters: Vec<Filter<'a>>) -> (Vec<Value>, Vec<Hook<'a>>) {
    let mut entries = Vec::new();
    let mut hooks = Vec::new();
    for f in filters {
        let hook = match f {
            Filter::Pandoc(v) => {
                entries.push(v);
                continue;
            }
            Filter::Callback(f) => Hook::Callback(f),
            Filter::Wasm(w) => Hook::Wasm(w),
        };
        entries.push(json!({"type": "callback", "index": hooks.len()}));
        hooks.push(hook);
    }
    (entries, hooks)
}

/// A conversion's result, or its error, which says so when the working
/// directory isn't in the sandbox.
fn result(raw: Raw, m: &Mounts) -> Result<Output> {
    if raw.status != 0 {
        let kind = raw.kind.unwrap_or_else(|| "Exception".into());
        let mut message = raw.message.unwrap_or_default();
        if let Some(note) = m.note() {
            message = format!("{message}\n{note}");
        }
        return Err(Error { kind, message });
    }
    Ok(Output {
        bytes: raw.output,
        log: serde_json::from_slice(&raw.log).unwrap_or_default(),
    })
}

/// [`crate::convert`] and [`crate::convert_with`] in the sandbox: the
/// options as they are, but for the sandbox; `filters` (if given) replace
/// the options' "filters", those of this process as callbacks.
pub(crate) fn convert(
    options: &Value,
    input: Option<&[u8]>,
    dirs: &Dirs,
    filters: Option<Vec<Filter<'_>>>,
) -> Result<Output> {
    let mut o = options.clone();
    let Value::Object(map) = &mut o else {
        return Err(refused("options: an object"));
    };
    map.remove("sandbox");
    let mut hooks = Vec::new();
    if let Some(filters) = filters {
        let (entries, h) = self::hooks(filters);
        map.insert("filters".into(), entries.into());
        hooks = h;
    }
    // what the callbacks are told: the options, the sandbox included
    let user = crate::user_options(options);
    let m = Mounts::new(dirs)?;
    result(
        run(
            &m,
            dirs,
            Call::Options(o.to_string()),
            input,
            (&mut hooks, Some(&user)),
        )?,
        &m,
    )
}

/// [`crate::convert_args`] in the sandbox: the arguments as they are.
pub(crate) fn convert_args<S: AsRef<str>>(
    args: &[S],
    input: Option<&[u8]>,
    dirs: &Dirs,
) -> Result<Output> {
    let args = args.iter().map(|a| a.as_ref().to_owned()).collect();
    let m = Mounts::new(dirs)?;
    result(run(&m, dirs, Call::Args(args), input, (&mut [], None))?, &m)
}

/// [`crate::main`] in the sandbox: the module's pandoc command, on this
/// process's standard input, output and error; its exit status. `filters`
/// (if given) replace those pandoc found, those of this process as
/// callbacks.
pub(crate) fn main<S: AsRef<str>>(
    args: &[S],
    filters: Option<Vec<Filter<'_>>>,
    dirs: &Dirs,
) -> Result<i32> {
    let args = args.iter().map(|a| a.as_ref().to_owned()).collect();
    let (json, mut hooks) = match filters {
        Some(f) => {
            let (entries, hooks) = self::hooks(f);
            (Some(Value::from(entries).to_string()), hooks)
        }
        None => (None, Vec::new()),
    };
    let m = Mounts::new(dirs)?;
    let status = run(&m, dirs, Call::Main(args, json), None, (&mut hooks, None))?.status;
    std::io::stdout()
        .flush()
        .map_err(|e| Error::new("PandocIOError", e.to_string()))?;
    if let Some(note) = m.note().filter(|_| status != 0) {
        eprintln!("{note}");
    }
    Ok(status)
}
