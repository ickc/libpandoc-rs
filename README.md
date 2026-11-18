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

A filter's `conversion` tells the formats pandoc decided on, the reader's
and the writer's options, whether pandoc's sandbox is on (`sandbox`), and
the conversion's options (`convert_with`'s, the directory sandbox's
included; none for `main`'s). `read_as(text,
conversion)`, `read_many_as` and `read_options(conversion, format)` read a
fragment the way the document was read. Read so, a text keeps its tabs,
as Lua's `pandoc.read` reads it; the `pandoc` command first replaces them
by spaces (`"preserve-tabs": false` does). A call each costs about 0.3 ms,
six times `pandoc.read`'s: batch them. amsthm's 738 notes take 0.21 s read
one by one and 0.013 s in one `read_many`. A text it can't read is an
error in that text's place (`ReadMany::docs`), its message `reading input
N: ...`, N counting from 0: the text's place in the list given, not in the
document, for the filter to map back to where the text came from.
**Confined when the
conversion is**: document text read this way may name files (LaTeX's
`\input`, RST's and Org's includes). When the conversion is sandboxed, by
pandoc's sandbox (`"sandbox": true`, `--sandbox`, a defaults file,
`"untrusted"`) or a directory sandbox (`"sandbox": {...}`,
`LIBPANDOC_SANDBOX`), the fragments are read in pandoc's own sandbox: they
read no files, not even in the directories listed (the filter runs in this
process, outside them). libpandoc tells every filter whether pandoc's
sandbox is on (`conversion.sandbox`, from a callback's context;
`PANDOC_SANDBOX` for wasm and JSON filters), and the directory sandbox
tells its filters it is. The rule: a read is confined when the
conversion's `sandbox` is `Some(true)`, or its options ask for a sandbox,
or when nothing can tell (`sandbox` is `None` and there are no options: a
conversion made by hand, or a JSON filter's under plain pandoc). A filter
that wants the files read sets `"sandbox": false` in `read_options`'s
result: its code, its choice.

It needs libpandoc 1.14 or a later 1.x (and so does the directory sandbox
of libpandoc.wasm): checked at the first call, which with an older library
fails with an `Error` naming both versions.

Options are pandoc's defaults-file keys, as JSON. A key whose value is
`null` is absent, as Lua's `nil` in a table (`"timeout": null`: no time
limit; in a sandbox, `"timeout": null`: the environment's); a list with a
`null` in it is an error.

Options from code you don't trust: `"untrusted": true` in them (a wasm
filter's calls to pandoc get it): libpandoc then accepts only options that
read no files, write none, fetch nothing and run nothing, with pandoc's
sandbox on (anything else is an `Error` naming it). The host writes the
key over whatever the code wrote, as `libpandoc::untrusted(options,
token)` does: `true`, or with a `CancelToken` `{"cancel-token":
token.id()}` (below). It is about options: it bounds neither the time nor
the memory a document takes. Documents from
someone you don't trust: `"sandbox": {"read": [...], "write": [...]}`,
with its time and memory limits (see "Security").

Time: `"timeout": seconds` in the options of `convert`, `convert_with` and
`read_many` stops a call that runs longer: an `Error` whose `is_timeout()`
(kind `"Timeout"`); in `read_many`, each input not read by then is one, in
its place, and the others are kept. It stops pandoc's own code, not a
filter in Rust (the call fails when the filter returns) nor Lua (filters,
custom readers and writers), which nothing stops.

Cancelling: a `CancelToken` stops calls while they run, from another
thread (a cancel button) or from a filter: give `"cancel-token":
token.id()` in the options of `convert`, `convert_with`, `read_many` or
`write_with`, and call `token.cancel()`; each call using it fails with an
`Error` whose `is_cancelled()` (kind `"Cancelled"`), and the process goes
on. It stops what `"timeout"` stops (in the directory sandbox, the whole
conversion, Lua included: the module is stopped from outside, by
wasmtime's epoch, which also brings other wasm runs' time limits one tick,
10 ms, nearer; loading the module can't be interrupted: a cancel during
it stops the call once the module has loaded, 0.2 s from wasmtime's cache,
a first compilation's tens of seconds otherwise). A token stays cancelled, serves any number of calls, is
`Send` and `Sync`, and is freed when its last clone is dropped.
Untrusted options take it inside the key the host writes, `"untrusted":
{"cancel-token": token.id()}` (`libpandoc::untrusted(options,
Some(&token))`; libpandoc 1.14), not beside it: a top-level
`"cancel-token"` with `"untrusted"` is refused, as the code could have
written it. **Ctrl-C:** the library handles no signal, so SIGINT keeps
its default action and ends the program at once, as it ends pandoc
(`pandocrs` too). To stop the call and go on instead, cancel a token from
a handler that runs on a thread of its own (not a raw signal handler:
cancelling takes locks and allocates), such as the
[`ctrlc`](https://crates.io/crates/ctrlc) crate's:

```rust
let token = libpandoc::CancelToken::new()?;
let t = token.clone();
ctrlc::set_handler(move || t.cancel())?;
match libpandoc::convert(&json!({"to": "html", "cancel-token": token.id()}), Some(input)) {
    Err(e) if e.is_cancelled() => eprintln!("interrupted"),
    r => println!("{}", r?.text()),
}
```

## Consecutive filters

pandoc hands a filter in this process the whole document as its JSON and
reads back the one it returns: an exchange, about 0.3 s each on a 9.8 MB
textbook, most of it pandoc decoding what comes back. **Consecutive
filters of this process share one exchange, by default**: filters in
Rust on the document (`Filter::func`, `Filter::panir`), on its JSON
(`Filter::raw`), and wasm filters. One of pandoc's own (Lua, a JSON
program, citeproc) ends a run. The same in `convert_with`, `main`
(pandocrs: `-F a -F b`, compiled in or `.wasm`) and the directory sandbox.
Within a run, from one filter to the next:

- from a filter on the document to another: the same `panir::Pandoc`, as
  it is (the same `&Conversion` too);
- from one on the document to one on JSON: panir writes it once;
- from one on JSON (raw or wasm) to one on the document: panir parses what
  it wrote, once;
- from one on JSON to another: the bytes exactly as written, unchecked.

A wasm filter keeps its own limits and directories, and its calls into
pandoc are as they were. On the textbook, JSON to HTML (best of 7,
`convert_with`), three filters on the document that change nothing took
3.17 s and now 2.51 s, what one takes (none: 2.19 s); three wasm
`identity` filters in pandocrs take 3.15 s apart and 2.54 s together (one:
2.53 s; none: 2.21 s; the whole command, best of 5).

**The switch:** `"separate-filters": true` in `convert_with`'s options
makes every filter its own exchange (as before this change), so that pandoc
reads and checks the document between any two. It is this crate's option,
not pandoc's (taken out of the options before pandoc sees them; `convert`
takes it too, and ignores it). By default it is
`LIBPANDOC_SEPARATE_FILTERS`: `1` on; unset, empty or `0` off; anything
else an error naming the variable and the value (a safety switch is not to
be ignored); the options' value wins over it. `main` and pandocrs obey the
variable.

**The safety side.** Chained, pandoc no longer reads the document between
two filters: a later filter is given exactly what the earlier one wrote,
and for raw and wasm filters, its bytes. pandoc always checks the final
document. The switch is for finding which filter broke a document, and for
not letting one filter's bytes reach the next unchecked.

What changes for a filter's author, against the same filters apart:

- The next filter gets exactly what the one before left, not the document
  pandoc read and wrote again. Between filters on the document, little can
  differ with panir's types: its `Meta` is a sorted map as pandoc's, and
  what panir writes pandoc reads as the same document. The one difference
  seen: panir reads no document nested more than `panir::MAX_DEPTH` (256)
  levels, and nothing is read between two filters on the document, so one
  may leave a deeper document for the next to undo, where apart the next
  fails to parse it. After a raw or wasm filter, the next is given its
  bytes as they are.
- A document pandoc would refuse is refused when the run ends, not after
  the filter that made it (or a filter of the run that can't read it
  fails first, below).
- pandoc's log (`--verbose`, pandocrs) counts a run as one filter: one
  `Running filter libpandoc:callback/0` for it.
- State in the document's metadata, and a filter's own (a struct's fields,
  a closure's captures, a static), behave the same either way: the same
  process and the same document.
- A failing filter fails the run, and those after it don't run. Its error
  names it by its place in the list given, from 0 (pandoc names the
  callback, which is the run): `Error running filter 'callback 0':
  filters[2]: ...`. When what it failed on went unchecked from a raw or
  wasm filter before it, the error says whose it was: `(its input is what
  filters[1] wrote, unchecked: LIBPANDOC_SEPARATE_FILTERS=1 has pandoc
  check it)`. A panic is raised again, as for one filter.
- Cancelling: between two filters of a run, the conversion's `CancelToken`
  (its `"cancel-token"`) is looked at, and a cancelled one stops the run
  before the next (the call fails as cancelled). A `"timeout"` is not: a
  run goes to its end, as one filter does, and the call fails then.

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

On Windows, add the downloaded `bin` to `PATH` (the build says where).

The download is the `continuous` release, which every push to libpandoc's
`main` replaces. When the release has a `SHA256SUMS` (from libpandoc 1.12),
each file downloaded is checked against it, and a file that doesn't match,
or that it doesn't list, fails the build (a download made while the release
is being replaced may: build again); without one the build warns that
nothing was checked. That catches a download corrupted or cut short, not a
file replaced on purpose: `SHA256SUMS` comes from the same place, and
anyone who can change the release can change both. The protection against
that is a SHA-256 pinned here for each file of a tagged release, for when
libpandoc tags releases (`libpandoc-sys/build.rs` has the table); until
then, verify a download's provenance yourself (libpandoc's README:
"Verifying a download") and build with `LIBPANDOC_PREFIX`.

With the `wasm` feature (on in pandocrs) it also finds libpandoc.wasm, for
the directory sandbox: `$LIBPANDOC_WASM` if set when building, else the one
in the download directory (`continuous/libpandoc.wasm`), else, with
`download`, the release's (~60 MB, once), with its licence notices beside
it (`libpandoc-wasm-THIRD-PARTY-NOTICES.txt`). Its path is built in;
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
`LIBPANDOC_SANDBOX=dir:dir` or `LIBPANDOC_SANDBOX_WRITE=dir:dir`, where
pandoc's command runs in libpandoc.wasm, which sees only those directories
("Security" below), with the same filters run in process:

- **wasm filters:** `-F name.wasm` is a pandoc JSON filter compiled for
  WASI (`wasm32-wasip1`), run in process by wasmtime, sandboxed: it sees
  the environment pandoc gives filters (none of this process's) and **no
  directory**, as a wasm filter is for code you don't trust, unless
  `LIBPANDOC_WASM_DIRS` gives some: directories as `PATH` lists them (`:`,
  `;` on Windows; unset or empty: none), read-only, the same for every
  wasm filter, in the directory sandbox too. One that doesn't exist or
  isn't a directory fails the filter before it runs
  (`LIBPANDOC_WASM_DIRS: X does not exist`, `... is not a directory`). A
  filter that fails while it was given none says so: `(it was given no
  directory: LIBPANDOC_WASM_DIRS)`. Looked for as given, then in pandoc's
  user data directory's `filters/`. A panir filter (`panir::filter`) is
  one as it is:

  ```sh
  cd filters && cargo build --release --target wasm32-wasip1
  pandocrs -F filters/target/wasm32-wasip1/release/upper.wasm input.md
  LIBPANDOC_WASM_DIRS=.:../common pandocrs -F include.wasm input.md
  ```

  The filter sees each directory under its absolute path (`C:\x` as `/c/x`
  on Windows; its real path too, if a link leads to it) and under its
  path relative to the working directory (`.`, `..`, `../common`, `img`;
  none on another drive), and the working directory as `.` when it lies
  inside one: so `here.txt`, `../common/part.txt` and absolute names work
  as for a native filter. The filter's own libc (wasi-libc) matches the
  beginning of a path against those names, without normalising it:
  `img/../../common/x` doesn't work ("Operation not permitted"), and a
  leading `./` is not to be relied on (`./../common/x`; `./img/x` when
  only `img` is given): the same Rust filter drops it built without
  link-time optimisation, and keeps it, so fails, built with `lto = true`.
  A symbolic link is refused when it leaves the directory its name is
  opened through, even into another one given. A program gives a filter
  its own directories
  instead of the variable's: `WasmFilter::dirs` (named as above),
  `::preopen(host, guest, writable)` (exactly), `::no_files()` (none).

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
plain pandoc a Lua filter can do anything you can). Wasm filters run in
this process, seeing their own directories (none, unless
`LIBPANDOC_WASM_DIRS` or the program gives some), not the mounts: the
document's list and the filters' are different lists. Filters in Rust run
there too, as the rest of your code does, unconfined. Filters that are programs (JSON
filters) fail: wasm starts no process, and a filter run outside would
read whatever the document names. On the command line, `pandocrs` runs
wasm filters and compiled-in ones the same way, in process; `pandoc lua`
and `pandoc server` are not in the module (status 4). A filter in Rust
that reads fragments with `read_as(text, conversion)` or `read_many_as`
reads them in pandoc's own sandbox: no files, not even in the directories
listed (above): every filter running in this process is told the
conversion is sandboxed (`conversion.sandbox`, and `PANDOC_SANDBOX=true`
for wasm filters), where the module tells of pandoc's own sandbox only.

**Limits.** `LIBPANDOC_WASM_TIMEOUT` (seconds) and
`LIBPANDOC_WASM_MAX_MEMORY` (bytes, or with `k`, `m`, `g`), or for one
conversion the sandbox's `"timeout"` (seconds) and `"max-memory"` (bytes,
an integer), stop a conversion past them, counted from the module's start
(as a `CancelToken` stops it);
none by default (0 is none too), wasm's 4 GiB at most. A negative one is
an error. A conversion stopped by the time limit fails with an
`Error` whose `is_timeout()`, as a call's `"timeout"` (which works in the
module too). A document can be built to take time or memory (Typst's
`#while true {}`: 3 GB in 4.5 s natively; 100,000 nested `>`: over 15 s):
set **both** for documents from the internet. The memory limit alone does
not stop every such document: a loop that grows slowly, or not at all, runs
on until the time limit (in the module, Typst's bare loop reached 512 MB in
5 s on one machine and not in a minute on another). The time limit covers
pandoc and Lua inside the module, not filters run in this process (a
filter in Rust; a wasm filter has its own limits).

Natively (no sandbox), a call has a time limit if its options give one
(`"timeout"`), and there is no memory limit at all: a document that takes
all the memory ends the process. For documents you don't trust, the
sandbox, or a process of their own.

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
- In WASI an empty file name is the working directory: a Lua filter's
  `io.open("")` opens it, where natively it is a missing file, and reading
  it then fails (include-code-files' `include=""` fails the conversion
  inside, where natively it is an empty block); a wasm filter's likewise.
- PDF output needs a program and fails, saying so: write LaTeX, HTML or
  Typst and make the PDF outside.
- `pandocrs --version` says what runs: the module's pandoc
  (`Features: -server -lua`).

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
options (the document it returns is another matter: below the list):

- **files:** only the directories it is given (none by default;
  `LIBPANDOC_WASM_DIRS` or the program gives some, read-only); no network,
  no programs, only the environment variables pandoc gives filters;
- **calls to pandoc:** in pandoc's sandbox, with no options that read or
  write files, fetch or run anything (libpandoc's `"untrusted"`): they read
  no file, not even one the filter can read itself (for citeproc, the
  filter reads the bibliography, style and abbreviations and gives them
  as their contents: below);
- **time and memory:** `LIBPANDOC_WASM_TIMEOUT` (seconds, as
  pandoc-server's `--timeout`) and `LIBPANDOC_WASM_MAX_MEMORY` (bytes, or
  with `k`, `m`, `g`, as pandoc's `+RTS -M`) stop a filter past them
  (none by default, as pandoc; `WasmFilter::timeout` and `::max_memory`
  for one filter). The time includes its calls to pandoc: each runs with
  the time the filter has left as its `"timeout"` (or the filter's own, if
  shorter), so one call on a slow document can't outlast it, and a filter
  whose time ran out in one is stopped. The memory is the filter's own:
  pandoc's, in its calls, is the process's, with no limit.

So a wasm filter can't reach your machine by itself
(`filters/src/bin/misbehave.rs` tries). **It can through the document it
returns**, which pandoc then writes with your access, as it writes any
document: a writer that embeds files (docx, ODT, EPUB, PPTX, HTML with
`--embed-resources`) reads the images the document names, citeproc run
after the filter reads the `bibliography` in its metadata, and a PDF
engine reads what raw LaTeX names. A filter given no directory at all can
name a file it couldn't open and find it in the output (`misbehave.rs`'s
`embed`, in `tests/wasm.rs`). So for a filter you don't trust, confine the
conversion too, as for a document you don't trust: the directory sandbox
above, where pandoc reads only what you listed (pandoc's own,
`"sandbox": true` or `--sandbox`, covers its writers and citeproc, not a
PDF engine). Then a filter from anywhere runs at the risk
of what it does to the document (raw HTML it adds runs in a reader's
browser) and of the directories you listed, not of your machine.

A filter that reads files reads what the document asks for, within its
directories (an include filter; `libpandoc::citeproc` on the metadata's
`bibliography`): when the document isn't trusted either, give the filter
no more than the document's own list (`LIBPANDOC_SANDBOX`).

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
not, even for a document it would call nothing for. A filter that doesn't
imports nothing and runs anywhere; one whose calls are optional can be
built twice, with a feature of its own around them.

The calls stay in the filter's sandbox: they run in pandoc's (readers read
no files: no LaTeX `\input`, no RST `include`), and options that would read
or write files, fetch resources or run programs (`filters`, `template`,
`output-file`, a Lua reader or writer, `pdf`, ...) are refused. The calls
are marked `"untrusted": true` and libpandoc checks them: one list for
every host. A filter can't name a cancel token (`"untrusted"` is written
over what it wrote); its calls stop with the conversion they run in.

**citeproc** (multibib and the like): its files are read by the filter,
with the access its host gave it, and given to pandoc as their contents.
`libpandoc::citeproc(doc, conversion)` does it
(`filters/src/bin/citeproc.rs`): it reads the files the metadata names (`bibliography`, a name or a list;
`csl` or `citation-style`; `citation-abbreviations`) and runs citeproc
with them as `"citeproc-files"` (libpandoc 1.14). A file it can't read
(outside its directories, missing, not UTF-8, a URL) is left out, and
pandoc reports it missing as it would. Compiled in, it is pandoc's
citeproc, and in a sandboxed conversion it reads only the files the
options name, not those the document names (as `read_as`: the filter
runs with the process's access). A filter in another language does the
same through `convert`: options `{"from": "json", "to": "json",
"citeproc": true, "citeproc-files": {"NAME": "TEXT", ...}}`, each file
under the name pandoc looks up: a bibliography as the metadata names it
(`refs.bib`, `../common/refs.bib`, an absolute path), `csl: style` as
`style.csl` and `citation-abbreviations: abbr` as `abbr.json` (a name
with a dot as it is), TEXT the file as a JSON string (UTF-8).
libpandoc's header has the rules (a dependent style's parent, LaTeX's
`\bibliography`); URLs are never fetched.

Rust has no stable ABI, so there are no native plugins (`.so` filters):
filters to distribute are wasm.

A filter in Rust, a wasm filter or a JSON filter is handed the whole
document as pandoc's JSON and hands it back, whatever it changes: on a 9.4
MB textbook one that does next to nothing adds 0.35 s (a wasm filter 0.4),
where a Lua filter, given elements as it asks for them, adds 0.09 s;
pandoc's own encoding and decoding of the JSON is most of it. Consecutive
filters in Rust and wasm share that cost ("Consecutive filters").

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

The crates' own code is GPL-2.0-or-later (`COPYING.md`), as pandoc's: they
link pandoc. What they load is libpandoc, under the GPL, version 2, as a
whole (pandoc and about 200 Haskell packages, GHC's runtime and a few C
libraries, each under its own licence). The crates carry none of it: the
library is installed (`LIBPANDOC_PREFIX`, conda) or downloaded by the
`download` feature, whose release archive is unpacked whole, its
`COPYING.md` and `THIRD-PARTY-NOTICES` beside `lib/`; libpandoc.wasm is
downloaded with `libpandoc-wasm-THIRD-PARTY-NOTICES.txt` beside it. A
program that ships libpandoc ships those with it.
