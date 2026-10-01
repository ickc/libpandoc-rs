//! For tests of a host's `libpandoc` imports: each code block with class
//! `call` holds a request, `{"convert": [options, input]}`,
//! `{"read_many": [inputs, options]}` or `{"query": [name, params]}`, and
//! gets the answer instead: `{"ok": ...}` or `{"error": [kind, message]}`.
use panir::{Block, CodeBlock, Ctx, Filter, Typewise};
use serde_json::{json, Value};

struct Calls;

fn call(req: &Value) -> Result<Value, libpandoc::Error> {
    if let Some([options, input]) = req
        .get("convert")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
    {
        let input = input.as_str().map(str::as_bytes);
        let out = libpandoc::convert(options, input)?;
        return Ok(Value::from(out.text()));
    }
    if let Some([inputs, options]) = req
        .get("read_many")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
    {
        let inputs: Vec<&str> = inputs
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let docs = libpandoc::read_many(&inputs, options)?;
        return Ok(docs
            .iter()
            .map(|d| serde_json::to_value(d).unwrap())
            .collect());
    }
    if let Some([name, params]) = req
        .get("query")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
    {
        return libpandoc::query(name.as_str().unwrap_or_default(), params.clone());
    }
    Ok(Value::Null)
}

impl Filter for Calls {
    type Order = Typewise;

    fn block(&mut self, b: &mut Block, _: &mut Ctx<Typewise>) -> Option<Vec<Block>> {
        if let Block::CodeBlock(c) = b {
            let CodeBlock { attr, text } = &mut **c;
            if attr.classes.iter().any(|c| c == "call") {
                let req: Value = serde_json::from_str(text).unwrap_or_default();
                *text = match call(&req) {
                    Ok(v) => json!({ "ok": v }),
                    Err(e) => json!({ "error": [e.kind, e.message] }),
                }
                .to_string()
                .into();
            }
        }
        None
    }
}

fn main() {
    libpandoc_example_filters::main(Calls);
}
