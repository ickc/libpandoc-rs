# libpandoc-rs

[pandoc](https://pandoc.org) as a Rust library, in process, through
[libpandoc](https://github.com/ickc/libpandoc)'s C ABI: no `pandoc`
executable, no subprocess. Documents are [panir](https://github.com/ickc/panir)
values, and filters written in Rust run inside a conversion.

| crate | what |
|---|---|
| `libpandoc-sys` | the raw C ABI (`libpandoc.h`); finds and links the library |
| `libpandoc` | the safe API: `convert`, `convert_args`, `read`, `read_many`, `write`, `query`, filters in Rust (`convert_with`), wasm filters and the directory sandbox (feature `wasm`) |
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
(anything else is an `Error` naming it). For documents only, with the
options yours: `"sandbox": {"read": [...], "write": [...]}` (see
"Security").

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

With the `wasm` feature (on in pandocrs) it also finds libpandoc.wasm, for
the directory sandbox: `$LIBPANDOC_WASM` if set when building, else the one
in the download directory (`continuous/libpandoc.wasm`), else, with
`download`, the release's (~60 MB, once). Its path is built in;
`$LIBPANDOC_WASM` at run time overrides it.

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
in process), except for filters it runs itself, and with
`LIBPANDOC_SANDBOX=dir:dir` or `LIBPANDOC_SANDBOX_WRITE=dir:dir`, where it
runs the conversion in libpandoc.wasm, which sees only those directories
("Security" below):

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

## Security: documents you don't trust

pandoc reads what a document asks for: LaTeX's `\input`, RST, Org,
AsciiDoc, man and Typst includes, images (embedded with
`--embed-resources`, in a docx or an EPUB), a `bibliography:` or `csl:`
in its metadata. Run on a document from elsewhere, it can put any file
you can read into its output. pandoc's own `--sandbox` (`"sandbox": true`)
is all or nothing: the document reads no file at all, not even its own
images.

A **directory sandbox** is a container at the level of mounts: pandoc runs
whole inside libpandoc.wasm under wasmtime (the `wasm` feature) and sees
only the directories you list, as a container sees only its mounts.

```sh
LIBPANDOC_SANDBOX=. LIBPANDOC_SANDBOX_WRITE=out pandocrs paper.tex -s -o out/paper.html
```

```rust
let opts = json!({"from": "latex", "to": "html", "input-files": ["paper.tex"],
                  "sandbox": {"read": ["."], "write": ["out"]}});
let html = libpandoc::convert(&opts, None)?;
```

**What is mounted.** The read list (`"read"`, `LIBPANDOC_SANDBOX`) and the
write list (`"write"`, `LIBPANDOC_SANDBOX_WRITE`, readable too), each
directory at its own absolute path; lists as `PATH` is (`:`, `;` on
Windows). A directory that doesn't exist is an error; a variable set but
empty, or an empty list, mounts nothing. A private temporary directory.
No network, no programs. Nothing else: **not the user data directory, not
`PANDOC_DATA_DIRS`**: list them if pandoc is to find your templates and
filters there.

**Everything pandoc reads must be in a mounted directory**: what the
document names, and what you name too (input files, template, CSS,
bibliography, defaults files, Lua filters). Outside, a file is missing.
Output goes to standard output, or into a directory of the write list
(`-o`, `--extract-media`, `--log`). The options and arguments reach pandoc
untouched. Listing a directory opens all of it to the document: list the
document's own directory, not your home.

**Filters.** Lua filters run inside, confined as the document is (in
plain pandoc a Lua filter can do anything you can). Wasm filters given to
`convert_with` see the same mounts; filters in Rust run there too, as the
rest of your code does, unconfined. Filters that are programs (JSON
filters) fail: wasm starts no process, and a filter run outside would
read whatever the document names. On the command line, `pandocrs` refuses
the filters it would run in process (wasm, compiled in); `pandoc lua` is
refused.

**Limits.** `LIBPANDOC_WASM_TIMEOUT` (seconds) and
`LIBPANDOC_WASM_MAX_MEMORY` (bytes, or with `k`, `m`, `g`) stop a
conversion past them, counted from the module's start; none by default,
wasm's 4 GiB at most. A document can be built to take time or memory
(Typst's `#while true {}`: 3 GB in 4.5 s natively; 100,000 nested `>`:
over 15 s): set **both** for documents from the internet. The memory
limit alone does not stop every such document: a loop that grows slowly,
or not at all, runs on until the time limit (in the module, Typst's bare
loop reached 512 MB in 5 s on one machine and not in a minute on
another).

**What to know.**

- `..` from one mounted directory into another fails (each mount resolves
  alone), in an absolute path too; a path without `..` works.
- Relative names resolve from the working directory, which must be in a
  mounted directory; if it isn't, they find nothing and the error says so.
- On Windows a path with a drive letter means nothing inside: use relative
  names with `/`, or `/c/...` for `C:\...`.
- A path leaving a mount by `..` or a symbolic link fails as "permission
  denied", where a missing file "does not exist": a document can tell the
  two apart where pandoc shows the error (an input file, an AsciiDoc or
  Typst include), though it reads nothing. A template, CSS or bibliography
  reads as missing either way.
- PDF output needs a program and fails, saying so: write LaTeX, HTML or
  Typst and make the PDF outside.
- `pandocrs` prints pandoc's warnings whatever `--quiet` or `--verbose` say.

**What it doesn't protect from:** a hard link inside a mounted directory
to a file elsewhere (it is that file), and anything else you put in the
directories; documents built to use time or memory beyond the limits you
set; bugs in wasmtime or in this host.

**What it costs** (pandoc's MANUAL, markdown to HTML, one run of
`pandocrs`, 32 threads): 2.1 s and 0.96 GB at most against 0.6 s and
0.35 GB natively. The first run compiles the 60 MB module, once, then
wasmtime caches it (65 MB): 20 s, and **about 8 GB of memory**, RAM and
swap together. No setting of wasmtime's lowers that (compiling on one
thread, unoptimised, without address maps or unwind info: 7.5 to 8 GB
each; its baseline compiler can't compile the module). With 4 GB of RAM
and swap the first run took 74 s; without swap, under 8 GB, it is killed.

**For real isolation**, run the conversion where only the directory
exists, a container at least, a virtual machine better:

```sh
podman run --rm --network=none --read-only --tmpfs /tmp --cap-drop=all \
  --security-opt=no-new-privileges --memory=2g --cpus=1 \
  -v "$PWD/doc:/work:ro,Z" -w /work IMAGE \
  timeout 60 pandocrs main.tex -t html > out.html
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
are marked `"untrusted": true` and libpandoc checks them: one list for
every host.

Rust has no stable ABI, so there are no native plugins (`.so` filters):
filters to distribute are wasm.

## Tests

```sh
rustup target add wasm32-wasip1       # the example filters, built by the tests
LIBPANDOC_PREFIX=/path/to/libpandoc LIBPANDOC_WASM=/path/to/libpandoc.wasm \
  cargo test --workspace --all-features
```

The sandbox's tests (`libpandoc/tests/sandbox.rs`) run the attack suite of
the project's sandbox study: 75 documents that try to read files beside
the directory (LaTeX, RST, Org, AsciiDoc, man, Typst, djot, wikis,
markdown, HTML, DocBook, JATS, citeproc metadata; to HTML, docx, odt,
EPUB, ...), each checked for the canaries in its output, messages and
extracted media, for connections to a local server, and against a missing
file in its place; then the rules of the mounts (what the options name,
outputs, filters, the working directory). `pandocrs/tests/sandbox.rs`:
the same through the command, the data directories, and the limits. CI
sets `LIBPANDOC_WASM_TIMEOUT=120` for the tests, so that a conversion that
hangs fails; the command's tests kill a run after 3 minutes. The tests
that need symbolic links run on Unix only (Windows needs a privilege to
make one).

Rust 1.97 or newer (wasmtime's minimum): CI runs the tests on 1.97, and
the minimum is raised only when a dependency needs it.

## License

GPL-2.0-or-later, as pandoc.
