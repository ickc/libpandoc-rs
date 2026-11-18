//! Run citeproc on the document (`libpandoc::citeproc`), as `--citeproc`
//! would: for tests of a host's `"citeproc-files"`. As a wasm filter, its
//! calls to pandoc read no file, so it reads the files the metadata names
//! (`bibliography`, `csl`, `citation-abbreviations`) itself, in the
//! directories its host gave it, and hands pandoc their contents: one
//! outside them is missing. Fails (status 1) with pandoc's error.
fn main() {
    panir::filter_with(|doc, conversion| {
        *doc = libpandoc::citeproc(doc, conversion)?;
        Ok::<(), libpandoc::Error>(())
    });
}
