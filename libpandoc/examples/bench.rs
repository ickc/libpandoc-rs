//! The benchmark, warm, in this process: one conversion (markdown to HTML)
//! with no filter, and with each workload as Lua, as Rust (panir, in
//! process), and as Rust compiled to wasm (wasmtime); the median ms of
//! each, and the peak memory, as JSON.
//!
//!     cargo run --release --features wasm --example bench -- DOC WASM_DIR LUA_DIR [RUNS]
use std::time::Instant;

use libpandoc::wasm::WasmFilter;
use libpandoc::{convert, convert_with, Filter};
use libpandoc_example_filters::{Count, Modify, Upper};
use serde_json::{json, Map, Value};

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

fn native(w: &str) -> Filter<'static> {
    match w {
        "upper" => Filter::panir(Upper),
        "modify" => Filter::panir(Modify),
        "count" => Filter::panir(Count::default()),
        _ => Filter::raw(|doc, _| Ok(doc.to_vec())),
    }
}

/// Peak resident memory of this process, in bytes (Linux).
fn peak_rss() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    Some(line.split_whitespace().nth(1)?.parse::<u64>().ok()? * 1024)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let doc = std::fs::read(&args[1]).unwrap();
    let wasm_dir = std::path::Path::new(&args[2]);
    let lua_dir = std::path::Path::new(&args[3]);
    let runs = args.get(4).map_or(7, |r| r.parse().unwrap());
    let opts = json!({"from": "markdown", "to": "html"});
    let mut r = Map::new();
    let mut put = |k: String, v: f64| {
        r.insert(k, v.into());
    };
    let t = Instant::now();
    libpandoc::pandoc_version().unwrap();
    put("load".into(), t.elapsed().as_secs_f64() * 1000.0);
    put(
        "none".into(),
        time(runs, || drop(convert(&opts, Some(&doc)).unwrap())),
    );
    let workloads = ["identity", "upper", "modify", "count"];
    for w in workloads {
        let lua = lua_dir.join(format!("{w}.lua"));
        let lua = lua.to_str().unwrap();
        put(
            format!("lua {w}"),
            time(runs, || {
                drop(convert_with(&opts, Some(&doc), vec![Filter::lua(lua)]).unwrap())
            }),
        );
        put(
            format!("rust {w}"),
            time(runs, || {
                drop(convert_with(&opts, Some(&doc), vec![native(w)]).unwrap())
            }),
        );
        let t = Instant::now();
        let wasm = WasmFilter::from_file(wasm_dir.join(format!("{w}.wasm"))).unwrap();
        put(
            format!("wasm compile {w}"),
            t.elapsed().as_secs_f64() * 1000.0,
        );
        put(
            format!("wasm {w}"),
            time(runs, || {
                drop(convert_with(&opts, Some(&doc), vec![wasm.clone().into_filter()]).unwrap())
            }),
        );
    }
    // the filters alone on the document's JSON: in wasm (a fresh instance per run, as in a conversion)
    let json = convert(&json!({"from": "markdown", "to": "json"}), Some(&doc))
        .unwrap()
        .bytes;
    let conversion = libpandoc::Conversion {
        format: Some("html".into()),
        ..Default::default()
    };
    for w in ["upper", "modify", "count"] {
        let wasm = WasmFilter::from_file(wasm_dir.join(format!("{w}.wasm"))).unwrap();
        put(
            format!("wasm alone {w}"),
            time(runs, || drop(wasm.run(&json, &conversion).unwrap())),
        );
    }
    let mut out = Value::Object(r);
    out["memory"] = json!({ "peak_rss": peak_rss() });
    println!("{out}");
}
