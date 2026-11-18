//! Cancelling calls while they run: [`CancelToken`] (libpandoc 1.13's
//! cancel tokens).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use libpandoc_sys as sys;

use crate::{Error, Result};

/// A way to stop calls while they run, from another thread (a cancel
/// button, Ctrl-C) or from a filter of the call: give its [`id`] as the
/// option `"cancel-token"` to [`convert`], [`convert_with`], [`read_many`]
/// or [`write_with`] (any number of calls, one after another or at once),
/// and [`cancel`] it. Each call using it then fails with an [`Error`] whose
/// [`Error::is_cancelled`], as one past its `"timeout"` fails with
/// [`Error::is_timeout`], and the process goes on. A token stays
/// cancelled: a call given it later fails at once.
///
/// It stops what `"timeout"` stops: pandoc's own code and the JSON filters
/// pandoc runs, and in the directory sandbox the whole of pandoc (Lua
/// included: the module is stopped from outside, by wasmtime's epoch); not
/// a filter in Rust running (it runs to its end, and the call then fails;
/// the calls it makes into pandoc on its thread stop too), nor Lua
/// outside the sandbox. A call that finished before the cancel keeps its
/// result. Untrusted options take it inside `"untrusted"`, which the host
/// writes, not beside it ([`untrusted`]; libpandoc 1.14).
///
/// It is `Send` and `Sync`; clones are the same token, freed when the last
/// is dropped (calls still using it go on, no longer cancellable).
///
/// ```
/// use serde_json::json;
///
/// let token = libpandoc::CancelToken::new().unwrap();
/// let t = token.clone();
/// std::thread::spawn(move || {
///     std::thread::sleep(std::time::Duration::from_millis(200));
///     t.cancel();
/// });
/// let slow = ">".repeat(100_000);
/// let e = libpandoc::convert(
///     &json!({"to": "html", "cancel-token": token.id()}),
///     Some(slow.as_bytes()),
/// )
/// .unwrap_err();
/// assert!(e.is_cancelled());
/// ```
///
/// **Ctrl-C:** the library handles no signal (a program keeps SIGINT's
/// default action, which ends it, as `pandocrs` does). A program that
/// would rather stop the call and go on cancels a token from a handler
/// that runs on a thread of its own, such as the `ctrlc` crate's (not a
/// raw signal handler: cancelling takes locks and allocates):
///
/// ```ignore
/// let token = libpandoc::CancelToken::new()?;
/// let t = token.clone();
/// ctrlc::set_handler(move || t.cancel())?;
/// match libpandoc::convert(&json!({"to": "html", "cancel-token": token.id()}), Some(input)) {
///     Err(e) if e.is_cancelled() => eprintln!("interrupted"),
///     r => { r?; }
/// }
/// ```
///
/// [`id`]: CancelToken::id
/// [`cancel`]: CancelToken::cancel
/// [`convert`]: crate::convert
/// [`convert_with`]: crate::convert_with
/// [`read_many`]: crate::read_many
/// [`write_with`]: crate::write_with
/// [`untrusted`]: crate::untrusted
#[derive(Clone, Debug)]
pub struct CancelToken(Arc<Token>);

#[derive(Debug)]
struct Token {
    id: i64,
    cancelled: Arc<AtomicBool>,
}

/// The live tokens' flags, by number: the directory sandbox, given a
/// number in the options, stops the module when its token is cancelled.
fn tokens() -> &'static Mutex<HashMap<i64, Arc<AtomicBool>>> {
    static TOKENS: OnceLock<Mutex<HashMap<i64, Arc<AtomicBool>>>> = OnceLock::new();
    TOKENS.get_or_init(Default::default)
}

impl CancelToken {
    /// A new token (libpandoc's `pandoc_cancel_token_new`).
    pub fn new() -> Result<Self> {
        crate::loaded()?;
        let id = sys::pandoc_cancel_token_new();
        if id <= 0 {
            return Err(Error::new(
                "PandocAppError",
                "no cancel token: pandoc's runtime isn't running",
            ));
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        tokens()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, cancelled.clone());
        Ok(CancelToken(Arc::new(Token { id, cancelled })))
    }

    /// Its number, the option `"cancel-token"`'s value.
    pub fn id(&self) -> i64 {
        self.0.id
    }

    /// Stop every call using this token, running or to come. From any
    /// thread (not from a raw signal handler: above).
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::SeqCst);
        sys::pandoc_cancel(self.0.id);
        #[cfg(feature = "wasm")]
        crate::wasm::interrupt();
    }

    /// Whether [`CancelToken::cancel`] was called.
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        tokens()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
        sys::pandoc_cancel_token_free(self.id);
    }
}

/// The flag of the token numbered `id`, if it is one this crate made and
/// it is alive.
#[cfg(feature = "wasm")]
pub(crate) fn flag(id: i64) -> Option<Arc<AtomicBool>> {
    tokens()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&id)
        .cloned()
}
