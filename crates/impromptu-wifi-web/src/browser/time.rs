//! Timers on the browser's event loop.

use core::future::Future;

use embassy_futures::select::{select, Either};
use js_sys::Promise;

/// Wait `ms` milliseconds.
pub async fn sleep(ms: u32) {
    let promise = Promise::new(&mut |resolve, _reject| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                &resolve,
                ms.min(i32::MAX as u32) as i32,
            );
        }
    });
    let _ = promise.await;
}

/// Run `f` for at most `ms` milliseconds; `None` if the time ran out first.
pub async fn with_timeout<T>(ms: u32, f: impl Future<Output = T>) -> Option<T> {
    match select(f, sleep(ms)).await {
        Either::First(v) => Some(v),
        Either::Second(()) => None,
    }
}
