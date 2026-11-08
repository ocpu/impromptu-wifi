//! The sans-IO client session.
//!
//! The protocol has no request ids, and error updates aren't tied to a
//! command, so a client can only have one command outstanding and has to
//! work out which incoming message answers it. These are the rules:
//!
//! * Every command is acknowledged with "no error". That's informational.
//! * Any other error while a command is outstanding is that command's failure.
//! * A result whose command id matches the outstanding command answers it.
//!   The one exception is "get current state" on serial: a provisioned
//!   device answers it with a *Wi-Fi settings* result carrying the URL.
//! * Serial scans answer with one result per network and finish with an
//!   empty result. BLE scans answer with a single result.
//! * Identify (BLE) has no response at all, so it never involves the
//!   session: the client is done once the write succeeds. Waiting for the
//!   "no error" acknowledgement would be unsound, because an acknowledgement
//!   can't be matched to its command and a stale one (for example the
//!   initial notification after subscribing) would complete it early.
//! * State updates are always recorded. "Get current state" completes on the
//!   state update unless the device is provisioned (then a result follows).

use impromptu_wifi_proto::{command_id, Command, ErrorCode, State, Transport};

/// A message from the device, reduced to what the session needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Event {
    /// A state update.
    State(State),
    /// An error update.
    Error(ErrorCode),
    /// A result frame for `command`; `empty` if it holds no strings.
    Result {
        /// The command id the result answers.
        command: u8,
        /// Whether the result's data is empty.
        empty: bool,
    },
}

/// What the caller should do after an [`Event`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Step {
    /// Not (yet) an answer; keep waiting.
    Continue,
    /// The result just received answers the outstanding command: decode it.
    /// If `last`, the command is complete.
    Result {
        /// Whether this is the final result for the command.
        last: bool,
    },
    /// The command completed without (further) results.
    Done,
    /// The device reported this error for the outstanding command.
    Failed(ErrorCode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    WifiSettings,
    GetState,
    Scan,
    Single,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pending {
    id: u8,
    kind: Kind,
}

/// Tracks the device's last reported state and error, and the outstanding command.
#[derive(Debug, Clone)]
pub struct Session {
    transport: Transport,
    state: Option<State>,
    error: ErrorCode,
    pending: Option<Pending>,
}

impl Session {
    /// A session for `transport`.
    pub const fn new(transport: Transport) -> Self {
        Self {
            transport,
            state: None,
            error: ErrorCode::None,
            pending: None,
        }
    }

    /// The device's last reported state, if any has been seen.
    pub fn state(&self) -> Option<State> {
        self.state
    }

    /// The device's last reported error.
    pub fn error(&self) -> ErrorCode {
        self.error
    }

    /// Whether a command is outstanding.
    pub fn is_busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Start waiting for the answer to `command`, replacing any outstanding
    /// one (for example after a caller timed out). Returns `false` if the
    /// command doesn't exist on this transport.
    pub fn begin(&mut self, command: &Command<'_>) -> bool {
        if !command.valid_for(self.transport) {
            return false;
        }
        let kind = match command {
            Command::WifiSettings { .. } => Kind::WifiSettings,
            Command::GetCurrentState => Kind::GetState,
            Command::ScanNetworks => Kind::Scan,
            _ => Kind::Single,
        };
        self.pending = Some(Pending {
            id: command.id(),
            kind,
        });
        true
    }

    /// Stop waiting for the outstanding command.
    pub fn abort(&mut self) {
        self.pending = None;
    }

    /// Process a message from the device.
    pub fn on_event(&mut self, event: Event) -> Step {
        match event {
            Event::State(state) => {
                self.state = Some(state);
                match self.pending {
                    Some(Pending {
                        kind: Kind::GetState,
                        ..
                    }) if state != State::Provisioned => self.finish(Step::Done),
                    _ => Step::Continue,
                }
            }

            Event::Error(error) => {
                self.error = error;
                match (error, self.pending) {
                    (ErrorCode::None, _) | (_, None) => Step::Continue,
                    (error, Some(_)) => self.finish(Step::Failed(error)),
                }
            }

            Event::Result { command, empty } => {
                let Some(p) = self.pending else {
                    return Step::Continue;
                };
                let answers = command == p.id
                    || (p.kind == Kind::GetState && command == command_id::WIFI_SETTINGS);
                if !answers {
                    return Step::Continue;
                }
                if p.kind == Kind::Scan && self.transport == Transport::Serial {
                    if empty {
                        self.finish(Step::Done)
                    } else {
                        Step::Result { last: false }
                    }
                } else {
                    self.finish(Step::Result { last: true })
                }
            }
        }
    }

    fn finish(&mut self, step: Step) -> Step {
        self.pending = None;
        step
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CREDS: Command<'static> = Command::WifiSettings {
        ssid: "home",
        password: "pw",
    };

    fn run(session: &mut Session, events: &[Event]) -> [Step; 8] {
        let mut out = [Step::Continue; 8];
        for (slot, ev) in out.iter_mut().zip(events) {
            *slot = session.on_event(*ev);
        }
        out
    }

    #[test]
    fn successful_provisioning() {
        let mut s = Session::new(Transport::Serial);
        assert!(s.begin(&CREDS));
        let steps = run(
            &mut s,
            &[
                Event::Error(ErrorCode::None),
                Event::State(State::Provisioning),
                Event::State(State::Provisioned),
                Event::Result {
                    command: 0x01,
                    empty: false,
                },
            ],
        );
        assert_eq!(
            steps[..4],
            [
                Step::Continue,
                Step::Continue,
                Step::Continue,
                Step::Result { last: true }
            ]
        );
        assert!(!s.is_busy());
        assert_eq!(s.state(), Some(State::Provisioned));
    }

    #[test]
    fn failed_provisioning() {
        let mut s = Session::new(Transport::Ble);
        s.begin(&CREDS);
        assert_eq!(
            s.on_event(Event::State(State::Provisioning)),
            Step::Continue
        );
        assert_eq!(
            s.on_event(Event::Error(ErrorCode::UnableToConnect)),
            Step::Failed(ErrorCode::UnableToConnect)
        );
        // The trailing state update is still recorded.
        assert_eq!(s.on_event(Event::State(State::Authorized)), Step::Continue);
        assert_eq!(s.state(), Some(State::Authorized));
    }

    #[test]
    fn serial_scan_ends_with_empty_result() {
        let mut s = Session::new(Transport::Serial);
        s.begin(&Command::ScanNetworks);
        assert_eq!(
            s.on_event(Event::Result {
                command: 0x04,
                empty: false
            }),
            Step::Result { last: false }
        );
        assert_eq!(
            s.on_event(Event::Result {
                command: 0x04,
                empty: false
            }),
            Step::Result { last: false }
        );
        assert_eq!(
            s.on_event(Event::Result {
                command: 0x04,
                empty: true
            }),
            Step::Done
        );
    }

    #[test]
    fn ble_scan_is_a_single_result() {
        let mut s = Session::new(Transport::Ble);
        s.begin(&Command::ScanNetworks);
        assert_eq!(
            s.on_event(Event::Result {
                command: 0x04,
                empty: false
            }),
            Step::Result { last: true }
        );
    }

    #[test]
    fn get_state_when_not_provisioned_completes_on_state() {
        let mut s = Session::new(Transport::Serial);
        s.begin(&Command::GetCurrentState);
        assert_eq!(s.on_event(Event::Error(ErrorCode::None)), Step::Continue);
        assert_eq!(s.on_event(Event::State(State::Authorized)), Step::Done);
    }

    #[test]
    fn get_state_when_provisioned_waits_for_wifi_result() {
        let mut s = Session::new(Transport::Serial);
        s.begin(&Command::GetCurrentState);
        assert_eq!(s.on_event(Event::State(State::Provisioned)), Step::Continue);
        assert_eq!(
            s.on_event(Event::Result {
                command: 0x01,
                empty: false
            }),
            Step::Result { last: true }
        );
    }

    #[test]
    fn acks_never_complete_a_command() {
        let mut s = Session::new(Transport::Ble);
        s.begin(&Command::DeviceInfo);
        assert_eq!(s.on_event(Event::Error(ErrorCode::None)), Step::Continue);
        assert!(s.is_busy());
    }

    #[test]
    fn unrelated_results_are_ignored() {
        let mut s = Session::new(Transport::Serial);
        s.begin(&Command::DeviceInfo);
        assert_eq!(
            s.on_event(Event::Result {
                command: 0x04,
                empty: true
            }),
            Step::Continue
        );
        assert_eq!(
            s.on_event(Event::Result {
                command: 0x03,
                empty: false
            }),
            Step::Result { last: true }
        );
    }

    #[test]
    fn wrong_transport_is_refused() {
        assert!(!Session::new(Transport::Serial).begin(&Command::Identify));
        assert!(!Session::new(Transport::Ble).begin(&Command::NetworkState));
    }

    #[test]
    fn errors_without_a_command_are_just_recorded() {
        let mut s = Session::new(Transport::Ble);
        assert_eq!(
            s.on_event(Event::Error(ErrorCode::NotAuthorized)),
            Step::Continue
        );
        assert_eq!(s.error(), ErrorCode::NotAuthorized);
    }
}
