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
  a wasm runtime (`filters/wasm-filter.sh`: `wasmtime run`, or pandocrs's
  `wasm-filter`), and in the browser beside libpandoc.wasm.

- **compiled in:** a program built on the `pandocrs` library names its own
  filters (`examples/my-pandoc.rs`):

  ```rust
  pandocrs::Pandocrs::new("my-pandoc")
      .filter("upper", || libpandoc::Filter::panir(Upper))
      .run(std::env::args().skip(1).collect());
  ```

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
