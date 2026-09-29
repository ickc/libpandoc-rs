//! Replace each code block with class `parse` by its text, read as the
//! document was (its input format and reader options): all of them in one
//! call to pandoc (`read_many`), as pantable reads table cells. As a wasm
//! filter, it calls the pandoc of the program running it.
use panir::{Block, CodeBlock, Ctx, Filter, Typewise};

fn is_parse(b: &Block) -> Option<&str> {
    match b {
        Block::CodeBlock(CodeBlock { attr, text }) if attr.classes.iter().any(|c| c == "parse") => {
            Some(text)
        }
        _ => None,
    }
}

/// The texts to read, in document order.
struct Collect(Vec<String>);

impl Filter for Collect {
    type Order = Typewise;

    fn block(&mut self, b: &mut Block, _: &mut Ctx<Typewise>) -> Option<Vec<Block>> {
        if let Some(t) = is_parse(b) {
            self.0.push(t.to_owned());
        }
        None
    }
}

/// Their blocks, in the same order.
struct Replace(std::vec::IntoIter<Vec<Block>>);

impl Filter for Replace {
    type Order = Typewise;

    fn block(&mut self, b: &mut Block, _: &mut Ctx<Typewise>) -> Option<Vec<Block>> {
        is_parse(b).and_then(|_| self.0.next())
    }
}

fn main() {
    panir::filter_with(|doc, conversion| {
        let mut texts = Collect(Vec::new());
        panir::apply_with(doc, &mut texts, conversion);
        if texts.0.is_empty() {
            return;
        }
        let docs = libpandoc::read_many_as(&texts.0, conversion).unwrap_or_else(|e| {
            eprintln!("parse: {e}");
            std::process::exit(1)
        });
        let blocks: Vec<Vec<Block>> = docs.into_iter().map(|d| d.blocks).collect();
        panir::apply_with(doc, &mut Replace(blocks.into_iter()), conversion);
    });
}
