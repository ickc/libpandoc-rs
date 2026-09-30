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
//! libpandoc.wasm. It runs sandboxed: it sees only the directories it is
//! given (by default the current directory, read-only), no network, and
//! the environment variables pandoc gives filters.
//!
//! A filter may call pandoc: built with this crate, `libpandoc::read` and
//! the like call this process's pandoc (the imports of `guest.rs`). Those
//! calls are marked `"untrusted": true` (libpandoc 1.7), so libpandoc runs
//! them in pandoc's sandbox and refuses options that would read or write
//! files, fetch resources or run programs: the filter gets no more access
//! through pandoc than it has itself. The list is libpandoc's, the same
//! for every host.
//!
//! Compiled code is cached (wasmtime's cache, e.g. `~/.cache/wasmtime`), so
//! only a filter's first run pays for compiling it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::Value;
use wasmtime::{Caller, Config, Engine, Linker, Module, Store};
use wasmtime_wasi::p1::{self, WasiP1Ctx};
use wasmtime_wasi::p2::pipe::{MemoryInputPipe, MemoryOutputPipe};
use wasmtime_wasi::{FsPerms, I32Exit, WasiCtxBuilder};

use crate::{BoxError, Conversion, Error, Filter, Output, Result};

/// The engine all wasm filters share, with wasmtime's cache when it can
/// have one.
fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut config = Config::new();
        if let Ok(cache) = wasmtime::Cache::new(wasmtime::CacheConfig::new()) {
            config.cache(Some(cache));
        }
        Engine::new(&config).expect("wasmtime's default configuration")
    })
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

/// A compiled wasm filter.
#[derive(Clone)]
pub struct WasmFilter {
    name: String,
    module: Module,
    preopens: Vec<Preopen>,
}

impl WasmFilter {
    /// Compile the filter at `path` (or take it from the cache).
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, BoxError> {
        let path = path.as_ref();
        let module = Module::from_file(engine(), path)
            .map_err(|e| format!("{}: not a wasm filter: {e}", path.display()))?;
        Ok(Self::new(path.display().to_string(), module))
    }

    /// Compile a filter from its bytes; `name` is its first argument.
    pub fn from_bytes(name: &str, wasm: &[u8]) -> Result<Self, BoxError> {
        let module =
            Module::new(engine(), wasm).map_err(|e| format!("{name}: not a wasm filter: {e}"))?;
        Ok(Self::new(name.to_owned(), module))
    }

    fn new(name: String, module: Module) -> Self {
        pandoc_version();
        let cwd = Preopen {
            host: ".".into(),
            guest: ".".into(),
            writable: false,
        };
        WasmFilter {
            name,
            module,
            preopens: vec![cwd],
        }
    }

    /// Let the filter see `host` as `guest` (read-only unless `writable`).
    pub fn preopen(mut self, host: impl Into<PathBuf>, guest: &str, writable: bool) -> Self {
        self.preopens.push(Preopen {
            host: host.into(),
            guest: guest.into(),
            writable,
        });
        self
    }

    /// See no directory at all.
    pub fn no_files(mut self) -> Self {
        self.preopens.clear();
        self
    }

    /// Run the filter on a document (pandoc's JSON).
    pub fn run(&self, doc: &[u8], conversion: &Conversion) -> Result<Vec<u8>, BoxError> {
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
        for p in &self.preopens {
            let perms = if p.writable {
                FsPerms::ReadWrite
            } else {
                FsPerms::ReadOnly
            };
            wasi.preopened_dir(&p.host, &p.guest, perms)
                .map_err(|e| format!("{}: {e}", p.host.display()))?;
        }
        let host = Host {
            wasi: wasi.build_p1(),
            result: None,
        };
        let mut store = Store::new(engine(), host);
        let mut linker: Linker<Host> = Linker::new(engine());
        p1::add_to_linker_sync(&mut linker, |h| &mut h.wasi)?;
        add_libpandoc(&mut linker)?;
        let instance = linker.instantiate(&mut store, &self.module)?;
        let start = instance.get_typed_func::<(), ()>(&mut store, "_start")?;
        match start.call(&mut store, ()) {
            Ok(()) => {}
            Err(e) => match e.downcast_ref::<I32Exit>() {
                Some(I32Exit(0)) => {}
                Some(I32Exit(n)) => {
                    return Err(format!("{} exited with status {n}", self.name).into())
                }
                None => return Err(format!("{}: {e}", self.name).into()),
            },
        }
        drop(store);
        Ok(stdout
            .try_into_inner()
            .map(|b| b.to_vec())
            .unwrap_or_default())
    }

    /// As a filter in a conversion.
    pub fn into_filter<'a>(self) -> Filter<'a> {
        Filter::raw(move |doc, conversion| self.run(doc, conversion))
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
    if let Some(f) = &conversion.input_format {
        env.push(("PANDOC_INPUT_FORMAT", f.clone()));
    }
    if let Some(f) = &conversion.output_format {
        env.push(("PANDOC_OUTPUT_FORMAT", f.clone()));
    }
    env
}

impl<'a> Filter<'a> {
    /// A wasm filter from a file.
    pub fn wasm(path: impl AsRef<Path>) -> Result<Self, BoxError> {
        Ok(WasmFilter::from_file(path)?.into_filter())
    }
}

/// A filter's instance: its WASI context, and the result of its last call
/// to pandoc.
struct Host {
    wasi: WasiP1Ctx,
    result: Option<Result<Output>>,
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

fn answer(c: &mut Ctx, r: Result<Output>) -> i32 {
    let status = r.is_err() as i32;
    c.data_mut().result = Some(r);
    status
}

/// The `libpandoc` imports (see `guest.rs`), on this process's pandoc.
fn add_libpandoc(linker: &mut Linker<Host>) -> wasmtime::Result<()> {
    linker.func_wrap(
        "libpandoc",
        "convert",
        |mut c: Ctx, o: u32, ol: u32, i: u32, il: u32, has: i32| {
            let options = json(&mut c, o, ol)?;
            let input = (has != 0).then(|| bytes(&mut c, i, il)).transpose()?;
            let r = options.and_then(|o| {
                let o = untrusted(o)?;
                let input = input.ok_or_else(|| {
                    Error::new("PandocOptionError", "a wasm filter gives convert its input")
                })?;
                crate::backend::convert(&o.to_string(), Some(&input))
            });
            Ok(answer(&mut c, r))
        },
    )?;
    linker.func_wrap("libpandoc", "read_many", |mut c: Ctx, p: u32, l: u32| {
        let r = json(&mut c, p, l)?.and_then(|mut req| {
            if !req.is_object() {
                return Err(refused("a request that isn't an object"));
            }
            let options = req
                .get("options")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            req["options"] = untrusted(options)?;
            crate::backend::read_many(&req.to_string())
        });
        Ok(answer(&mut c, r))
    })?;
    linker.func_wrap("libpandoc", "query", |mut c: Ctx, p: u32, l: u32| {
        let r = json(&mut c, p, l)?
            .and_then(untrusted)
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
/// shares).
fn untrusted(what: Value) -> Result<Value> {
    let v = unsafe { libpandoc_sys::pandoc_abi_version() };
    if v < 1007 {
        return Err(refused(format!(
            "a call from a wasm filter needs libpandoc 1.7 (\"untrusted\"), not {}.{}",
            v / 1000,
            v % 1000
        )));
    }
    let Value::Object(mut o) = what else {
        return Err(refused("options that aren't an object"));
    };
    o.insert("untrusted".into(), true.into());
    Ok(Value::Object(o))
}

fn refused(what: impl std::fmt::Display) -> Error {
    Error::new(
        "PandocOptionError",
        format!("not allowed for untrusted code: {what}"),
    )
}
