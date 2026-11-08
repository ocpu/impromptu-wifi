//! The sans-IO provisioning state machine.

use impromptu_wifi_proto::{is_valid_hostname, Command, ErrorCode, State, Transport};

/// What the driver should do in response to a command.
///
/// By the time a `Decision` is returned the machine has already updated its
/// own state and error; the driver performs the side effect and reports
/// outcomes back via [`Machine::on_connect_result`] or
/// [`Machine::on_rpc_error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Decision<'a> {
    /// The command was refused with this error (already recorded).
    Reject(ErrorCode),
    /// Try to join this network, then call [`Machine::on_connect_result`].
    Connect {
        /// Network name.
        ssid: &'a str,
        /// Passphrase.
        password: &'a str,
    },
    /// Re-send the current state; on serial, if provisioned, also re-send
    /// the Wi-Fi settings result.
    ReportState,
    /// Run the device's identify action.
    Identify,
    /// Send device information.
    DeviceInfo,
    /// Scan and send results.
    Scan,
    /// Send the hostname.
    GetHostname,
    /// Apply this (already validated) hostname, then send it.
    SetHostname(&'a str),
    /// Send the device name.
    GetDeviceName,
    /// Apply this device name, then send it.
    SetDeviceName(&'a str),
    /// Send serial network-state flags.
    NetworkState,
}

/// The provisioning state machine.
///
/// It owns the protocol's rules: which commands are allowed in which state,
/// when authorization applies, and what the client should be told. It also
/// tracks which state and error updates are still unsent; drain them with
/// [`take_error`](Self::take_error) and [`take_state`](Self::take_state).
///
/// # Authorization
///
/// With `require_auth`, every write (Wi-Fi settings, hostname, device name)
/// needs a physical authorization ([`authorize`](Self::authorize)) that has
/// not yet been [revoked](Self::revoke). This holds in every state,
/// including [`State::Provisioned`]: a provisioned device keeps reporting
/// `Provisioned` after a button press, and accepts new settings only until
/// the authorization is revoked.
#[derive(Debug, Clone)]
pub struct Machine {
    transport: Transport,
    state: State,
    error: ErrorCode,
    require_auth: bool,
    /// A write is allowed. Always `true` without `require_auth`; otherwise
    /// set by `authorize` and cleared by `revoke`, independent of `state`.
    authorized: bool,
    state_dirty: bool,
    error_dirty: bool,
}

impl Machine {
    /// A new machine. `require_auth` only has an effect on BLE, where the
    /// machine then starts in [`State::AuthorizationRequired`].
    pub fn new(transport: Transport, require_auth: bool) -> Self {
        let require_auth = require_auth && transport == Transport::Ble;
        Self {
            transport,
            state: Self::initial_state(require_auth),
            error: ErrorCode::None,
            require_auth,
            authorized: !require_auth,
            state_dirty: true,
            error_dirty: true,
        }
    }

    const fn initial_state(require_auth: bool) -> State {
        Self::idle_state(!require_auth)
    }

    /// The state to report when no provisioning is in progress and the
    /// device is not provisioned.
    const fn idle_state(authorized: bool) -> State {
        if authorized {
            State::Authorized
        } else {
            State::AuthorizationRequired
        }
    }

    /// The transport this machine speaks.
    pub fn transport(&self) -> Transport {
        self.transport
    }

    /// Current state.
    pub fn state(&self) -> State {
        self.state
    }

    /// Current error.
    pub fn error(&self) -> ErrorCode {
        self.error
    }

    /// The state, if it changed since last taken.
    pub fn take_state(&mut self) -> Option<State> {
        core::mem::take(&mut self.state_dirty).then_some(self.state)
    }

    /// The error, if it was set since last taken.
    pub fn take_error(&mut self) -> Option<ErrorCode> {
        core::mem::take(&mut self.error_dirty).then_some(self.error)
    }

    fn set_state(&mut self, state: State) {
        debug_assert!(state.valid_for(self.transport));
        if self.state != state {
            self.state = state;
            self.state_dirty = true;
        }
    }

    /// Errors are always re-sent, even if unchanged: the spec asks for an
    /// error update (to "no error") on every incoming RPC.
    fn set_error(&mut self, error: ErrorCode) {
        let error = if error.valid_for(self.transport) {
            error
        } else {
            ErrorCode::Unknown
        };
        self.error = error;
        self.error_dirty = true;
    }

    /// Whether a write would currently pass the authorization check.
    pub fn is_authorized(&self) -> bool {
        self.authorized
    }

    /// A physical authorization happened (BLE). Allows writes until
    /// [`revoke`](Self::revoke); reported as [`State::Authorized`] unless
    /// the device is provisioning or provisioned.
    pub fn authorize(&mut self) {
        self.authorized = true;
        if self.state == State::AuthorizationRequired {
            self.set_state(State::Authorized);
        }
    }

    /// The authorization timeout expired (BLE). The spec suggests about a
    /// minute; timing is the platform's job. Ends write access in every
    /// state, including [`State::Provisioned`].
    pub fn revoke(&mut self) {
        if !self.require_auth {
            return;
        }
        self.authorized = false;
        if self.state == State::Authorized {
            self.set_state(State::AuthorizationRequired);
        }
    }

    /// The device is already online (it was provisioned before the driver
    /// started). Moves an idle machine to [`State::Provisioned`]; does
    /// nothing mid-provisioning. Does not authorize: with `require_auth`,
    /// new settings still need [`authorize`](Self::authorize).
    pub fn mark_provisioned(&mut self) {
        if matches!(self.state, State::Authorized | State::AuthorizationRequired) {
            self.set_state(State::Provisioned);
        }
    }

    /// Mark provisioning unavailable or available again (serial only),
    /// for example when the Wi-Fi radio is switched off.
    pub fn set_available(&mut self, available: bool) {
        if self.transport != Transport::Serial {
            return;
        }
        match (available, self.state) {
            (false, _) => self.set_state(State::Stopped),
            (true, State::Stopped) => self.set_state(Self::idle_state(self.authorized)),
            _ => {}
        }
    }

    /// Decide what to do with a command.
    pub fn on_command<'a>(&mut self, command: Command<'a>) -> Decision<'a> {
        self.set_error(ErrorCode::None);
        let decision = match command {
            Command::WifiSettings { ssid, password } => match self.may_write() {
                Err(e) => Decision::Reject(e),
                Ok(()) => {
                    self.set_state(State::Provisioning);
                    Decision::Connect { ssid, password }
                }
            },
            Command::GetCurrentState => {
                self.state_dirty = true;
                Decision::ReportState
            }
            Command::Identify => Decision::Identify,
            Command::DeviceInfo => Decision::DeviceInfo,
            Command::ScanNetworks => Decision::Scan,
            Command::NetworkState => Decision::NetworkState,
            Command::Hostname(None) => Decision::GetHostname,
            Command::DeviceName(None) => Decision::GetDeviceName,
            Command::Hostname(Some(h)) => match self.may_write() {
                Err(e) => Decision::Reject(e),
                Ok(()) if !is_valid_hostname(h) => Decision::Reject(ErrorCode::BadHostname),
                Ok(()) => Decision::SetHostname(h),
            },
            Command::DeviceName(Some(n)) => match self.may_write() {
                Err(e) => Decision::Reject(e),
                Ok(()) => Decision::SetDeviceName(n),
            },
        };
        if let Decision::Reject(code) = decision {
            self.set_error(code);
        }
        decision
    }

    /// The single authorization gate for every write command.
    fn may_write(&self) -> Result<(), ErrorCode> {
        match self.state {
            State::Provisioning | State::Stopped => Err(ErrorCode::Unknown),
            _ if !self.authorized => Err(ErrorCode::NotAuthorized),
            State::Authorized | State::Provisioned => Ok(()),
            // Unreachable while `authorized` and the state agree; refuse
            // rather than trust the state alone.
            State::AuthorizationRequired => Err(ErrorCode::NotAuthorized),
        }
    }

    /// Report the outcome of a [`Decision::Connect`].
    pub fn on_connect_result(&mut self, connected: bool) {
        if self.state != State::Provisioning {
            return;
        }
        if connected {
            self.set_state(State::Provisioned);
        } else {
            self.set_error(ErrorCode::UnableToConnect);
            self.set_state(Self::idle_state(self.authorized));
        }
    }

    /// Record an error that arose outside [`on_command`](Self::on_command):
    /// a frame that failed to parse, or a side effect that failed.
    pub fn on_rpc_error(&mut self, error: ErrorCode) {
        self.set_error(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CREDS: Command<'static> = Command::WifiSettings {
        ssid: "net",
        password: "pw",
    };

    fn drain(m: &mut Machine) -> (Option<ErrorCode>, Option<State>) {
        (m.take_error(), m.take_state())
    }

    #[test]
    fn initial_state_is_reported_once() {
        let mut m = Machine::new(Transport::Serial, false);
        assert_eq!(
            drain(&mut m),
            (Some(ErrorCode::None), Some(State::Authorized))
        );
        assert_eq!(drain(&mut m), (None, None));
    }

    #[test]
    fn serial_ignores_require_auth() {
        let m = Machine::new(Transport::Serial, true);
        assert_eq!(m.state(), State::Authorized);
    }

    #[test]
    fn ble_auth_gates_credentials() {
        let mut m = Machine::new(Transport::Ble, true);
        drain(&mut m);
        assert_eq!(
            m.on_command(CREDS),
            Decision::Reject(ErrorCode::NotAuthorized)
        );
        assert_eq!(drain(&mut m), (Some(ErrorCode::NotAuthorized), None));

        m.authorize();
        assert_eq!(
            m.on_command(CREDS),
            Decision::Connect {
                ssid: "net",
                password: "pw"
            }
        );
        assert_eq!(
            drain(&mut m),
            (Some(ErrorCode::None), Some(State::Provisioning))
        );
    }

    #[test]
    fn identify_and_info_work_without_auth() {
        let mut m = Machine::new(Transport::Ble, true);
        assert_eq!(m.on_command(Command::Identify), Decision::Identify);
        assert_eq!(m.on_command(Command::DeviceInfo), Decision::DeviceInfo);
    }

    #[test]
    fn failed_connect_returns_to_authorized_with_error() {
        let mut m = Machine::new(Transport::Serial, false);
        m.on_command(CREDS);
        drain(&mut m);
        m.on_connect_result(false);
        assert_eq!(
            drain(&mut m),
            (Some(ErrorCode::UnableToConnect), Some(State::Authorized))
        );
    }

    #[test]
    fn successful_connect_and_reprovision() {
        let mut m = Machine::new(Transport::Serial, false);
        m.on_command(CREDS);
        m.on_connect_result(true);
        assert_eq!(m.state(), State::Provisioned);
        assert!(matches!(m.on_command(CREDS), Decision::Connect { .. }));
    }

    #[test]
    fn credentials_rejected_while_busy() {
        let mut m = Machine::new(Transport::Serial, false);
        m.on_command(CREDS);
        assert_eq!(m.on_command(CREDS), Decision::Reject(ErrorCode::Unknown));
    }

    #[test]
    fn hostname_validation() {
        let mut m = Machine::new(Transport::Ble, false);
        assert_eq!(
            m.on_command(Command::Hostname(Some("bad host"))),
            Decision::Reject(ErrorCode::BadHostname)
        );
        assert_eq!(
            m.on_command(Command::Hostname(Some("good-host"))),
            Decision::SetHostname("good-host")
        );
    }

    #[test]
    fn get_state_forces_report() {
        let mut m = Machine::new(Transport::Serial, false);
        drain(&mut m);
        assert_eq!(
            m.on_command(Command::GetCurrentState),
            Decision::ReportState
        );
        assert_eq!(drain(&mut m).1, Some(State::Authorized));
    }

    #[test]
    fn revoke_after_timeout() {
        let mut m = Machine::new(Transport::Ble, true);
        m.authorize();
        m.revoke();
        assert_eq!(m.state(), State::AuthorizationRequired);
    }

    #[test]
    fn already_online_at_start_still_needs_authorization() {
        let mut m = Machine::new(Transport::Ble, true);
        m.mark_provisioned();
        assert_eq!(m.state(), State::Provisioned);
        assert_eq!(
            m.on_command(CREDS),
            Decision::Reject(ErrorCode::NotAuthorized)
        );
        assert_eq!(
            m.on_command(Command::Hostname(Some("other"))),
            Decision::Reject(ErrorCode::NotAuthorized)
        );
        assert_eq!(
            m.on_command(Command::DeviceName(Some("Other"))),
            Decision::Reject(ErrorCode::NotAuthorized)
        );
        assert_eq!(m.state(), State::Provisioned);

        // A button press allows re-provisioning, and the state stays honest.
        m.authorize();
        assert_eq!(m.state(), State::Provisioned);
        assert!(matches!(m.on_command(CREDS), Decision::Connect { .. }));
    }

    #[test]
    fn already_online_without_auth_allows_reprovisioning() {
        let mut m = Machine::new(Transport::Ble, false);
        m.mark_provisioned();
        assert!(matches!(m.on_command(CREDS), Decision::Connect { .. }));
    }

    #[test]
    fn revoke_while_provisioned_blocks_writes() {
        let mut m = Machine::new(Transport::Ble, true);
        m.authorize();
        m.on_command(CREDS);
        m.on_connect_result(true);
        assert_eq!(m.state(), State::Provisioned);

        m.revoke();
        assert_eq!(m.state(), State::Provisioned);
        assert_eq!(
            m.on_command(CREDS),
            Decision::Reject(ErrorCode::NotAuthorized)
        );
        assert_eq!(
            m.on_command(Command::Hostname(Some("other"))),
            Decision::Reject(ErrorCode::NotAuthorized)
        );
    }

    #[test]
    fn revoke_during_provisioning_then_failure_requires_auth_again() {
        let mut m = Machine::new(Transport::Ble, true);
        m.authorize();
        m.on_command(CREDS);
        m.revoke();
        assert_eq!(m.state(), State::Provisioning);
        m.on_connect_result(false);
        assert_eq!(m.state(), State::AuthorizationRequired);
        assert_eq!(m.error(), ErrorCode::UnableToConnect);
    }

    #[test]
    fn hostname_and_device_name_need_auth() {
        let mut m = Machine::new(Transport::Ble, true);
        assert_eq!(
            m.on_command(Command::Hostname(Some("good-host"))),
            Decision::Reject(ErrorCode::NotAuthorized)
        );
        assert_eq!(
            m.on_command(Command::DeviceName(Some("Lamp"))),
            Decision::Reject(ErrorCode::NotAuthorized)
        );
        // Reads stay open.
        assert_eq!(m.on_command(Command::Hostname(None)), Decision::GetHostname);
    }

    #[test]
    fn serial_stop_and_resume() {
        let mut m = Machine::new(Transport::Serial, false);
        m.set_available(false);
        assert_eq!(m.state(), State::Stopped);
        assert_eq!(m.on_command(CREDS), Decision::Reject(ErrorCode::Unknown));
        m.set_available(true);
        assert_eq!(m.state(), State::Authorized);
    }

    #[test]
    fn transport_specific_errors_are_downgraded() {
        let mut m = Machine::new(Transport::Serial, false);
        m.on_rpc_error(ErrorCode::NotAuthorized);
        assert_eq!(m.error(), ErrorCode::Unknown);
    }
}
