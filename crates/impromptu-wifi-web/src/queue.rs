//! Notifications from browser event callbacks, awaited by the GATT client.

use core::cell::RefCell;
use core::future::poll_fn;
use core::task::{Poll, Waker};
use std::collections::VecDeque;

/// Something the device sent, or the connection ending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The current-state characteristic changed to this value.
    State(u8),
    /// The error-state characteristic changed to this value.
    Error(u8),
    /// The RPC result characteristic changed to this value, which may be
    /// cut to the MTU; see [`rpc_frame_complete`].
    RpcResult(Vec<u8>),
    /// The connection ended.
    Disconnected,
}

/// A single-threaded event queue. Events wait in the queue until taken, so
/// a caller that stops waiting loses nothing.
#[derive(Debug, Default)]
pub struct NotificationQueue {
    inner: RefCell<(VecDeque<Event>, Option<Waker>)>,
}

/// Whether `bytes` holds a whole RPC result frame (command, length, data,
/// checksum). A notification longer than the MTU arrives cut short.
pub fn rpc_frame_complete(bytes: &[u8]) -> bool {
    match bytes.get(1) {
        Some(&len) => bytes.len() >= usize::from(len) + 3,
        None => false,
    }
}

impl NotificationQueue {
    /// Remove the first queued RPC result equal to `bytes`.
    ///
    /// Reading a characteristic fires `characteristicvaluechanged` for the
    /// value read (Web Bluetooth, `readValue()`), so a read of the RPC
    /// result queues an echo of what was just read. Removing it keeps the
    /// echo from answering the next command.
    pub fn discard_echo(&self, bytes: &[u8]) {
        let mut inner = self.inner.borrow_mut();
        let echo = inner
            .0
            .iter()
            .position(|e| matches!(e, Event::RpcResult(v) if v.as_slice() == bytes));
        if let Some(i) = echo {
            inner.0.remove(i);
        }
    }

    /// Drop queued errors and RPC results, keeping state changes and a
    /// disconnect. Called before a command is sent: anything else in the
    /// queue answers an earlier command that timed out.
    pub fn drain_stale(&self) {
        self.inner
            .borrow_mut()
            .0
            .retain(|e| matches!(e, Event::State(_) | Event::Disconnected));
    }

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
        q.push(Event::RpcResult(vec![1]));
        assert_eq!(embassy_futures::block_on(q.next()), Event::State(2));
        assert_eq!(
            embassy_futures::block_on(q.next()),
            Event::RpcResult(vec![1])
        );
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

    fn drain(q: &NotificationQueue) -> Vec<Event> {
        q.inner.borrow_mut().0.drain(..).collect()
    }

    #[test]
    fn frames_are_complete_when_the_length_fits() {
        assert!(rpc_frame_complete(&[0x05, 0x02, b'a', b'b', 0x00]));
        assert!(!rpc_frame_complete(&[0x05, 0x05, b'a']));
        assert!(!rpc_frame_complete(&[0x05]));
        assert!(!rpc_frame_complete(&[]));
    }

    #[test]
    fn the_echo_of_a_read_is_discarded() {
        let q = NotificationQueue::default();
        q.push(Event::State(3));
        q.push(Event::RpcResult(vec![1, 2]));
        q.discard_echo(&[1, 2]);
        assert_eq!(drain(&q), [Event::State(3)]);
    }

    #[test]
    fn other_results_are_not_taken_for_an_echo() {
        let q = NotificationQueue::default();
        q.push(Event::RpcResult(vec![9]));
        q.discard_echo(&[1, 2]);
        assert_eq!(drain(&q), [Event::RpcResult(vec![9])]);
    }

    #[test]
    fn stale_answers_are_drained_but_state_and_disconnect_stay() {
        let q = NotificationQueue::default();
        q.push(Event::Error(3));
        q.push(Event::State(2));
        q.push(Event::RpcResult(vec![1]));
        q.push(Event::Disconnected);
        q.drain_stale();
        assert_eq!(drain(&q), [Event::State(2), Event::Disconnected]);
    }
}
