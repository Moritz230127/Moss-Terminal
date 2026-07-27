//! C ABI surface for embedding the Moss engine into the kitty process.
//!
//! Contract:
//! - `moss_init()` must be called once before anything else (idempotent).
//! - All entry points are panic-safe: unwinding never crosses the boundary.
//! - Text pointers are UTF-8 byte slices (not NUL-terminated) with explicit
//!   lengths; invalid UTF-8 is replaced lossily.
//! - `moss_poll_output` / `moss_stream_state` implement the drain protocol:
//!   the stream for a window is finished when state == 0 AND a poll returns 0.
//!
//! The consumer is kitty's moss_integration.py (ctypes) plus moss_hook.c
//! (dlopen), both part of the Moss Terminal patch set.

pub mod context;
pub mod output;
pub mod runtime;

use runtime::EngineRuntime;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, LazyLock, Mutex};

static ENGINE: LazyLock<Mutex<Option<Arc<EngineRuntime>>>> = LazyLock::new(|| Mutex::new(None));

fn engine() -> Option<Arc<EngineRuntime>> {
    ENGINE.lock().ok().and_then(|g| g.clone())
}

fn text_from_raw(ptr: *const u8, len: usize) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    Some(String::from_utf8_lossy(slice).into_owned())
}

/// Wrap an FFI body so panics become an error code instead of UB.
fn guarded<T>(default: T, body: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("moss: internal panic caught at FFI boundary");
            default
        }
    }
}

/// Initialize the embedded engine. Returns 0 on success (including when
/// already initialized), negative on failure.
#[no_mangle]
pub extern "C" fn moss_init() -> i32 {
    guarded(-99, || {
        let mut slot = match ENGINE.lock() {
            Ok(s) => s,
            Err(_) => return -98,
        };
        if slot.is_some() {
            return 0;
        }
        match EngineRuntime::create() {
            Ok(rt) => {
                *slot = Some(rt);
                0
            }
            Err(err) => {
                eprintln!("moss: init failed: {err:#}");
                -1
            }
        }
    })
}

/// Shut down the engine and drop all sessions. Safe to call multiple times.
#[no_mangle]
pub extern "C" fn moss_shutdown() {
    guarded((), || {
        let taken = ENGINE.lock().ok().and_then(|mut s| s.take());
        if let Some(rt) = taken {
            // Sessions hold no OS resources beyond SQLite handles closed on
            // drop; give in-flight tasks a brief grace period.
            match Arc::try_unwrap(rt) {
                Ok(owned) => {
                    owned
                        .runtime
                        .shutdown_timeout(std::time::Duration::from_millis(800));
                }
                Err(shared) => {
                    // A streaming task still holds the runtime; detach.
                    drop(shared);
                }
            }
        }
    })
}

/// Feed one completed terminal line. `kind` is the OSC 133 prompt kind of
/// the row (0 unknown / 1 prompt / 2 secondary prompt / 3 output).
#[no_mangle]
pub extern "C" fn moss_on_line(window_id: u64, kind: u8, text: *const u8, len: usize) {
    guarded((), || {
        if let (Some(rt), Some(text)) = (engine(), text_from_raw(text, len)) {
            rt.on_line(window_id, kind, &text);
        }
    })
}

/// Forward an OSC 133 event: mark is 'A', 'C' or 'D'; data carries the
/// cmdline (C) or exit status (D), possibly empty.
#[no_mangle]
pub extern "C" fn moss_on_prompt_mark(window_id: u64, mark: u8, data: *const u8, len: usize) {
    guarded((), || {
        if let Some(rt) = engine() {
            let data = text_from_raw(data, len).unwrap_or_default();
            rt.on_prompt_mark(window_id, mark, &data);
        }
    })
}

/// Update the window's working directory (from OSC 7).
#[no_mangle]
pub extern "C" fn moss_on_cwd(window_id: u64, text: *const u8, len: usize) {
    guarded((), || {
        if let (Some(rt), Some(cwd)) = (engine(), text_from_raw(text, len)) {
            rt.on_cwd(window_id, &cwd);
        }
    })
}

/// Start answering a question for a window.
/// Returns 0 ok, -1 engine not initialized, -2 busy, -3 empty question.
#[no_mangle]
pub extern "C" fn moss_ask(window_id: u64, text: *const u8, len: usize) -> i32 {
    guarded(-99, || {
        let Some(rt) = engine() else { return -1 };
        let Some(question) = text_from_raw(text, len) else {
            return -3;
        };
        rt.ask(window_id, &question)
    })
}

/// Re-enable (1) or mute (0) terminal-line capture for a window. The kitty
/// injector calls this with 1 once the streamed answer is fully written to
/// the screen; capture is muted automatically when an ask starts.
#[no_mangle]
pub extern "C" fn moss_set_capture(window_id: u64, enabled: i32) {
    guarded((), || {
        if let Some(rt) = engine() {
            rt.set_capture(window_id, enabled != 0);
        }
    })
}

/// Request cancellation of the in-flight ask for a window.
#[no_mangle]
pub extern "C" fn moss_cancel(window_id: u64) {
    guarded((), || {
        if let Some(rt) = engine() {
            rt.cancel(window_id);
        }
    })
}

/// Drain pending styled output bytes for a window into `buf`.
/// Returns the number of bytes written (0 when nothing is pending).
///
/// # Safety
/// `buf` must be valid for writes of `cap` bytes for the duration of this
/// call (or `cap` may be 0, in which case `buf` is not accessed).
#[no_mangle]
pub unsafe extern "C" fn moss_poll_output(window_id: u64, buf: *mut u8, cap: usize) -> usize {
    guarded(0, || {
        if buf.is_null() || cap == 0 {
            return 0;
        }
        let Some(rt) = engine() else { return 0 };
        let Some(session) = rt.existing_session(window_id) else {
            return 0;
        };
        let dest = unsafe { std::slice::from_raw_parts_mut(buf, cap) };
        session.out.drain_into(dest)
    })
}

/// Stream state for a window: 1 while a request is active or output is
/// still buffered, 0 when idle.
#[no_mangle]
pub extern "C" fn moss_stream_state(window_id: u64) -> i32 {
    guarded(0, || {
        let Some(rt) = engine() else { return 0 };
        match rt.existing_session(window_id) {
            Some(session) => session.out.state(),
            None => 0,
        }
    })
}

/// Drop all per-window state when kitty closes a window.
#[no_mangle]
pub extern "C" fn moss_window_closed(window_id: u64) {
    guarded((), || {
        if let Some(rt) = engine() {
            rt.window_closed(window_id);
        }
    })
}

/// Re-read config.jsonc; idle sessions pick it up on their next ask.
/// Returns 0 on success.
#[no_mangle]
pub extern "C" fn moss_reload_config() -> i32 {
    guarded(-99, || {
        let Some(rt) = engine() else { return -1 };
        match rt.reload_config() {
            Ok(()) => 0,
            Err(err) => {
                eprintln!("moss: config reload failed: {err:#}");
                -1
            }
        }
    })
}

/// Engine version string (static storage, never freed).
#[no_mangle]
pub extern "C" fn moss_version() -> *const u8 {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr()
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_nul_terminated() {
        let ptr = super::moss_version();
        let s = unsafe { std::ffi::CStr::from_ptr(ptr as *const std::ffi::c_char) };
        assert!(!s.to_str().unwrap().is_empty());
    }

    #[test]
    fn uninitialized_engine_is_safe() {
        // No moss_init: every call must be a harmless no-op.
        super::moss_on_line(1, 3, b"hello".as_ptr(), 5);
        super::moss_cancel(1);
        super::moss_window_closed(1);
        assert_eq!(super::moss_stream_state(1), 0);
        let mut buf = [0u8; 8];
        assert_eq!(
            unsafe { super::moss_poll_output(1, buf.as_mut_ptr(), buf.len()) },
            0
        );
        assert_eq!(super::moss_ask(1, b"q".as_ptr(), 1), -1);
    }

    #[test]
    fn null_pointers_are_safe() {
        super::moss_on_line(1, 0, std::ptr::null(), 0);
        super::moss_on_cwd(1, std::ptr::null(), 0);
        assert_eq!(
            unsafe { super::moss_poll_output(1, std::ptr::null_mut(), 16) },
            0
        );
        // With no engine initialized the not-initialized code (-1) wins over
        // the null-text code (-3); either way the call must not crash.
        assert_eq!(super::moss_ask(1, std::ptr::null(), 4), -1);
    }
}
