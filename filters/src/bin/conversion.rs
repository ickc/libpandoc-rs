//! Put what the filter was told about the conversion at the document's
//! end, as a code block of JSON: for tests. With what it sees of the
//! environment: `"env"`, the names of all its variables, sorted, and
//! `"test-env"`, the values of those named `LIBPANDOC_TEST_...` (a prefix
//! for tests: no other value is shown, so that a failing test never
//! prints a real one).
use panir::{Attr, CodeBlock};
use serde_json::{Map, Value, json};

/// The prefix of the variables whose values the filter shows.
const SHOWN: &str = "LIBPANDOC_TEST_";

fn main() {
    let mut names: Vec<String> = std::env::vars_os()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    names.sort();
    let shown: Map<String, Value> = std::env::vars_os()
        .filter_map(|(k, v)| {
            let k = k.into_string().ok()?;
            k.starts_with(SHOWN)
                .then(|| (k, v.to_string_lossy().into_owned().into()))
        })
        .collect();
    panir::filter_with(|doc, c| {
        let told = json!({
            "format": c.format,
            "input-format": c.input_format,
            "output-format": c.output_format,
            "reader-options": c.reader_options.is_some(),
            "number-sections": c.writer_options.as_ref().map(|w| &w["number-sections"]),
            "pandoc-version": std::env::var("PANDOC_VERSION").ok(),
            "sandbox": std::env::var("PANDOC_SANDBOX").ok(),
            "env": &names,
            "test-env": &shown,
        });
        doc.blocks.push(
            CodeBlock {
                attr: Attr::default(),
                text: told.to_string().into(),
            }
            .into(),
        );
    });
}
