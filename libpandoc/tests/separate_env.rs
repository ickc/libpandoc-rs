//! `LIBPANDOC_SEPARATE_FILTERS`: the default of `"separate-filters"`. One
//! test in its own binary (its own process), as it sets the environment.

use libpandoc::{Filter, SEPARATE_FILTERS_VAR, convert_with};
use panir::Block;
use serde_json::{Value, json};

/// Whether two filters on the document were one exchange: the first nests
/// the document deeper than panir reads, which the second undoes.
fn together(options: Value) -> libpandoc::Result<bool> {
    let filters = vec![
        Filter::func(|doc, _| {
            for _ in 0..panir::MAX_DEPTH + 10 {
                doc.blocks = vec![Block::BlockQuote(std::mem::take(&mut doc.blocks))];
            }
            Ok(())
        }),
        Filter::func(|doc, _| {
            while let [Block::BlockQuote(_)] = &doc.blocks[..] {
                if let Some(Block::BlockQuote(b)) = doc.blocks.pop() {
                    doc.blocks = b;
                }
            }
            Ok(())
        }),
    ];
    match convert_with(&options, Some(b"x"), filters) {
        Ok(_) => Ok(true),
        Err(e) if e.message.contains("levels deep") => Ok(false),
        Err(e) => Err(e),
    }
}

#[test]
fn the_variable_is_the_default_and_the_options_win() {
    let set = |v: Option<&str>| {
        // SAFETY: the only test in this binary: no other thread reads the
        // environment meanwhile.
        unsafe {
            match v {
                Some(v) => std::env::set_var(SEPARATE_FILTERS_VAR, v),
                None => std::env::remove_var(SEPARATE_FILTERS_VAR),
            }
        }
    };
    let plain = json!({"to": "plain"});
    let given = |separate: bool| json!({"to": "plain", "separate-filters": separate});
    for v in [None, Some(""), Some("0")] {
        set(v);
        assert!(together(plain.clone()).unwrap(), "{v:?}");
        assert!(!together(given(true)).unwrap(), "{v:?}");
    }
    set(Some("1"));
    assert!(!together(plain.clone()).unwrap());
    assert!(together(given(false)).unwrap());
    for v in ["yes", "true", " 1", "2"] {
        set(Some(v));
        let e = together(plain.clone()).unwrap_err();
        assert_eq!(e.kind, "PandocOptionError");
        assert!(
            e.message.contains(SEPARATE_FILTERS_VAR) && e.message.contains(&format!("{v:?}")),
            "{e}"
        );
        assert!(together(given(false)).unwrap());
    }
    set(None);
}
