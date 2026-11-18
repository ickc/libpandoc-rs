//! What a wasm filter sees of this process's environment. One test in its
//! own binary (its own process), as it sets the environment.
#![cfg(feature = "wasm")]

use std::path::PathBuf;
use std::process::Command;

use libpandoc::convert_with;
use libpandoc::wasm::{ENV_VAR, WasmFilter};
use serde_json::{Value, json};

/// The example filter `conversion`, built for wasm.
fn conversion() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../filters");
    let status = Command::new(env!("CARGO"))
        .args(["build", "--release", "--target", "wasm32-wasip1", "--quiet"])
        .args(["--bin", "conversion"])
        .current_dir(&root)
        .status()
        .expect("cargo");
    assert!(
        status.success(),
        "building the example filters (rustup target add wasm32-wasip1)"
    );
    root.join("target/wasm32-wasip1/release/conversion.wasm")
}

/// What the filter was told (`conversion`'s report) in a conversion with
/// these options.
fn told_in(f: WasmFilter, options: Value) -> Value {
    let out = convert_with(&options, Some(b"x"), vec![f.into_filter()]).unwrap();
    let doc: Value = serde_json::from_slice(&out.bytes).unwrap();
    serde_json::from_str(
        doc["blocks"].as_array().unwrap().last().unwrap()["c"][1]
            .as_str()
            .unwrap(),
    )
    .unwrap()
}

/// What the filter saw of the environment: the names of its variables
/// besides the host's own, and the values of those named
/// `LIBPANDOC_TEST_...`.
fn seen(f: WasmFilter) -> (Vec<String>, Value) {
    let told = told_in(f, json!({"to": "json"}));
    let names: Vec<String> = told["env"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap().to_owned())
        .collect();
    for own in OWN {
        assert_eq!(
            names.iter().filter(|n| *n == own).count(),
            1,
            "{own}: {names:?}"
        );
    }
    let others = names
        .into_iter()
        .filter(|n| !OWN.contains(&n.as_str()))
        .collect();
    (others, told["test-env"].clone())
}

/// The variables the host sets for a filter, in a conversion to JSON.
const OWN: [&str; 6] = [
    "PANDOC_INPUT_FORMAT",
    "PANDOC_OUTPUT_FORMAT",
    "PANDOC_READER_OPTIONS",
    "PANDOC_SANDBOX",
    "PANDOC_VERSION",
    "PANDOC_WRITER_OPTIONS",
];

/// An error's text.
fn error<E: std::fmt::Display>(r: Result<WasmFilter, E>) -> String {
    match r {
        Ok(_) => panic!("no error"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn a_wasm_filter_sees_only_what_it_is_given() {
    // SAFETY: the only test in this binary: no other thread reads the
    // environment meanwhile.
    let set = |k: &str, v: Option<&str>| unsafe {
        match v {
            Some(v) => std::env::set_var(k, v),
            None => std::env::remove_var(k),
        }
    };
    let path = conversion();
    let shown = path.display().to_string();
    let filter = || WasmFilter::from_file(&path).unwrap();
    let none: (Vec<String>, Value) = (vec![], json!({}));
    set(ENV_VAR, None);
    set("LIBPANDOC_TEST_A", Some("a"));
    set("LIBPANDOC_TEST_B", Some("b"));
    set("LIBPANDOC_TEST_SECRET", Some("canary"));
    set("LIBPANDOC_TEST_MISSING", None);

    // by default, none of the process's variables: the host's own only
    assert_eq!(seen(filter()), none);
    let names = told_in(filter(), json!({"to": "json"}))["env"].clone();
    assert_eq!(names, json!(OWN));

    // by name, with the process's values at the call; one it lacks is
    // left out
    let f = filter()
        .pass_env(["LIBPANDOC_TEST_A", "LIBPANDOC_TEST_MISSING"])
        .unwrap();
    set("LIBPANDOC_TEST_A", Some("changed"));
    assert_eq!(
        seen(f),
        (
            vec!["LIBPANDOC_TEST_A".into()],
            json!({"LIBPANDOC_TEST_A": "a"})
        )
    );
    set("LIBPANDOC_TEST_A", Some("a"));
    // with values; calls add up, a later one for a name replacing it
    let f = filter()
        .envs([("LIBPANDOC_TEST_B", "given"), ("LIBPANDOC_TEST_C", "c")])
        .unwrap()
        .pass_env(["LIBPANDOC_TEST_A"])
        .unwrap()
        .envs([("LIBPANDOC_TEST_C", "again")])
        .unwrap();
    assert_eq!(
        seen(f).1,
        json!({"LIBPANDOC_TEST_A": "a", "LIBPANDOC_TEST_B": "given", "LIBPANDOC_TEST_C": "again"})
    );
    // the host's own win
    let f = filter()
        .envs([("PANDOC_VERSION", "0"), ("PANDOC_SANDBOX", "yes")])
        .unwrap();
    let told = told_in(f, json!({"to": "json"}));
    assert_eq!(told["pandoc-version"], libpandoc::pandoc_version().unwrap());
    assert_eq!(told["sandbox"], "false");
    assert_eq!(told["env"], json!(OWN));

    // the variable, read when the filter is made: several names, empty
    // entries skipped, one the process lacks left out
    set(
        ENV_VAR,
        Some(",LIBPANDOC_TEST_A,,LIBPANDOC_TEST_MISSING,LIBPANDOC_TEST_B,"),
    );
    let f = filter();
    set("LIBPANDOC_TEST_A", Some("changed"));
    set("LIBPANDOC_TEST_MISSING", Some("late"));
    let both = (
        vec!["LIBPANDOC_TEST_A".into(), "LIBPANDOC_TEST_B".into()],
        json!({"LIBPANDOC_TEST_A": "a", "LIBPANDOC_TEST_B": "b"}),
    );
    assert_eq!(seen(f), both);
    set("LIBPANDOC_TEST_A", Some("a"));
    set("LIBPANDOC_TEST_MISSING", None);
    // a program's choice replaces it
    assert_eq!(seen(filter().no_env()), none);
    assert_eq!(seen(filter().pass_env::<&str>([]).unwrap()), none);
    let f = filter().pass_env(["LIBPANDOC_TEST_SECRET"]).unwrap();
    assert_eq!(seen(f).1, json!({"LIBPANDOC_TEST_SECRET": "canary"}));
    let f = filter().envs([("LIBPANDOC_TEST_C", "c")]).unwrap();
    assert_eq!(seen(f).1, json!({"LIBPANDOC_TEST_C": "c"}));
    // the host's own win over the variable's too
    set("PANDOC_SANDBOX", Some("yes"));
    set(ENV_VAR, Some("PANDOC_SANDBOX"));
    let told = told_in(filter(), json!({"to": "json"}));
    assert_eq!(
        (&told["sandbox"], &told["env"]),
        (&json!("false"), &json!(OWN))
    );
    set("PANDOC_SANDBOX", None);

    // in the directory sandbox, as anywhere: the conversion runs in
    // libpandoc.wasm, the filter in this process
    set(ENV_VAR, Some("LIBPANDOC_TEST_A"));
    let dir = std::env::temp_dir();
    let told = told_in(
        filter().envs([("LIBPANDOC_TEST_C", "c")]).unwrap(),
        json!({"to": "json", "sandbox": {"read": [dir], "timeout": 120}}),
    );
    assert_eq!(told["sandbox"], "true");
    assert_eq!(told["test-env"], json!({"LIBPANDOC_TEST_C": "c"}));
    let told = told_in(
        filter(),
        json!({"to": "json", "sandbox": {"read": [dir], "timeout": 120}}),
    );
    assert_eq!(told["test-env"], json!({"LIBPANDOC_TEST_A": "a"}));

    // what can't be a name is an error, naming the variable and the entry
    // (up to an `=`, past which may be a value)
    for (entry, says) in [
        ("LIBPANDOC_TEST_A,A B", r#""A B""#),
        (" LIBPANDOC_TEST_A", r#"" LIBPANDOC_TEST_A""#),
        ("A\tB", r#""A\tB""#),
        ("LIBPANDOC_TEST_A=canary,B", r#""LIBPANDOC_TEST_A=...""#),
        ("=canary", r#""=...""#),
    ] {
        set(ENV_VAR, Some(entry));
        let e = error(WasmFilter::from_file(&path));
        assert_eq!(
            e,
            format!("LIBPANDOC_WASM_ENV: not a variable's name: {says}")
        );
    }
    set(ENV_VAR, None);
    // and given by the program, naming the filter
    for name in ["A B", "", "A\0B", "LIBPANDOC_TEST_A=canary"] {
        let says = match name.split_once('=') {
            Some((n, _)) => format!("{n}=..."),
            None => name.into(),
        };
        let says = format!("{shown}: not a variable's name: {says:?}");
        assert_eq!(error(filter().pass_env([name])), says);
        assert_eq!(error(filter().envs([(name, "canary")])), says);
    }
    assert_eq!(
        error(filter().envs([("LIBPANDOC_TEST_C", "can\0ary")])),
        format!("{shown}: LIBPANDOC_TEST_C: its value has a NUL")
    );

    // a value that isn't UTF-8: an error naming the variable, not its value
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let bad = std::ffi::OsStr::from_bytes(b"canary\xff");
        // SAFETY: as `set`
        unsafe { std::env::set_var("LIBPANDOC_TEST_BAD", bad) };
        assert_eq!(
            error(filter().pass_env(["LIBPANDOC_TEST_BAD"])),
            format!("{shown}: LIBPANDOC_TEST_BAD: its value is not UTF-8")
        );
        set(ENV_VAR, Some("LIBPANDOC_TEST_BAD"));
        assert_eq!(
            error(WasmFilter::from_file(&path)),
            "LIBPANDOC_WASM_ENV: LIBPANDOC_TEST_BAD: its value is not UTF-8"
        );
        set(ENV_VAR, None);
        set("LIBPANDOC_TEST_BAD", None);
    }
}
