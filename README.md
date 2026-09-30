# libpandoc-rs

[pandoc](https://pandoc.org) as a Rust library, in process, through
[libpandoc](https://github.com/ickc/libpandoc)'s C ABI: no `pandoc`
executable, no subprocess. Documents are [panir](https://github.com/ickc/panir)
values, and filters written in Rust run inside a conversion.

| crate | what |
|---|---|
| `libpandoc-sys` | the raw C ABI (`libpandoc.h`); finds and links the library |
| `libpandoc` | the safe API: `convert`, `convert_args`, `read`, `read_many`, `write`, `query`, filters in Rust (`convert_with`), wasm filters (feature `wasm`) |
| `pandocrs` | the pandoc command in process, running Rust filters itself: compiled in, or `.wasm` |

Status: prototype. Tier 3 in the project's priorities (after Python, and
JavaScript/wasm).

```rust
use libpandoc::Filter;
use serde_json::json;

let out = libpandoc::convert_with(
    &json!({"from": "markdown", "to": "html"}),
    Some(b"hello *world*"),
    vec![Filter::func(|doc, conversion| {
        // change the panir::Pandoc; conversion tells the formats and options
        Ok(())
    })],
)?;
```

For documents or options from someone you don't trust, `"untrusted": true`
in the options: libpandoc then accepts only options that read no files,
write none, fetch nothing and run nothing, with pandoc's sandbox on
(anything else is an `Error` naming it).

## Finding libpandoc

At build time `libpandoc-sys` looks in `$LIBPANDOC_PREFIX`, then `$PREFIX`
and `$CONDA_PREFIX`, for `lib/libpandoc.so` (`.dylib`; `bin/pandoc.dll`
on Windows): an unpacked [release](https://github.com/ickc/libpandoc/releases)
or a conda environment. The same variables as libpandoc-python. Then it
asks pkg-config (`libpandoc.pc`). Then, with the `download` feature (on
by default in pandocrs, off in the libraries), it downloads the release's
build for the target once, into `$LIBPANDOC_DOWNLOAD_DIR` (default
`~/.local/share/libpandoc`, `~/Library/Application Support/libpandoc`,
`%LOCALAPPDATA%\libpandoc`), which outlives `cargo install`'s build
directory. So with nothing installed:

```sh
cargo install pandocrs    # downloads libpandoc once (~60 MB)
```

On Windows, add the downloaded `bin` to `PATH` (the build says where). For
now the download is the `continuous` build, whose checksum can't be
pinned; a tagged libpandoc release will be pinned by SHA-256.

At run time no `LD_LIBRARY_PATH` is needed: the directory is baked in as an
rpath (`DEP_PANDOC_RPATH`, which a program's build script passes to the
linker, as `pandocrs/build.rs` does; Cargo passes link arguments only to
the package asking). `$LIBPANDOC_RPATH` sets another, such as
`$ORIGIN/../lib` for a relocatable install (`bin/` beside `lib/`, as in a
conda package; `$ORIGIN` becomes `@loader_path` on macOS), or empty for
none. libpandoc-python's `setup.py` reads both variables the same way. On
Windows the DLL is linked with `raw-dylib` (no import library) and found on
`PATH` or next to the program.

## pandocrs

`pandocrs ARGS` does what `pandoc ARGS` does (it is pandoc's own `main`,
in process), except for filters it runs itself:

- **wasm filters:** `-F name.wasm` is a pandoc JSON filter compiled for
  WASI (`wasm32-wasip1`), run in process by wasmtime, sandboxed: it sees
  the current directory read-only, and the environment pandoc gives
  filters. Looked for as given, then in pandoc's user data directory's
  `filters/`. A panir filter (`panir::filter`) is one as it is:

  ```sh
  cd filters && cargo build --release --target wasm32-wasip1
  pandocrs -F filters/target/wasm32-wasip1/release/upper.wasm input.md
  ```

  The same file runs as an ordinary JSON filter under plain pandoc through
  `filters/wasm-filter.sh` (pandocrs's `wasm-filter` if installed, as
  pandocrs runs it; else `wasmtime run`, which can only give a directory
  read-write, so none), and in the browser beside libpandoc.wasm.

- **compiled in:** a program built on the `pandocrs` library names its own
  filters (`examples/my-pandoc.rs`):

  ```rust
  pandocrs::Pandocrs::new("my-pandoc")
      .filter("upper", || libpandoc::Filter::panir(Upper))
      .run(std::env::args().skip(1).collect());
  ```

## Why wasm: filters you needn't trust

Every other kind of pandoc filter can do what you can: a JSON filter is a
program, a Lua filter has Lua's `io` and `os` and pandoc's `pandoc.system`
and `pandoc.pipe`, and pandoc's `--sandbox` limits readers and writers,
not filters (pandoc's manual: "audit filters and custom writers very
carefully"). Nor does pandoc limit a filter's time or memory. A wasm
filter runs behind a boundary its host enforces, whatever pandoc's
options:

- **files:** only the directories it is given (the current one,
  read-only, by default); no network, no programs, only the environment
  variables pandoc gives filters;
- **calls to pandoc:** in pandoc's sandbox, with no options that read or
  write files, fetch or run anything (libpandoc's `"untrusted"`);
- **time and memory:** `LIBPANDOC_WASM_TIMEOUT` (seconds, as
  pandoc-server's `--timeout`) and `LIBPANDOC_WASM_MAX_MEMORY` (bytes, or
  with `k`, `m`, `g`, as pandoc's `+RTS -M`) stop a filter past them
  (none by default, as pandoc; `WasmFilter::timeout` and `::max_memory`
  for one filter). The time includes its calls to pandoc; the memory is
  the filter's own (pandoc's is the process's).

So a filter from anywhere can run on your documents at the risk of what it
does to the document, not to your machine (`filters/src/bin/misbehave.rs`
tries the rest).

## Wasm filters that call pandoc

A filter that parses fragments (as pantable parses table cells) or renders
some calls pandoc. Built for wasm, the `libpandoc` crate's `read`,
`read_as`, `read_many`, `write` and `convert` are calls to the program
running the filter (imports from a `libpandoc` module; see
`libpandoc/src/guest.rs`), so the same code works compiled in and as a
wasm filter (`filters/src/bin/parse.rs`):

```rust
let docs = libpandoc::read_many_as(&texts, conversion)?; // all at once, in parallel
```

Hosts: pandocrs and any program with `libpandoc`'s `wasm` feature;
libpandoc.wasm in Node and browsers (`wasm/wasm-filter.mjs`, given the
`pandoc`); and plain pandoc through `wasm-filter` (pandocrs's second
program: `exec wasm-filter "$0.wasm" "$@"`), which has a libpandoc of its
own. `wasmtime run` can't: a filter that calls pandoc imports what it has
not. A filter that doesn't imports nothing and runs anywhere.

The calls stay in the filter's sandbox: they run in pandoc's (readers read
no files: no LaTeX `\input`, no RST `include`), and options that would read
or write files, fetch resources or run programs (`filters`, `template`,
`output-file`, a Lua reader or writer, `pdf`, ...) are refused. The calls
are marked `"untrusted": true` and libpandoc (≥ 1.7) checks them: one list
for every host.

Rust has no stable ABI, so there are no native plugins (`.so` filters):
filters to distribute are wasm.

## Tests

```sh
rustup target add wasm32-wasip1       # the example filters, built by the tests
LIBPANDOC_PREFIX=/path/to/libpandoc cargo test --workspace --all-features
```

## License

GPL-2.0-or-later, as pandoc.
