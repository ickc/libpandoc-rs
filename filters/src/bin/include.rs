//! Replace a code block with class `include` by the file it names: shows
//! what a wasm filter may read (the current directory, by default).
use panir::{Block, CodeBlock, Ctx, Filter, Typewise};

struct Include;

impl Filter for Include {
    type Order = Typewise;

    fn block(&mut self, b: &mut Block, _: &mut Ctx<Typewise>) -> Option<Vec<Block>> {
        if let Block::CodeBlock(c) = b {
            let CodeBlock { attr, text } = &mut **c;
            if attr.classes.iter().any(|c| c == "include") {
                match std::fs::read_to_string(text.trim()) {
                    Ok(s) => *text = s.into(),
                    Err(e) => {
                        eprintln!("include: {}: {e}", text.trim());
                        std::process::exit(3);
                    }
                }
                attr.classes.retain(|c| c != "include");
            }
        }
        None
    }
}

fn main() {
    panir::filter_with(|doc, conversion| panir::apply_with(doc, &mut Include, conversion));
}
