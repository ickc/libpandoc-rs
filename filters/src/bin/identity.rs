//! The document unchanged, not parsed: the cost of running a filter at all.
use std::io::{copy, stdin, stdout};

fn main() {
    copy(&mut stdin().lock(), &mut stdout().lock()).expect("copying stdin to stdout");
}
