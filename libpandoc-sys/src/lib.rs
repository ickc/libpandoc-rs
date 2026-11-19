//! Raw bindings to libpandoc's C ABI (`libpandoc.h`, ABI 1.15): pandoc as a
//! C library. See the `libpandoc` crate for a safe API.
//!
//! They are for a library of ABI 1.15 or a later 1.x
//! ([`LIBPANDOC_ABI_VERSION`]): a program using them checks
//! [`pandoc_abi_version`] before anything else, as `libpandoc.h` asks (the
//! `libpandoc` crate does, once).
//!
//! All strings are UTF-8 and passed with their lengths; results are owned by
//! the caller and released with [`pandoc_result_free`].
//!
//! The functions that take no pointers and have no precondition
//! ([`pandoc_abi_version`], [`pandoc_init`], [`pandoc_set_num_threads`],
//! and the cancel tokens' three) are safe to call; the others are `unsafe`, with what the caller must
//! uphold in their `# Safety` sections, from `libpandoc.h`.
#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_void};

pub const LIBPANDOC_ABI_VERSION_MAJOR: c_int = 1;
pub const LIBPANDOC_ABI_VERSION_MINOR: c_int = 15;
/// The ABI these bindings are for (major * 1000 + minor): a library works
/// with them if its [`pandoc_abi_version`] has the same major and a minor at
/// least as large.
pub const LIBPANDOC_ABI_VERSION: c_int =
    LIBPANDOC_ABI_VERSION_MAJOR * 1000 + LIBPANDOC_ABI_VERSION_MINOR;

#[repr(C)]
pub struct pandoc_result {
    /// 0 on success.
    pub status: c_int,
    /// Output bytes, followed by a NUL not counted in `output_len`.
    pub output: *mut c_char,
    pub output_len: usize,
    /// On failure: pandoc's error constructor, "Timeout" (a call stopped by
    /// its `"timeout"`), "Cancelled" (by its `"cancel-token"`, from 1.13),
    /// or "Exception"; NUL-terminated.
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
unsafe extern "C" {
    /// The number of threads pandoc grows to, the first time it runs
    /// something in parallel: at least 1, at most the number of processors.
    /// Before that growth it sets the number and returns it; after, it
    /// changes nothing and returns the number in effect (from 1.9). 0 if the
    /// runtime could not start, or after [`pandoc_shutdown`].
    ///
    /// Safe: it takes no pointer, starts the runtime itself
    /// ([`pandoc_init`]), and may be called from any thread, also from
    /// several at once (the calls take turns; from 1.8), at any time (it no
    /// longer changes a running runtime's threads; from 1.9).
    pub safe fn pandoc_set_num_threads(n: c_int) -> c_int;
    /// Start the Haskell runtime; 0 on success. Every other function calls
    /// it, so calling it is optional.
    ///
    /// Safe: it takes no pointer and may be called any number of times,
    /// from any thread: the start runs once (`pthread_once`,
    /// `InitOnceExecuteOnce` on Windows), and later calls only return its
    /// status (nonzero after [`pandoc_shutdown`], which the runtime can't
    /// come back from).
    pub safe fn pandoc_init() -> c_int;
    /// Stop the Haskell runtime. Optional.
    ///
    /// # Safety
    ///
    /// At most once, at the end of the process: no other libpandoc call may
    /// be running, on any thread (nor may this be called from inside a
    /// callback filter), and none may start while it runs. Calls after it
    /// fail (results are NULL) rather than restart the runtime.
    pub fn pandoc_shutdown();
    /// [`LIBPANDOC_ABI_VERSION`] of the loaded library (major * 1000 +
    /// minor): a program built against major.minor works with it if the
    /// major is the same and the minor at least as large.
    ///
    /// Safe: it returns a constant, and touches nothing else.
    pub safe fn pandoc_abi_version() -> c_int;
    /// Convert, as `pandoc` with a defaults file: `options` is a JSON object
    /// of defaults-file keys, `input` standard input (NULL: the options'
    /// input files, else the process's standard input). NULL only if the
    /// runtime could not start.
    ///
    /// # Safety
    ///
    /// `options` readable for `options_len` bytes; `input` NULL or readable
    /// for `input_len` bytes.
    pub fn pandoc_convert(
        options: *const c_char,
        options_len: usize,
        input: *const c_char,
        input_len: usize,
    ) -> *mut pandoc_result;
    /// Convert, as `pandoc ARGS` (`argv` without the program's name).
    ///
    /// # Safety
    ///
    /// `argv` holds `argc` pointers to NUL-terminated strings; `input` NULL
    /// or readable for `input_len` bytes.
    pub fn pandoc_convert_args(
        argc: c_int,
        argv: *const *const c_char,
        input: *const c_char,
        input_len: usize,
    ) -> *mut pandoc_result;
    /// Set a callback filter's answer (copied; the last call wins). A NULL
    /// `out` is ignored.
    ///
    /// # Safety
    ///
    /// `out` is NULL or the buffer libpandoc passed to the running callback;
    /// `data` readable for `len` bytes.
    pub fn pandoc_buffer_set(out: *mut pandoc_buffer, data: *const c_char, len: usize);
    /// [`pandoc_convert`], with callback filters the options name as
    /// `{"type": "callback", "index": i}`.
    ///
    /// # Safety
    ///
    /// As [`pandoc_convert`]; `filters` holds `filters_len` filters, each
    /// with a `fn_` (not `None`) that is sound to call with its `userdata`,
    /// on this thread, during the call.
    pub fn pandoc_convert_filters(
        options: *const c_char,
        options_len: usize,
        input: *const c_char,
        input_len: usize,
        filters: *const pandoc_filter,
        filters_len: usize,
    ) -> *mut pandoc_result;
    /// [`pandoc_convert_args`], with callback filters the arguments name as
    /// `--lua-filter=libpandoc:callback/i`.
    ///
    /// # Safety
    ///
    /// As [`pandoc_convert_args`] and, for `filters`,
    /// [`pandoc_convert_filters`].
    pub fn pandoc_convert_args_filters(
        argc: c_int,
        argv: *const *const c_char,
        input: *const c_char,
        input_len: usize,
        filters: *const pandoc_filter,
        filters_len: usize,
    ) -> *mut pandoc_result;
    /// The pandoc command, in this process (`argv[0]` the program's name);
    /// the exit status. `filters_json`, if not NULL, replaces the filters
    /// the arguments name (it may name the include step with its roots,
    /// `{"type": "panclude", "roots": [...]}`, from 1.15).
    ///
    /// # Safety
    ///
    /// `argv` holds `argc` pointers to NUL-terminated strings;
    /// `filters_json` NULL or readable for `filters_json_len` bytes;
    /// `filters` as for [`pandoc_convert_filters`]. pandoc reads and writes
    /// the process's standard streams, and a `pandoc lua` script may exit
    /// the process.
    pub fn pandoc_main(
        argc: c_int,
        argv: *const *const c_char,
        filters_json: *const c_char,
        filters_json_len: usize,
        filters: *const pandoc_filter,
        filters_len: usize,
    ) -> c_int;
    /// Read many texts at once (a JSON request `{"options": ...,
    /// "inputs": [...]}`).
    ///
    /// # Safety
    ///
    /// `request` readable for `request_len` bytes.
    pub fn pandoc_read_many(request: *const c_char, request_len: usize) -> *mut pandoc_result;
    /// Ask pandoc for information (a JSON object with a "query" key).
    ///
    /// # Safety
    ///
    /// `query` readable for `query_len` bytes.
    pub fn pandoc_query(query: *const c_char, query_len: usize) -> *mut pandoc_result;
    /// A cancel token (from 1.13): a number, from 1, never reused; 0 only
    /// when the runtime isn't running or memory ran out. A call takes it as
    /// the option `"cancel-token": N` (`pandoc_convert`'s options,
    /// `pandoc_read_many`'s `"options"`), or, for untrusted options, inside
    /// the `"untrusted"` the host writes, `"untrusted": {"cancel-token": N}`
    /// (from 1.14; a top-level one beside `"untrusted"` is refused);
    /// [`pandoc_cancel`] stops every call using it. Not in libpandoc.wasm.
    ///
    /// Safe: it takes no pointer, and may be called from any thread at any
    /// time (not from a signal handler: it takes locks and allocates).
    pub safe fn pandoc_cancel_token_new() -> i64;
    /// Cancel a token: every call using it, running or to come, fails with
    /// "Cancelled" (pandoc's own code stops where it allocates; a callback
    /// runs to its end; Lua isn't stopped). 0, or -1 for a number that is
    /// no token (then nothing happens).
    ///
    /// Safe: any number, from any thread, at any time, also while calls run
    /// and from inside a callback (not from a signal handler).
    pub safe fn pandoc_cancel(token: i64) -> c_int;
    /// Forget a token (calls using it go on, no longer cancellable through
    /// it); a number that is no token: nothing.
    ///
    /// Safe: as [`pandoc_cancel`].
    pub safe fn pandoc_cancel_token_free(token: i64);
    /// Free a result (NULL is ignored).
    ///
    /// # Safety
    ///
    /// `result` is NULL or a result libpandoc returned, not freed before
    /// and not used after.
    pub fn pandoc_result_free(result: *mut pandoc_result);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_version() {
        let v = pandoc_abi_version();
        assert_eq!(v / 1000, LIBPANDOC_ABI_VERSION_MAJOR);
        assert!(v % 1000 >= LIBPANDOC_ABI_VERSION_MINOR);
    }

    #[test]
    fn safe_functions() {
        // any number of times, from any thread, with any argument
        assert_eq!(pandoc_init(), 0);
        assert_eq!(std::thread::spawn(|| pandoc_init()).join().unwrap(), 0);
        // (the number is set only until pandoc's threads have grown, which
        // another test of this process may have caused: then the answer is
        // the number in effect)
        assert!(pandoc_set_num_threads(-1) >= 1);
        let n: Vec<_> = (0..4)
            .map(|i| std::thread::spawn(move || pandoc_set_num_threads(i)))
            .collect();
        assert!(n.into_iter().all(|n| n.join().unwrap() >= 1));
    }

    #[test]
    fn convert() {
        let opts = br#"{"from":"markdown","to":"html"}"#;
        let input = b"*hi*";
        // SAFETY: both buffers are given with their lengths and outlive the
        // call.
        let r = unsafe {
            pandoc_convert(
                opts.as_ptr().cast(),
                opts.len(),
                input.as_ptr().cast(),
                input.len(),
            )
        };
        assert!(!r.is_null());
        // SAFETY: a result from pandoc_convert, not yet freed; on success
        // `output` holds `output_len` bytes (libpandoc.h).
        let out = unsafe {
            assert_eq!((*r).status, 0);
            std::slice::from_raw_parts((*r).output.cast::<u8>(), (*r).output_len).to_vec()
        };
        assert_eq!(out, b"<p><em>hi</em></p>\n");
        // SAFETY: freed once, and `out` is a copy.
        unsafe { pandoc_result_free(r) };
    }
}
