//! In a wasm filter, libpandoc is the program running the filter: pandocrs,
//! libpandoc.wasm's JavaScript, or any host that provides these imports
//! (module `libpandoc`):
//!
//! - `convert(options, options_len, input, input_len, has_input) -> status`
//! - `read_many(request, request_len) -> status`
//! - `query(query, query_len) -> status`
//! - `result_len(part) -> len`, `result_read(part, to)`: the last call's
//!   result, `part` 0 its output, 1 its log (JSON), 2 the error's kind and
//!   3 its message.
//!
//! Arguments are as libpandoc's C functions take them, the status 0 on
//! success. Hosts run these calls sandboxed (pandoc's `sandbox`), and turn
//! down options that would read or write files or run programs: the filter
//! gets no more access through pandoc than it has itself.
//!
//! A filter that makes none of these calls imports none of them, and runs
//! anywhere a WASI command does (`wasmtime run`).

use crate::{Error, Output, Result};

#[link(wasm_import_module = "libpandoc")]
extern "C" {
    #[link_name = "convert"]
    fn host_convert(
        options: *const u8,
        options_len: usize,
        input: *const u8,
        input_len: usize,
        has_input: i32,
    ) -> i32;
    #[link_name = "read_many"]
    fn host_read_many(request: *const u8, request_len: usize) -> i32;
    #[link_name = "query"]
    fn host_query(query: *const u8, query_len: usize) -> i32;
    #[link_name = "result_len"]
    fn result_len(part: i32) -> usize;
    #[link_name = "result_read"]
    fn result_read(part: i32, to: *mut u8);
}

fn part(i: i32) -> Vec<u8> {
    unsafe {
        let mut v = vec![0u8; result_len(i)];
        result_read(i, v.as_mut_ptr());
        v
    }
}

fn result(status: i32) -> Result<Output> {
    if status == 0 {
        Ok(Output {
            bytes: part(0),
            log: serde_json::from_slice(&part(1)).unwrap_or_default(),
        })
    } else {
        let text = |i| String::from_utf8_lossy(&part(i)).into_owned();
        Err(Error {
            kind: text(2),
            message: text(3),
        })
    }
}

pub(crate) fn convert(options: &str, input: Option<&[u8]>) -> Result<Output> {
    let (ip, il) = input.map_or((std::ptr::null(), 0), |b| (b.as_ptr(), b.len()));
    result(unsafe {
        host_convert(
            options.as_ptr(),
            options.len(),
            ip,
            il,
            input.is_some() as i32,
        )
    })
}

pub(crate) fn read_many(request: &str) -> Result<Output> {
    result(unsafe { host_read_many(request.as_ptr(), request.len()) })
}

pub(crate) fn query(query: &str) -> Result<Output> {
    result(unsafe { host_query(query.as_ptr(), query.len()) })
}
