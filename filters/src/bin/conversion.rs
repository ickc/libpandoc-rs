//! Put what the filter was told about the conversion at the document's
//! end, as a code block of JSON: for tests.
use panir::{Attr, CodeBlock};
use serde_json::json;

fn main() {
    panir::filter_with(|doc, c| {
        let told = json!({
            "format": c.format,
            "input-format": c.input_format,
            "output-format": c.output_format,
            "reader-options": c.reader_options.is_some(),
            "pandoc-version": std::env::var("PANDOC_VERSION").ok(),
        });
        doc.blocks.push(
            CodeBlock {
                attr: Attr::default(),
                text: told.to_string(),
            }
            .into(),
        );
    });
}
