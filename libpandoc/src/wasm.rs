//! Filters compiled to WebAssembly, run in process by wasmtime.
//!
//! A wasm filter is a pandoc JSON filter built for WASI (`wasm32-wasip1`):
//! a command that reads the document as pandoc's JSON on standard input and
//! writes the new one to standard output, told the output format as its
//! first argument and the rest in the environment, as pandoc tells a JSON
//! filter. A panir filter (`panir::filter`) is one as it is:
//!
//! ```sh
//! cargo build --release --target wasm32-wasip1 --example upper
//! pandocrs -F upper.wasm input.md
//! ```
//!
//! One file runs on every platform, and in the browser beside
//! libpandoc.wasm. It runs sandboxed: no network, the environment variables
//! pandoc gives filters, and only the directories it is given: by default
//! none, as a wasm filter is for code you don't trust. `LIBPANDOC_WASM_DIRS`
//! gives every filter some, read-only: directories as `PATH` lists them
//! (`:`, `;` on Windows; unset or empty: none). A program gives one filter
//! its own instead ([`WasmFilter::dirs`], [`WasmFilter::preopen`],
//! [`WasmFilter::no_files`]), whatever the variable says.
//!
//! The filter sees each such directory under its absolute path (`C:\x` as
//! `/c/x` on Windows), its real one too if a link leads to it, and its path
//! relative to the working directory (`.`, `..`, `../common`, `img`; none
//! on another drive), and the working directory as `.` when it lies inside
//! one: so `../common/part.txt`, `here.txt` and absolute names work as for
//! a native filter. wasi-libc matches the beginning of a path against those
//! names as written, without normalising it: `./../common/x` and
//! `img/../../common/x` don't work, nor `./img/x` when only `img` is given,
//! nor a symbolic link leaving a directory given.
//!
//! Limits, which pandoc has for no filter: a timeout (wall-clock time,
//! its calls to pandoc included: each runs with the time the filter has
//! left as its `"timeout"`) and a memory limit (the filter's own memory,
//! at most 4 GiB anyway). None by default, as pandoc; set for every
//! filter by `LIBPANDOC_WASM_TIMEOUT` (seconds, as pandoc-server's
//! `--timeout`) and `LIBPANDOC_WASM_MAX_MEMORY` (bytes, or with `k`, `m`
//! or `g`, as pandoc's `+RTS -M`), or for one by [`WasmFilter::timeout`]
//! and [`WasmFilter::max_memory`]. A filter past either fails.
//!
//! Of the environment it gets libpandoc's variables only, from its
//! conversion: `PANDOC_SANDBOX` (`true` in the directory sandbox) among
//! them, none of this process's.
//!
//! A filter may call pandoc: built with this crate, `libpandoc::read` and
//! the like call this process's pandoc (the imports of `guest.rs`). Those
//! calls are marked `"untrusted": true`, so libpandoc runs
//! them in pandoc's sandbox and refuses options that would read or write
//! files, fetch resources or run programs: the filter gets no more access
//! through pandoc than it has itself. The list is libpandoc's, the same
//! for every host.
//!
//! Compiled code is cached (wasmtime's cache, e.g. `~/.cache/wasmtime`), so
//! only a filter's first run pays for compiling it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::Value;
use wasmtime::{Caller, Config, Engine, Linker, Module, ResourceLimiter, Store};
use wasmtime_wasi::p1::{self, WasiP1Ctx};
use wasmtime_wasi::p2::pipe::{MemoryInputPipe, MemoryOutputPipe};
use wasmtime_wasi::{FsPerms, I32Exit, WasiCtxBuilder};

use crate::{BoxError, Conversion, Error, Filter, Output, Result};

/// The engine all wasm filters (and libpandoc.wasm, in a sandbox) share,
/// with wasmtime's cache when it can have one.
pub(crate) fn engine() -> &'static Engine {
    ENGINE.get_or_init(|| {
        let mut config = Config::new();
        config.epoch_interruption(true);
        // libpandoc.wasm's Lua is built with wasm's exception handling
        config.wasm_exceptions(true);
        if let Ok(cache) = wasmtime::Cache::new(wasmtime::CacheConfig::new()) {
            config.cache(Some(cache));
        }
        Engine::new(&config).expect("wasmtime's default configuration")
    })
}

static ENGINE: OnceLock<Engine> = OnceLock::new();

/// Advance the epoch, if wasm ran in this process: a sandboxed conversion
/// with a [`crate::CancelToken`] then looks at it at once (every other
/// store's time limit comes one tick, 10 ms, nearer).
pub(crate) fn interrupt() {
    if let Some(engine) = ENGINE.get() {
        engine.increment_epoch();
    }
}

/// The environment variables setting every filter's limits.
pub const TIMEOUT_VAR: &str = "LIBPANDOC_WASM_TIMEOUT";
pub const MAX_MEMORY_VAR: &str = "LIBPANDOC_WASM_MAX_MEMORY";
/// The environment variable giving every filter directories, read-only.
pub const DIRS_VAR: &str = "LIBPANDOC_WASM_DIRS";

/// How often the engine's epoch advances, once a filter has a timeout.
pub(crate) const TICK: Duration = Duration::from_millis(10);

/// The epoch's clock: a thread advancing it once per [`TICK`] of real time,
/// started by the first filter with a timeout. It counts time, not sleeps:
/// a sleep of 10 ms can last several times that (macOS coalesces timers),
/// which made a 2 s limit last 8 s on a CI runner.
pub(crate) fn ticking() {
    static CLOCK: OnceLock<()> = OnceLock::new();
    CLOCK.get_or_init(|| {
        std::thread::Builder::new()
            .name("wasm-filter-clock".into())
            .spawn(|| {
                let start = Instant::now();
                let mut epoch = 0;
                loop {
                    std::thread::sleep(TICK);
                    let now = start.elapsed().as_nanos() / TICK.as_nanos();
                    while epoch < now {
                        engine().increment_epoch();
                        epoch += 1;
                    }
                }
            })
            .expect("a thread for wasm filters' timeouts");
    });
}

/// A timeout in seconds (`LIBPANDOC_WASM_TIMEOUT`): none if empty or 0.
pub fn parse_timeout(s: &str) -> Result<Option<Duration>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    match s.parse::<f64>() {
        Ok(0.0) => Ok(None),
        Ok(t) if t > 0.0 && t.is_finite() => Ok(seconds(t)),
        _ => Err(format!("{TIMEOUT_VAR}: seconds, not {s:?}")),
    }
}

/// A time limit of `t` seconds (more than 0): none if too long for a
/// [`Duration`] (none in practice; `from_secs_f64` panicked on 1e300).
pub(crate) fn seconds(t: f64) -> Option<Duration> {
    Duration::try_from_secs_f64(t).ok()
}

/// The epoch deadline for a time limit, in ticks (one more, for the tick
/// under way); never past what wasmtime counts to.
pub(crate) fn deadline(t: Option<Duration>) -> u64 {
    t.map_or(u64::MAX / 2, |t| {
        (t.div_duration_f64(TICK).ceil() as u64)
            .saturating_add(1)
            .min(u64::MAX / 2)
    })
}

/// A size in bytes, or with `k`, `m` or `g` (`LIBPANDOC_WASM_MAX_MEMORY`,
/// as pandoc's `+RTS -M`): none if empty or 0.
pub fn parse_memory(s: &str) -> Result<Option<usize>, String> {
    let s = s.trim();
    let (digits, unit) = match s.char_indices().last() {
        Some((i, c)) if c.is_ascii_alphabetic() => (&s[..i], c.to_ascii_lowercase()),
        _ => (s, 'b'),
    };
    let shift = match unit {
        'b' => 0,
        'k' => 10,
        'm' => 20,
        'g' => 30,
        _ => {
            return Err(format!(
                "{MAX_MEMORY_VAR}: bytes, or with k, m or g, not {s:?}"
            ));
        }
    };
    if s.is_empty() {
        return Ok(None);
    }
    digits
        .parse::<usize>()
        .ok()
        .and_then(|n| n.checked_mul(1 << shift))
        .map(|n| (n > 0).then_some(n))
        .ok_or_else(|| format!("{MAX_MEMORY_VAR}: bytes, or with k, m or g, not {s:?}"))
}

/// The limits the environment sets.
pub(crate) fn env_limits() -> Result<(Option<Duration>, Option<usize>), String> {
    let var = |k| std::env::var(k).unwrap_or_default();
    Ok((
        parse_timeout(&var(TIMEOUT_VAR))?,
        parse_memory(&var(MAX_MEMORY_VAR))?,
    ))
}

/// The directories `LIBPANDOC_WASM_DIRS` gives (empty entries skipped).
fn env_dirs() -> Vec<Dir> {
    std::env::var_os(DIRS_VAR)
        .map(|v| {
            std::env::split_paths(&v)
                .filter(|p| !p.as_os_str().is_empty())
                .map(Dir::Host)
                .collect()
        })
        .unwrap_or_default()
}

/// pandoc's version, for PANDOC_VERSION: asked for when a filter is
/// loaded, not while it runs: the first call into pandoc from inside a
/// conversion starts another of pandoc's threads, which takes tens of ms.
fn pandoc_version() -> Option<&'static str> {
    static VERSION: OnceLock<Option<String>> = OnceLock::new();
    VERSION
        .get_or_init(|| crate::pandoc_version().ok())
        .as_deref()
}

/// A directory a filter may see: the host's path, the filter's, and
/// whether it may write.
#[derive(Clone, Debug)]
struct Preopen {
    host: PathBuf,
    guest: String,
    writable: bool,
}

/// A directory a filter is given: one of the host's, seen by the names
/// [`seen`] gives it, or one exactly as the program names it.
#[derive(Clone, Debug)]
enum Dir {
    Host(PathBuf),
    Exact(Preopen),
}

/// The names a filter sees the host's directory `dir` by, read-only: its
/// absolute path (as the directory sandbox names it), and its real one if
/// a link leads to it; its path relative to the working directory, from
/// their real paths; and the working directory as `.` if it lies inside.
/// An error, naming it, if it doesn't exist or isn't a directory.
fn seen(dir: &Path) -> Result<Vec<Preopen>, String> {
    use std::io::ErrorKind::{NotADirectory, NotFound};
    match std::fs::metadata(dir) {
        Err(e) if matches!(e.kind(), NotFound | NotADirectory) => {
            return Err(format!("{} does not exist", dir.display()));
        }
        Err(e) => return Err(format!("{}: {e}", dir.display())),
        Ok(m) if !m.is_dir() => return Err(format!("{} is not a directory", dir.display())),
        Ok(_) => {}
    }
    let fail = |e: std::io::Error| format!("{}: {e}", dir.display());
    let real = dir.canonicalize().map_err(fail)?;
    let cwd = std::env::current_dir()
        .and_then(|d| d.canonicalize())
        .map_err(fail)?;
    let read_only = |host: &Path, guest: String| Preopen {
        host: host.to_owned(),
        guest,
        writable: false,
    };
    // (a name that can't be written as WASI's, as a UNC path: left out)
    let absolute = std::path::absolute(dir).map_err(fail)?;
    let mut names: Vec<Preopen> = [&absolute, &real]
        .into_iter()
        .filter_map(|p| crate::sandbox::guest(p).ok())
        .map(|g| read_only(&real, g))
        .collect();
    names.extend(relative(&cwd, &real).map(|r| read_only(&real, r)));
    if real != cwd && cwd.starts_with(&real) {
        names.push(read_only(&cwd, ".".into()));
    }
    Ok(names)
}

/// The path from the directory `from` to `to`, both real paths, with `/`:
/// `.`, `..`, `../common`, `img`; none on another drive, or not UTF-8.
fn relative(from: &Path, to: &Path) -> Option<String> {
    let (from, to): (Vec<_>, Vec<_>) = (from.components().collect(), to.components().collect());
    if from.first() != to.first() {
        return None;
    }
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts = vec![".."; from.len() - common];
    for c in &to[common..] {
        parts.push(c.as_os_str().to_str()?);
    }
    Some(match parts.is_empty() {
        true => ".".into(),
        false => parts.join("/"),
    })
}

/// A compiled wasm filter.
#[derive(Clone)]
pub struct WasmFilter {
    name: String,
    module: Module,
    dirs: Vec<Dir>,
    /// whether the program said which directories (else the variable's)
    named: bool,
    timeout: Option<Duration>,
    max_memory: Option<usize>,
}

impl WasmFilter {
    /// Compile the filter at `path` (or take it from the cache).
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, BoxError> {
        let path = path.as_ref();
        let module = Module::from_file(engine(), path)
            .map_err(|e| format!("{}: not a wasm filter: {e}", path.display()))?;
        Self::new(path.display().to_string(), module)
    }

    /// Compile a filter from its bytes; `name` is its first argument.
    pub fn from_bytes(name: &str, wasm: &[u8]) -> Result<Self, BoxError> {
        let module =
            Module::new(engine(), wasm).map_err(|e| format!("{name}: not a wasm filter: {e}"))?;
        Self::new(name.to_owned(), module)
    }

    fn new(name: String, module: Module) -> Result<Self, BoxError> {
        pandoc_version();
        let (timeout, max_memory) = env_limits()?;
        Ok(WasmFilter {
            name,
            module,
            dirs: env_dirs(),
            named: false,
            timeout,
            max_memory,
        })
    }

    /// Stop the filter after this long (`None`: never), instead of what
    /// `LIBPANDOC_WASM_TIMEOUT` says.
    pub fn timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    /// Let the filter's memory grow to this many bytes at most (`None`: as
    /// far as wasm allows, 4 GiB), instead of what
    /// `LIBPANDOC_WASM_MAX_MEMORY` says.
    pub fn max_memory(mut self, bytes: Option<usize>) -> Self {
        self.max_memory = bytes;
        self
    }

    /// Let the filter see these directories of this machine, read-only,
    /// as `LIBPANDOC_WASM_DIRS` gives them (the module's documentation:
    /// under their absolute paths and their paths relative to the working
    /// directory), instead of what the variable says; with those given by
    /// the other calls below. They are looked at when the filter runs: one
    /// that doesn't exist, or isn't a directory, fails it then.
    pub fn dirs<P: Into<PathBuf>>(mut self, dirs: impl IntoIterator<Item = P>) -> Self {
        self.own();
        self.dirs
            .extend(dirs.into_iter().map(|d| Dir::Host(d.into())));
        self
    }

    /// Let the filter see `host` as `guest` exactly (read-only unless
    /// `writable`), instead of what `LIBPANDOC_WASM_DIRS` says; with those
    /// given by the other calls.
    pub fn preopen(mut self, host: impl Into<PathBuf>, guest: &str, writable: bool) -> Self {
        self.own();
        self.dirs.push(Dir::Exact(Preopen {
            host: host.into(),
            guest: guest.into(),
            writable,
        }));
        self
    }

    /// See no directory at all, whatever `LIBPANDOC_WASM_DIRS` says (and
    /// none of those given before).
    pub fn no_files(mut self) -> Self {
        self.own();
        self.dirs.clear();
        self
    }

    /// The program says which directories: the variable's are dropped.
    fn own(&mut self) {
        if !self.named {
            self.dirs.clear();
            self.named = true;
        }
    }

    /// What the filter sees: the program's names as given, the host's
    /// directories by theirs (the first of a name kept).
    fn preopens(&self) -> Result<Vec<Preopen>, String> {
        let mut preopens: Vec<Preopen> = Vec::new();
        for d in &self.dirs {
            let names = match d {
                Dir::Exact(p) => vec![p.clone()],
                Dir::Host(h) => seen(h).map_err(|e| match self.named {
                    true => format!("{}: {e}", self.name),
                    false => format!("{}: {DIRS_VAR}: {e}", self.name),
                })?,
            };
            for p in names {
                if !preopens.iter().any(|x| x.guest == p.guest) {
                    preopens.push(p);
                }
            }
        }
        Ok(preopens)
    }

    /// Run the filter on a document (pandoc's JSON).
    pub fn run(&self, doc: &[u8], conversion: &Conversion) -> Result<Vec<u8>, BoxError> {
        let preopens = self.preopens()?;
        let stdout = MemoryOutputPipe::new(usize::MAX);
        let mut wasi = WasiCtxBuilder::new();
        wasi.stdin(MemoryInputPipe::new(doc.to_vec()))
            .stdout(stdout.clone())
            .inherit_stderr()
            .arg(&self.name)
            .arg(conversion.format.as_deref().unwrap_or(""));
        for (k, v) in env(conversion) {
            wasi.env(k, v);
        }
        for p in &preopens {
            let perms = if p.writable {
                FsPerms::ReadWrite
            } else {
                FsPerms::ReadOnly
            };
            wasi.preopened_dir(&p.host, &p.guest, perms)
                .map_err(|e| format!("{}: {e}", p.host.display()))?;
        }
        let started = Instant::now();
        let host = Host {
            wasi: wasi.build_p1(),
            result: None,
            limits: Limits {
                max_memory: self.max_memory,
                refused: false,
            },
            deadline: self.timeout.and_then(|t| started.checked_add(t)),
            out_of_time: false,
        };
        let mut store = Store::new(engine(), host);
        store.limiter(|h| &mut h.limits);
        if self.timeout.is_some() {
            ticking();
        }
        store.set_epoch_deadline(deadline(self.timeout));
        let mut linker: Linker<Host> = Linker::new(engine());
        p1::add_to_linker_sync(&mut linker, |h| &mut h.wasi)?;
        add_libpandoc(&mut linker)?;
        let instance = linker.instantiate(&mut store, &self.module)?;
        let start = instance.get_typed_func::<(), ()>(&mut store, "_start")?;
        let ran = start.call(&mut store, ());
        if store.data().out_of_time {
            // its time ran out in a call to pandoc: stopped, whatever it did
            return Err(self.out_of_time(started).into());
        }
        match ran {
            Ok(()) => {}
            Err(e) => match e.downcast_ref::<I32Exit>() {
                Some(I32Exit(0)) => {}
                Some(I32Exit(n)) => {
                    let msg = format!("{} exited with status {n}", self.name);
                    let msg = self.failed(msg, &e, &store, started, preopens.is_empty());
                    return Err(msg.into());
                }
                None => {
                    let msg = format!("{}: {e}", self.name);
                    let msg = self.failed(msg, &e, &store, started, preopens.is_empty());
                    return Err(msg.into());
                }
            },
        }
        drop(store);
        Ok(stdout
            .try_into_inner()
            .map(|b| b.to_vec())
            .unwrap_or_default())
    }

    /// What a trap means: the limits it was stopped by, if any; else
    /// that it was given no directory, if so (which may be why it failed).
    fn failed(
        &self,
        msg: String,
        e: &wasmtime::Error,
        store: &Store<Host>,
        started: Instant,
        no_dirs: bool,
    ) -> String {
        if let (Some(_), Some(wasmtime::Trap::Interrupt)) = (self.timeout, e.downcast_ref()) {
            return self.out_of_time(started);
        }
        if let (Some(m), true) = (self.max_memory, store.data().limits.refused) {
            return format!(
                "{}: out of memory, its limit being {m} bytes ({MAX_MEMORY_VAR})",
                self.name
            );
        }
        match (no_dirs, self.named) {
            (false, _) => msg,
            (true, true) => format!("{msg} (it was given no directory)"),
            (true, false) => format!("{msg} (it was given no directory: {DIRS_VAR})"),
        }
    }

    /// What a filter stopped by its time limit fails with.
    fn out_of_time(&self, started: Instant) -> String {
        format!(
            "{}: stopped after {:.1} s, its time limit ({TIMEOUT_VAR}: {})",
            self.name,
            started.elapsed().as_secs_f64(),
            self.timeout.unwrap_or_default().as_secs_f64()
        )
    }

    /// As a filter in a conversion (in a directory sandbox too, it sees its
    /// own directories, not the sandbox's).
    pub fn into_filter<'a>(self) -> Filter<'a> {
        Filter::Wasm(self)
    }
}

/// The environment pandoc gives a JSON filter, as libpandoc does.
fn env(conversion: &Conversion) -> Vec<(&'static str, String)> {
    let mut env = Vec::new();
    if let Some(v) = pandoc_version() {
        env.push(("PANDOC_VERSION", v.to_owned()));
    }
    let reader = conversion.reader_options.as_ref().map(Value::to_string);
    env.push((
        "PANDOC_READER_OPTIONS",
        reader.unwrap_or_else(|| "{}".into()),
    ));
    if let Some(w) = &conversion.writer_options {
        env.push(("PANDOC_WRITER_OPTIONS", w.to_string()));
    }
    if let Some(f) = &conversion.input_format {
        env.push(("PANDOC_INPUT_FORMAT", f.clone()));
    }
    if let Some(f) = &conversion.output_format {
        env.push(("PANDOC_OUTPUT_FORMAT", f.clone()));
    }
    if let Some(s) = conversion.sandbox {
        env.push(("PANDOC_SANDBOX", s.to_string()));
    }
    env
}

impl<'a> Filter<'a> {
    /// A wasm filter from a file.
    pub fn wasm(path: impl AsRef<Path>) -> Result<Self, BoxError> {
        Ok(WasmFilter::from_file(path)?.into_filter())
    }
}

/// A filter's instance: its WASI context, the result of its last call to
/// pandoc, its limits, and whether its time ran out in such a call.
struct Host {
    wasi: WasiP1Ctx,
    result: Option<Result<Output>>,
    limits: Limits,
    deadline: Option<Instant>,
    out_of_time: bool,
}

/// A filter's memory limit, and whether it was reached.
pub(crate) struct Limits {
    pub(crate) max_memory: Option<usize>,
    pub(crate) refused: bool,
}

impl ResourceLimiter for Limits {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let ok =
            self.max_memory.is_none_or(|m| desired <= m) && maximum.is_none_or(|m| desired <= m);
        self.refused |= !ok;
        Ok(ok)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(maximum.is_none_or(|m| desired <= m))
    }
}

impl Host {
    /// A part of the last result, as `result_len` and `result_read` give it.
    fn part(&self, part: i32) -> Vec<u8> {
        match (&self.result, part) {
            (Some(Ok(o)), 0) => o.bytes.clone(),
            (Some(Ok(o)), 1) => Value::from(o.log.clone()).to_string().into_bytes(),
            (Some(Err(e)), 2) => e.kind.clone().into_bytes(),
            (Some(Err(e)), 3) => e.message.clone().into_bytes(),
            _ => Vec::new(),
        }
    }
}

type Ctx<'a> = Caller<'a, Host>;

fn memory(c: &mut Ctx) -> wasmtime::Result<wasmtime::Memory> {
    c.get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| wasmtime::Error::msg("the filter exports no memory"))
}

fn bytes(c: &mut Ctx, ptr: u32, len: u32) -> wasmtime::Result<Vec<u8>> {
    let mut v = vec![0; len as usize];
    memory(c)?.read(&mut *c, ptr as usize, &mut v)?;
    Ok(v)
}

fn json(c: &mut Ctx, ptr: u32, len: u32) -> wasmtime::Result<Result<Value>> {
    let b = bytes(c, ptr, len)?;
    Ok(serde_json::from_slice(&b).map_err(|e| Error::new("PandocOptionError", e.to_string())))
}

/// Keep a call's result for the filter to read; its status. A call that
/// ends past the filter's time limit ends the filter.
fn answer(c: &mut Ctx, r: Result<Output>) -> i32 {
    let h = c.data_mut();
    h.out_of_time |= h.deadline.is_some_and(|d| Instant::now() >= d);
    let status = r.is_err() as i32;
    h.result = Some(r);
    status
}

/// The time the filter has left, if it has a limit: refused once there is
/// none.
fn time_left(c: &mut Ctx) -> Result<Option<Duration>> {
    let Some(deadline) = c.data().deadline else {
        return Ok(None);
    };
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        c.data_mut().out_of_time = true;
        return Err(Error::new(
            Error::TIMEOUT,
            "the filter's time limit is reached",
        ));
    }
    Ok(Some(left))
}

/// Options a filter gives pandoc, within its time: their `"timeout"` the
/// time it has left, or its own if shorter.
fn within(options: Value, left: Option<Duration>) -> Result<Value> {
    let mut o = untrusted(options)?;
    if let Some(left) = left {
        let left = left.as_secs_f64();
        let own = o.get("timeout").and_then(Value::as_f64);
        if !own.is_some_and(|t| t > 0.0 && t < left) {
            o["timeout"] = left.into();
        }
    }
    Ok(o)
}

/// The `libpandoc` imports (see `guest.rs`), on this process's pandoc.
fn add_libpandoc(linker: &mut Linker<Host>) -> wasmtime::Result<()> {
    linker.func_wrap(
        "libpandoc",
        "convert",
        |mut c: Ctx, o: u32, ol: u32, i: u32, il: u32, has: i32| {
            let options = json(&mut c, o, ol)?;
            let input = (has != 0).then(|| bytes(&mut c, i, il)).transpose()?;
            let left = time_left(&mut c);
            let r = options.and_then(|o| {
                let o = within(o, left?)?;
                let input = input.ok_or_else(|| {
                    Error::new("PandocOptionError", "a wasm filter gives convert its input")
                })?;
                crate::backend::convert(&o.to_string(), Some(&input))
            });
            Ok(answer(&mut c, r))
        },
    )?;
    linker.func_wrap("libpandoc", "read_many", |mut c: Ctx, p: u32, l: u32| {
        let left = time_left(&mut c);
        let r = json(&mut c, p, l)?.and_then(|mut req| {
            if !req.is_object() {
                return Err(refused("a request that isn't an object"));
            }
            let options = req
                .get("options")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            req["options"] = within(options, left?)?;
            crate::backend::read_many(&req.to_string())
        });
        Ok(answer(&mut c, r))
    })?;
    linker.func_wrap("libpandoc", "query", |mut c: Ctx, p: u32, l: u32| {
        let left = time_left(&mut c);
        let r = json(&mut c, p, l)?
            .and_then(|q| left.and(untrusted(q)))
            .and_then(|q| crate::backend::query(&q.to_string()));
        Ok(answer(&mut c, r))
    })?;
    linker.func_wrap("libpandoc", "result_len", |c: Ctx, part: i32| {
        c.data().part(part).len() as u32
    })?;
    linker.func_wrap(
        "libpandoc",
        "result_read",
        |mut c: Ctx, part: i32, to: u32| -> wasmtime::Result<()> {
            let data = c.data().part(part);
            memory(&mut c)?.write(&mut c, to as usize, &data)?;
            Ok(())
        },
    )?;
    Ok(())
}

/// What a filter gives pandoc (options, or a query), marked
/// `"untrusted": true` for libpandoc to check: it accepts only what reads
/// and writes no files, fetches nothing and runs nothing, and turns on
/// pandoc's sandbox (libpandoc's `LibPandoc.Untrusted`, which every host
/// shares). Written over whatever the filter wrote, so that it can't name
/// a cancel token there (its calls stop with the conversion they run in).
fn untrusted(what: Value) -> Result<Value> {
    crate::untrusted(what, None)
}

fn refused(what: impl std::fmt::Display) -> Error {
    Error::new(
        "PandocOptionError",
        format!("not allowed for untrusted code: {what}"),
    )
}
