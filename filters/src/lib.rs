//! The example filters, as panir filters: each is also a program (a JSON
//! filter, and a wasm one), and pandocrs's `bench-pandoc` example compiles
//! them in. `upper`, `modify` and `count` are the benchmark's workloads,
//! the same as its Lua, JavaScript and Python ones.

use panir::{Block, Ctx, Filter, Inline, Pandoc, Typewise};

/// Every Str upper-cased.
pub struct Upper;

impl Filter for Upper {
    type Order = Typewise;

    fn inline(&mut self, x: &mut Inline, _: &mut Ctx<Typewise>) -> Option<Vec<Inline>> {
        if let Inline::Str(s) = x {
            *s = s.to_uppercase();
        }
        None
    }
}

/// Headers demoted, link and image targets and code classes changed.
pub struct Modify;

impl Filter for Modify {
    type Order = Typewise;

    fn inline(&mut self, x: &mut Inline, _: &mut Ctx<Typewise>) -> Option<Vec<Inline>> {
        match x {
            Inline::Link(l) => l.target.url = format!("https://example.org/{}", l.target.url),
            Inline::Image(i) => {
                i.target.url = format!("img/{}", i.target.url);
                i.attr.attributes.push(("loading".into(), "lazy".into()));
            }
            _ => {}
        }
        None
    }

    fn block(&mut self, x: &mut Block, _: &mut Ctx<Typewise>) -> Option<Vec<Block>> {
        match x {
            Block::Header(h) => h.level += 1,
            Block::CodeBlock(c) => c.attr.classes.push("numbered".into()),
            _ => {}
        }
        None
    }
}

/// Every Str counted, and a paragraph with the count added at the end.
#[derive(Default)]
pub struct Count(pub usize);

impl Filter for Count {
    type Order = Typewise;

    fn inline(&mut self, x: &mut Inline, _: &mut Ctx<Typewise>) -> Option<Vec<Inline>> {
        if matches!(x, Inline::Str(_)) {
            self.0 += 1;
        }
        None
    }

    fn pandoc(&mut self, doc: &mut Pandoc, _: &mut Ctx<Typewise>) {
        doc.blocks
            .push(Block::Para(vec![Inline::Str(self.0.to_string())]));
    }
}

/// Run a filter as a JSON filter (and so as a wasm one).
pub fn main<F: Filter>(mut f: F) {
    panir::filter_with(|doc, conversion| panir::apply_with(doc, &mut f, conversion));
}
