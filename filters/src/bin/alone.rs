//! For the benchmark: the filters alone, on a document's JSON (read, filter,
//! write), timed in this process: `alone FILE [RUNS]` prints the median ms
//! of each. Built natively or for wasm (`wasmtime run --dir=. alone.wasm`).
use std::time::Instant;

use libpandoc_example_filters::{Count, Modify, Upper};
use panir::Filter;

fn time(runs: usize, mut f: impl FnMut()) -> f64 {
    f();
    let mut ts: Vec<f64> = (0..runs)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    ts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    ts[ts.len() / 2]
}

fn run<F: Filter>(json: &str, mut f: impl FnMut() -> F) -> String {
    let mut doc = panir::from_str(json).unwrap();
    panir::apply(&mut doc, &mut f(), Some("html"));
    panir::to_string(&doc)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let json = std::fs::read_to_string(&args[1]).unwrap();
    let runs = args.get(2).map_or(7, |r| r.parse().unwrap());
    let only = time(runs, || {
        let doc = panir::from_str(&json).unwrap();
        std::hint::black_box(panir::to_string(&doc));
    });
    println!("json only {only:.1}");
    println!("upper {:.1}", time(runs, || drop(run(&json, || Upper))));
    println!("modify {:.1}", time(runs, || drop(run(&json, || Modify))));
    println!(
        "count {:.1}",
        time(runs, || drop(run(&json, Count::default)))
    );
}
