//! Raw bindings to libpandoc's C ABI (`libpandoc.h`, ABI 1.5): pandoc as a
//! C library. See the `libpandoc` crate for a safe API.
//!
//! All strings are UTF-8 and passed with their lengths; results are owned by
//! the caller and released with [`pandoc_result_free`].
#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_void};

pub const LIBPANDOC_ABI_VERSION_MAJOR: c_int = 1;
pub const LIBPANDOC_ABI_VERSION_MINOR: c_int = 5;
pub const LIBPANDOC_ABI_VERSION: c_int =
    LIBPANDOC_ABI_VERSION_MAJOR * 1000 + LIBPANDOC_ABI_VERSION_MINOR;

#[repr(C)]
pub struct pandoc_result {
    /// 0 on success.
    pub status: c_int,
    /// Output bytes, followed by a NUL not counted in `output_len`.
    pub output: *mut c_char,
    pub output_len: usize,
    /// On failure: pandoc's error constructor, or "Exception"; NUL-terminated.
    pub error_kind: *mut c_char,
    pub error_message: *mut c_char,
    /// A JSON array of pandoc's log messages; never NULL.
    pub log: *mut c_char,
}

/// Where a filter puts its answer ([`pandoc_buffer_set`]).
#[repr(C)]
pub struct pandoc_buffer {
    _private: [u8; 0],
}

/// A filter implemented by the caller: gets the document and a context
/// (both JSON, not NUL-terminated), returns 0 with the new document in
/// `out`, or nonzero with an error message in `out`.
pub type pandoc_filter_fn = Option<
    unsafe extern "C" fn(
        userdata: *mut c_void,
        doc: *const c_char,
        doc_len: usize,
        context: *const c_char,
        context_len: usize,
        out: *mut pandoc_buffer,
    ) -> c_int,
>;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct pandoc_filter {
    pub fn_: pandoc_filter_fn,
    pub userdata: *mut c_void,
}

#[cfg_attr(windows, link(name = "pandoc", kind = "raw-dylib"))]
extern "C" {
    pub fn pandoc_set_num_threads(n: c_int) -> c_int;
    pub fn pandoc_init() -> c_int;
    pub fn pandoc_shutdown();
    pub fn pandoc_abi_version() -> c_int;
    pub fn pandoc_convert(
        options: *const c_char,
        options_len: usize,
        input: *const c_char,
        input_len: usize,
    ) -> *mut pandoc_result;
    pub fn pandoc_convert_args(
        argc: c_int,
        argv: *const *const c_char,
        input: *const c_char,
        input_len: usize,
    ) -> *mut pandoc_result;
    pub fn pandoc_buffer_set(out: *mut pandoc_buffer, data: *const c_char, len: usize);
    pub fn pandoc_convert_filters(
        options: *const c_char,
        options_len: usize,
        input: *const c_char,
        input_len: usize,
        filters: *const pandoc_filter,
        filters_len: usize,
    ) -> *mut pandoc_result;
    pub fn pandoc_convert_args_filters(
        argc: c_int,
        argv: *const *const c_char,
        input: *const c_char,
        input_len: usize,
        filters: *const pandoc_filter,
        filters_len: usize,
    ) -> *mut pandoc_result;
    pub fn pandoc_main(
        argc: c_int,
        argv: *const *const c_char,
        filters_json: *const c_char,
        filters_json_len: usize,
        filters: *const pandoc_filter,
        filters_len: usize,
    ) -> c_int;
    pub fn pandoc_read_many(request: *const c_char, request_len: usize) -> *mut pandoc_result;
    pub fn pandoc_query(query: *const c_char, query_len: usize) -> *mut pandoc_result;
    pub fn pandoc_result_free(result: *mut pandoc_result);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_version() {
        let v = unsafe { pandoc_abi_version() };
        assert_eq!(v / 1000, LIBPANDOC_ABI_VERSION_MAJOR);
        assert!(v % 1000 >= LIBPANDOC_ABI_VERSION_MINOR);
    }

    #[test]
    fn convert() {
        let opts = br#"{"from":"markdown","to":"html"}"#;
        let input = b"*hi*";
        unsafe {
            let r = pandoc_convert(
                opts.as_ptr().cast(),
                opts.len(),
                input.as_ptr().cast(),
                input.len(),
            );
            assert_eq!((*r).status, 0);
            let out = std::slice::from_raw_parts((*r).output.cast::<u8>(), (*r).output_len);
            assert_eq!(out, b"<p><em>hi</em></p>\n");
            pandoc_result_free(r);
        }
    }
}
