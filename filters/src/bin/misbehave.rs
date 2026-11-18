//! A filter that misbehaves, as a code block of the document asks, to show
//! a wasm filter's limits: `spin` runs forever (stopped by a timeout),
//! `hog` takes memory without end (stopped by a memory limit), `write`
//! writes a file in the current directory (which a wasm filter sees
//! read-only; exits with status 3 when it can't), `embed` becomes an image
//! of the file it names, which the filter itself never opens: pandoc does,
//! when it writes the document (what the filter's own limits don't cover).
use panir::{Attr, Block, CodeBlock, Ctx, Filter, Image, Inline, Target};

struct Misbehave;

impl Filter for Misbehave {
    fn block(&mut self, b: &mut Block, _: &mut Ctx) -> Option<Vec<Block>> {
        if let Block::CodeBlock(c) = b {
            let CodeBlock { attr, text } = &mut **c;
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
            if attr.classes.iter().any(|c| c == "write")
                && let Err(e) = std::fs::write(text.trim(), "written by a wasm filter")
            {
                eprintln!("misbehave: {}: {e}", text.trim());
                std::process::exit(3);
            }
            if attr.classes.iter().any(|c| c == "embed") {
                let image = Image {
                    attr: Attr::default(),
                    content: Vec::new(),
                    target: Target {
                        url: text.trim().into(),
                        title: "".into(),
                    },
                };
                return Some(vec![Block::Para(vec![Inline::Image(Box::new(image))])]);
            }
        }
        None
    }
}

fn main() {
    panir::filter_with(|doc, conversion| panir::apply_with(doc, &mut Misbehave, conversion));
}
