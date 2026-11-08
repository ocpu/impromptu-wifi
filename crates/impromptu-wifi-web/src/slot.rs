//! One connection, used by one command at a time.

use core::cell::RefCell;

/// Why the value can't be taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// Another command holds it.
    Busy,
    /// It was cleared, for example after a disconnect.
    Gone,
}

enum State<T> {
    Ready(T),
    InUse,
    Gone,
}

/// Holds a value that a command takes out while it runs and puts back when
/// it is done. Taking it while it is out fails with
/// [`Unavailable::Busy`] instead of panicking, so a second click while a
/// command runs gets an answer.
pub struct Slot<T> {
    state: RefCell<State<T>>,
}

impl<T> Slot<T> {
    /// A slot holding `value`.
    pub fn new(value: T) -> Self {
        Self {
            state: RefCell::new(State::Ready(value)),
        }
    }

    /// Take the value out.
    pub fn take(&self) -> Result<T, Unavailable> {
        let mut state = self.state.borrow_mut();
        match core::mem::replace(&mut *state, State::InUse) {
            State::Ready(v) => Ok(v),
            State::InUse => Err(Unavailable::Busy),
            State::Gone => {
                *state = State::Gone;
                Err(Unavailable::Gone)
            }
        }
    }

    /// Put the value back. If the slot was cleared meanwhile, the value is
    /// returned, so the caller can close it.
    pub fn put(&self, value: T) -> Result<(), T> {
        let mut state = self.state.borrow_mut();
        if matches!(*state, State::InUse) {
            *state = State::Ready(value);
            Ok(())
        } else {
            Err(value)
        }
    }

    /// Drop the value for good; later takes fail with
    /// [`Unavailable::Gone`]. Returns the value if it was in the slot.
    pub fn clear(&self) -> Option<T> {
        match core::mem::replace(&mut *self.state.borrow_mut(), State::Gone) {
            State::Ready(v) => Some(v),
            State::InUse | State::Gone => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_take_is_busy_until_put() {
        let slot = Slot::new(7);
        let v = slot.take().unwrap();
        assert_eq!(slot.take(), Err(Unavailable::Busy));
        assert_eq!(slot.put(v), Ok(()));
        assert_eq!(slot.take(), Ok(7));
    }

    #[test]
    fn cleared_slot_is_gone_even_after_put() {
        let slot = Slot::new(7);
        let v = slot.take().unwrap();
        assert_eq!(slot.clear(), None);
        assert_eq!(slot.put(v), Err(7), "the value comes back to be closed");
        assert_eq!(slot.take(), Err(Unavailable::Gone));
    }
}
