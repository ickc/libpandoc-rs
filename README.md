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
or a conda environment. The same variables as libpandoc-python.

At run time no `LD_LIBRARY_PATH` is needed: the directory is baked in as an
rpath (`DEP_PANDOC_RPATH`, which a program's build script passes to the
linker, as `pandocrs/build.rs` does; Cargo passes link arguments only to
the package asking). `$LIBPANDOC_RPATH` sets another, such as
`$ORIGIN/../lib` for a relocatable install next to the library. On
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
  a wasm runtime (`wasmtime run`), and in the browser beside libpandoc.wasm.

- **compiled in:** a program built on the `pandocrs` library names its own
  filters (`examples/my-pandoc.rs`):

  ```rust
  pandocrs::Pandocrs::new("my-pandoc")
      .filter("upper", || libpandoc::Filter::panir(Upper))
      .run(std::env::args().skip(1).collect());
  ```

Rust has no stable ABI, so there are no native plugins (`.so` filters):
filters to distribute are wasm.

## Tests

```sh
rustup target add wasm32-wasip1       # the example filters, built by the tests
LIBPANDOC_PREFIX=/path/to/libpandoc cargo test --workspace --all-features
```

## License

GPL-2.0-or-later, as pandoc.
