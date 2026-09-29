//! Upper-case all text outside code.
use panir::{Ctx, Filter, Inline, Typewise};

struct Upper;

impl Filter for Upper {
    type Order = Typewise;

    fn inline(&mut self, x: &mut Inline, _: &mut Ctx<Typewise>) -> Option<Vec<Inline>> {
        if let Inline::Str(s) = x {
            *s = s.to_uppercase();
        }
        None
    }
}

fn main() {
    panir::filter_with(|doc, conversion| panir::apply_with(doc, &mut Upper, conversion));
}
