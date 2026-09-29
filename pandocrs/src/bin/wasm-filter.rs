//! `wasm-filter FILTER.wasm FORMAT`: a wasm filter as an ordinary JSON
//! filter, for pandoc itself (`pandoc -F NAME`, with NAME a script
//! `exec wasm-filter "$0.wasm" "$@"`). As `wasmtime run`, but a filter that
//! calls pandoc (`libpandoc::read` and the like) gets this program's.
//! The filter sees the current directory, read-only.
use std::io::{Read, Write};
use std::process::ExitCode;

use libpandoc::wasm::WasmFilter;
use libpandoc::Conversion;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: wasm-filter FILTER.wasm [FORMAT]");
        return ExitCode::from(2);
    };
    let conversion = Conversion::from_parts(args.next(), |k| std::env::var(k).ok());
    let run = || -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        let mut doc = Vec::new();
        std::io::stdin().read_to_end(&mut doc)?;
        WasmFilter::from_file(&path)?.run(&doc, &conversion)
    };
    match run() {
        Ok(out) => {
            std::io::stdout().write_all(&out).ok();
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("wasm-filter: {e}");
            ExitCode::FAILURE
        }
    }
}
