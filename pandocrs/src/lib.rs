//! pandocrs: the pandoc command, in this process, through libpandoc, with
//! filters in Rust run inside the conversion instead of as programs.
//!
//! `pandocrs ARGS` does what `pandoc ARGS` does (pandoc itself parses the
//! arguments, reads, writes and reports errors, with its exit status),
//! except for the filters named with `-F`/`--filter` (on the command line
//! or in a defaults file) that it can run itself:
//!
//! - **compiled in:** a program built with this library names its own
//!   filters, and `-F name` runs them;
//! - **wasm filters:** `-F name.wasm`, a JSON filter built for WASI
//!   (`wasm32-wasip1`), run by wasmtime (the `wasm` feature). It is looked
//!   for as given, then in pandoc's user data directory's `filters/`, where
//!   pandoc looks for filters too.
//!
//! Other filters run as with pandoc.
//!
//! ```no_run
//! use panir::{Ctx, Inline, Typewise};
//!
//! struct Upper;
//! impl panir::Filter for Upper {
//!     type Order = Typewise;
//!     fn inline(&mut self, x: &mut Inline, _: &mut Ctx<Typewise>) -> Option<Vec<Inline>> {
//!         if let Inline::Str(s) = x { *s = s.to_uppercase().into(); }
//!         None
//!     }
//! }
//!
//! fn main() {
//!     // `my-pandoc -F upper in.md` runs Upper in process
//!     let status = pandocrs::Pandocrs::new("my-pandoc")
//!         .filter("upper", || libpandoc::Filter::panir(Upper))
//!         .run(std::env::args().skip(1).collect());
//!     std::process::exit(status);
//! }
//! ```

use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use libpandoc::Filter;
use serde_json::Value;

/// pandoc's exit status for a failed filter.
pub const FILTER_FAILED: i32 = 83;

type Factory = Box<dyn Fn() -> Filter<'static>>;

/// A pandoc command with filters of its own.
pub struct Pandocrs {
    prog: String,
    filters: Vec<(String, Factory)>,
}

impl Pandocrs {
    /// `prog` is the program's name, for pandoc's messages.
    pub fn new(prog: &str) -> Self {
        Pandocrs {
            prog: prog.into(),
            filters: Vec::new(),
        }
    }

    /// Run `make()` in process for `-F name`.
    pub fn filter(mut self, name: &str, make: impl Fn() -> Filter<'static> + 'static) -> Self {
        self.filters.push((name.into(), Box::new(make)));
        self
    }

    /// Run pandoc with these arguments (without the program's name); the
    /// exit status.
    pub fn run(&self, args: Vec<String>) -> i32 {
        let parsed = libpandoc::parse_args(&args).unwrap_or(Value::Null);
        let mut argv = vec![self.prog.clone()];
        argv.extend(args.iter().cloned());
        let planned = match parsed.get("filters").and_then(Value::as_array) {
            Some(entries) => match self.plan(entries, &args) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{}: {e}", self.prog);
                    return FILTER_FAILED;
                }
            },
            None => None,
        };
        let status = panic::catch_unwind(AssertUnwindSafe(|| libpandoc::main(&argv, planned)));
        let status = match status {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                eprintln!("{}: {e}", self.prog);
                1
            }
            // the panic's message was printed by the panic hook
            Err(_) => FILTER_FAILED,
        };
        match parsed.get("informational").and_then(Value::as_str) {
            Some("Help") => {
                let mut names: Vec<&str> = self.filters.iter().map(|(n, _)| n.as_str()).collect();
                names.sort();
                let names = if names.is_empty() {
                    "none".into()
                } else {
                    names.join(", ")
                };
                println!("\nFilters run in process: compiled in (-F NAME): {names}; wasm (-F NAME.wasm): {}",
                    if cfg!(feature = "wasm") { "yes" } else { "no" });
            }
            Some("VersionInfo") => {
                println!(
                    "{}: pandoc in process, through libpandoc (pandocrs {})",
                    self.prog,
                    env!("CARGO_PKG_VERSION")
                );
            }
            _ => {}
        }
        status
    }

    /// pandoc's filters, with those this program runs itself as callbacks;
    /// `None` if there are none.
    fn plan(
        &self,
        entries: &[Value],
        args: &[String],
    ) -> Result<Option<Vec<Filter<'static>>>, String> {
        let mut any = false;
        let mut out = Vec::new();
        for e in entries {
            let path = (e.get("type").and_then(Value::as_str) == Some("json"))
                .then(|| e.get("path").and_then(Value::as_str))
                .flatten();
            let mine = path.and_then(|p| self.filters.iter().find(|(n, _)| n == p));
            if let Some((_, make)) = mine {
                out.push(make());
                any = true;
            } else if let Some(p) = path.filter(|p| p.ends_with(".wasm")) {
                out.push(wasm_filter(p, args)?);
                any = true;
            } else {
                out.push(Filter::Pandoc(e.clone()));
            }
        }
        Ok(any.then_some(out))
    }
}

#[cfg(feature = "wasm")]
fn wasm_filter(path: &str, args: &[String]) -> Result<Filter<'static>, String> {
    let found = find_filter(path, args).ok_or_else(|| format!("filter {path} not found"))?;
    Filter::wasm(found).map_err(|e| e.to_string())
}

#[cfg(not(feature = "wasm"))]
fn wasm_filter(path: &str, _: &[String]) -> Result<Filter<'static>, String> {
    // pandoc will say it can't run it
    Ok(Filter::json(path))
}

/// A filter's file as pandoc finds one: as given, else in the user data
/// directory's `filters/`.
pub fn find_filter(path: &str, args: &[String]) -> Option<PathBuf> {
    let p = Path::new(path);
    if p.is_file() {
        return Some(p.to_owned());
    }
    let found = data_dir(args)?.join("filters").join(p);
    found.is_file().then_some(found)
}

/// pandoc's user data directory: `--data-dir`, else `$XDG_DATA_HOME/pandoc`
/// (`~/.local/share/pandoc`) if it exists, else `~/.pandoc`.
pub fn data_dir(args: &[String]) -> Option<PathBuf> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if let Some(d) = a.strip_prefix("--data-dir=") {
            return Some(d.into());
        }
        if a == "--data-dir" {
            return it.next().map(PathBuf::from);
        }
    }
    if cfg!(windows) {
        return std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("pandoc"));
    }
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let xdg = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"))
        .join("pandoc");
    Some(if xdg.is_dir() {
        xdg
    } else {
        home.join(".pandoc")
    })
}
