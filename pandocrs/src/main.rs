//! `pandocrs ARGS`: pandoc, in process, running `-F name.wasm` filters
//! itself.

fn main() {
    let args = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let status = pandocrs::Pandocrs::new("pandocrs").run(args);
    std::process::exit(status);
}
