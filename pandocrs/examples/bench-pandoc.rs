//! pandoc with the example filters compiled in (`-F upper`, `-F modify`,
//! `-F count`, `-F identity`): the benchmark's native Rust in process.
use libpandoc::Filter;
use libpandoc_example_filters::{Count, Modify, Upper};

fn main() {
    let args = std::env::args().skip(1).collect();
    let status = pandocrs::Pandocrs::new("bench-pandoc")
        .filter("upper", || Filter::panir(Upper))
        .filter("modify", || Filter::panir(Modify))
        .filter("count", || Filter::panir(Count::default()))
        .filter("identity", || Filter::raw(|doc, _| Ok(doc.to_vec())))
        .run(args);
    std::process::exit(status);
}
