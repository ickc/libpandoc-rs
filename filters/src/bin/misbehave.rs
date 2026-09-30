//! A filter that misbehaves, as a code block of the document asks, to show
//! a wasm filter's limits: `spin` runs forever (stopped by a timeout),
//! `hog` takes memory without end (stopped by a memory limit), `write`
//! writes a file in the current directory (which a wasm filter sees
//! read-only; exits with status 3 when it can't).
use panir::{Block, CodeBlock, Ctx, Filter, Typewise};

struct Misbehave;

impl Filter for Misbehave {
    type Order = Typewise;

    fn block(&mut self, b: &mut Block, _: &mut Ctx<Typewise>) -> Option<Vec<Block>> {
        if let Block::CodeBlock(CodeBlock { attr, text }) = b {
            if attr.classes.iter().any(|c| c == "spin") {
                let mut n: u64 = 0;
                while std::hint::black_box(n) != u64::MAX {
                    n = n.wrapping_add(1) % (u64::MAX - 1);
                }
            }
            if attr.classes.iter().any(|c| c == "hog") {
                let mut held: Vec<Vec<u8>> = Vec::new();
                loop {
                    held.push(std::hint::black_box(vec![1; 1 << 20]));
                }
            }
            if attr.classes.iter().any(|c| c == "write") {
                if let Err(e) = std::fs::write(text.trim(), "written by a wasm filter") {
                    eprintln!("misbehave: {}: {e}", text.trim());
                    std::process::exit(3);
                }
            }
        }
        None
    }
}

fn main() {
    panir::filter_with(|doc, conversion| panir::apply_with(doc, &mut Misbehave, conversion));
}
