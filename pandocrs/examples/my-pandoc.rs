//! A pandoc with a Rust filter compiled in: `-F upper` runs it in process.
//!
//!     cargo run --example my-pandoc -- -F upper input.md
use panir::{Ctx, Inline, Typewise};

struct Upper;

impl panir::Filter for Upper {
    type Order = Typewise;
    fn inline(&mut self, x: &mut Inline, _: &mut Ctx<Typewise>) -> Option<Vec<Inline>> {
        if let Inline::Str(s) = x {
            *s = s.to_uppercase().into();
        }
        None
    }
}

fn main() {
    let args = std::env::args().skip(1).collect();
    let status = pandocrs::Pandocrs::new("my-pandoc")
        .filter("upper", || libpandoc::Filter::panir(Upper))
        .run(args);
    std::process::exit(status);
}
