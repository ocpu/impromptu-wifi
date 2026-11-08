//! Notifications from browser event callbacks, awaited by the GATT client.

use core::cell::RefCell;
use core::future::poll_fn;
use core::task::{Poll, Waker};
use std::collections::VecDeque;

/// Something the device sent, or the connection ending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The current-state characteristic changed to this value.
    State(u8),
    /// The error-state characteristic changed to this value.
    Error(u8),
    /// The RPC result characteristic changed; read it for the full value.
    RpcResult,
    /// The connection ended.
    Disconnected,
}

/// A single-threaded event queue. Events wait in the queue until taken, so
/// a caller that stops waiting loses nothing.
#[derive(Debug, Default)]
pub struct NotificationQueue {
    inner: RefCell<(VecDeque<Event>, Option<Waker>)>,
}

impl NotificationQueue {
    /// Add an event and wake the waiting task.
    pub fn push(&self, e: Event) {
        let mut inner = self.inner.borrow_mut();
        inner.0.push_back(e);
        if let Some(w) = inner.1.take() {
            w.wake();
        }
    }

    /// Take the oldest event, waiting for one if the queue is empty.
    pub async fn next(&self) -> Event {
        poll_fn(|cx| {
            let mut inner = self.inner.borrow_mut();
            match inner.0.pop_front() {
                Some(e) => Poll::Ready(e),
                None => {
                    inner.1 = Some(cx.waker().clone());
                    Poll::Pending
                }
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::future::Future;
    use core::pin::pin;
    use std::sync::Arc;
    use std::task::{Context, Wake};

    struct Flag(std::sync::atomic::AtomicBool);
    impl Wake for Flag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[test]
    fn next_returns_in_order() {
        let q = NotificationQueue::default();
        q.push(Event::State(2));
        q.push(Event::RpcResult);
        assert_eq!(embassy_futures::block_on(q.next()), Event::State(2));
        assert_eq!(embassy_futures::block_on(q.next()), Event::RpcResult);
    }

    #[test]
    fn next_waits_and_is_woken_by_push() {
        let q = NotificationQueue::default();
        let flag = Arc::new(Flag(false.into()));
        let waker = Waker::from(flag.clone());
        let mut cx = Context::from_waker(&waker);
        let mut fut = pin!(q.next());
        assert!(fut.as_mut().poll(&mut cx).is_pending());
        q.push(Event::Disconnected);
        assert!(flag.0.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Event::Disconnected));
    }
}
