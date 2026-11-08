use core::fmt;

use impromptu_wifi_proto::ble::Capabilities;
use impromptu_wifi_proto::rpc::Strings;
use impromptu_wifi_proto::{Command, EncodeError, ErrorCode, RawRpc, State, Transport};
use impromptu_wifi_traits::{NetworkFlags, Ssid, Url};

use crate::link::{ClientLink, Incoming};
use crate::session::{Event, Session, Step};
use crate::types::{DeviceInfo, NetworkState, ScannedNetwork, Text};

const FRAME_BUF: usize = impromptu_wifi_proto::ble::MAX_RPC_FRAME;

/// Why a client call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum ClientError<E> {
    /// The link failed.
    Link(E),
    /// The device answered with this error code. `UnknownCommand` usually
    /// means the device doesn't support the command.
    Device(ErrorCode),
    /// The command doesn't exist on this transport (for example identify
    /// over serial).
    Unsupported,
    /// The device sent something that doesn't fit the protocol.
    Protocol,
    /// A value was too long to send, or a response too long to keep.
    TooLong,
}

impl<E: fmt::Debug> fmt::Display for ClientError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Link(e) => write!(f, "link error: {e:?}"),
            Self::Device(code) => write!(f, "device reported {code:?}"),
            Self::Unsupported => f.write_str("command does not exist on this transport"),
            Self::Protocol => f.write_str("unexpected or malformed response"),
            Self::TooLong => f.write_str("value too long"),
        }
    }
}

impl<E: fmt::Debug> core::error::Error for ClientError<E> {}

/// Decoding problems inside a result; becomes [`ClientError::Protocol`] or
/// [`ClientError::TooLong`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bad {
    Protocol,
    TooLong,
}

impl<E> From<Bad> for ClientError<E> {
    fn from(b: Bad) -> Self {
        match b {
            Bad::Protocol => ClientError::Protocol,
            Bad::TooLong => ClientError::TooLong,
        }
    }
}

/// Talks to one device over one link.
pub struct Client<L> {
    link: L,
    session: Session,
    rx: [u8; FRAME_BUF],
    tx: [u8; FRAME_BUF],
}

impl<L: ClientLink> Client<L> {
    /// A client on `link`.
    pub fn new(link: L) -> Self {
        Self {
            link,
            session: Session::new(L::TRANSPORT),
            rx: [0; FRAME_BUF],
            tx: [0; FRAME_BUF],
        }
    }

    /// The device's last reported state, without asking it.
    ///
    /// This can lag: after a failed join the device reports the error
    /// before its state falls back, and a call returns as soon as the error
    /// arrives. Use [`current_state`](Self::current_state) on serial, or
    /// [`wait_for_state`](Self::wait_for_state), when it matters.
    pub fn last_state(&self) -> Option<State> {
        self.session.state()
    }

    /// The device's last reported error, without asking it.
    pub fn last_error(&self) -> ErrorCode {
        self.session.error()
    }

    /// Give back the link.
    pub fn into_inner(self) -> L {
        self.link
    }

    /// The device's current state: read from the state characteristic on
    /// BLE, requested with a command on serial.
    pub async fn current_state(&mut self) -> Result<State, ClientError<L::Error>> {
        if let Some(v) = self.link.read_state().await.map_err(ClientError::Link)? {
            let state = State::try_from(v).map_err(|_| ClientError::Protocol)?;
            self.session.on_event(Event::State(state));
            return Ok(state);
        }
        self.exchange(Command::GetCurrentState, |_| Ok(())).await?;
        self.session.state().ok_or(ClientError::Protocol)
    }

    /// Which optional commands the device supports (BLE only). Serial has
    /// no capabilities byte: try a command and treat
    /// `Device(UnknownCommand)` as "not supported".
    pub async fn capabilities(&mut self) -> Result<Capabilities, ClientError<L::Error>> {
        match self
            .link
            .read_capabilities()
            .await
            .map_err(ClientError::Link)?
        {
            Some(bits) => Ok(Capabilities::from_bits(bits)),
            None => Err(ClientError::Unsupported),
        }
    }

    /// The link, for transport-specific operations.
    pub fn link_mut(&mut self) -> &mut L {
        &mut self.link
    }

    /// Wait until the device reports `target`, for example
    /// `State::Authorized` after asking the user to press the device's button.
    pub async fn wait_for_state(&mut self, target: State) -> Result<(), ClientError<L::Error>> {
        while self.session.state() != Some(target) {
            let incoming = self
                .link
                .recv(&mut self.rx)
                .await
                .map_err(ClientError::Link)?;
            if let Some(event) = self.event(incoming)? {
                self.session.on_event(event);
            }
        }
        Ok(())
    }

    /// Send credentials and wait for the outcome. On success, returns the
    /// URL the device suggests opening, if any.
    pub async fn provision(
        &mut self,
        ssid: &str,
        password: &str,
    ) -> Result<Option<Url>, ClientError<L::Error>> {
        let mut url = None;
        self.exchange(Command::WifiSettings { ssid, password }, |mut strings| {
            url = match strings.next() {
                Some(s) => Some(s.map_err(|_| Bad::Protocol)?)
                    .filter(|s| !s.is_empty())
                    .map(owned)
                    .transpose()?,
                None => None,
            };
            Ok(())
        })
        .await?;
        Ok(url)
    }

    /// Ask for firmware and hardware information.
    pub async fn device_info(&mut self) -> Result<DeviceInfo, ClientError<L::Error>> {
        let mut info = None;
        self.exchange(Command::DeviceInfo, |mut strings| {
            let mut next = || -> Result<Option<Text>, Bad> {
                strings
                    .next()
                    .map(|s| owned(s.map_err(|_| Bad::Protocol)?))
                    .transpose()
            };
            let required = [next()?, next()?, next()?, next()?];
            let [Some(firmware_name), Some(firmware_version), Some(chip), Some(device_name)] =
                required
            else {
                return Err(Bad::Protocol);
            };
            let os = match (next()?, next()?) {
                (Some(name), version) => Some((name, version.unwrap_or_default())),
                (None, _) => None,
            };
            info = Some(DeviceInfo {
                firmware_name,
                firmware_version,
                chip,
                device_name,
                os,
            });
            Ok(())
        })
        .await?;
        info.ok_or(ClientError::Protocol)
    }

    /// Ask for visible networks. Networks beyond `N` are dropped.
    pub async fn scan<const N: usize>(
        &mut self,
    ) -> Result<heapless::Vec<ScannedNetwork, N>, ClientError<L::Error>> {
        let mut found = heapless::Vec::new();
        self.exchange(Command::ScanNetworks, |mut strings| {
            // Serial: one network per result. BLE: all networks, in triples.
            loop {
                let Some(ssid) = strings.next() else {
                    return Ok(());
                };
                let ssid = ssid.map_err(|_| Bad::Protocol)?;
                let rssi = strings
                    .next()
                    .ok_or(Bad::Protocol)?
                    .map_err(|_| Bad::Protocol)?;
                let auth = strings
                    .next()
                    .ok_or(Bad::Protocol)?
                    .map_err(|_| Bad::Protocol)?;
                let network = scanned(L::TRANSPORT, ssid, rssi, auth)?;
                let _ = found.push(network);
            }
        })
        .await?;
        Ok(found)
    }

    /// Make the device identify itself (BLE). Returns once the command is
    /// written: the spec defines no response to wait for.
    pub async fn identify(&mut self) -> Result<(), ClientError<L::Error>> {
        let len = Command::Identify
            .encode(L::TRANSPORT, &mut self.tx)
            .map_err(|_| ClientError::Unsupported)?;
        self.session.abort();
        self.link
            .send_command(&self.tx[..len])
            .await
            .map_err(ClientError::Link)
    }

    /// Read the hostname.
    pub async fn hostname(&mut self) -> Result<Text, ClientError<L::Error>> {
        self.single_string(Command::Hostname(None)).await
    }

    /// Set the hostname; returns the hostname the device reports afterwards.
    pub async fn set_hostname(&mut self, hostname: &str) -> Result<Text, ClientError<L::Error>> {
        self.single_string(Command::Hostname(Some(hostname))).await
    }

    /// Read the device name.
    pub async fn device_name(&mut self) -> Result<Text, ClientError<L::Error>> {
        self.single_string(Command::DeviceName(None)).await
    }

    /// Set the device name; returns the name the device reports afterwards.
    pub async fn set_device_name(&mut self, name: &str) -> Result<Text, ClientError<L::Error>> {
        self.single_string(Command::DeviceName(Some(name))).await
    }

    /// Ask for general connectivity (serial).
    pub async fn network_state(&mut self) -> Result<NetworkState, ClientError<L::Error>> {
        let mut state = None;
        self.exchange(Command::NetworkState, |mut strings| {
            let flags = strings
                .next()
                .ok_or(Bad::Protocol)?
                .map_err(|_| Bad::Protocol)?;
            let flags = NetworkFlags(flags.parse().map_err(|_| Bad::Protocol)?);
            let mut urls = heapless::Vec::new();
            for url in strings {
                let url =
                    Url::try_from(url.map_err(|_| Bad::Protocol)?).map_err(|_| Bad::TooLong)?;
                let _ = urls.push(url);
            }
            state = Some(NetworkState { flags, urls });
            Ok(())
        })
        .await?;
        state.ok_or(ClientError::Protocol)
    }

    async fn single_string(&mut self, command: Command<'_>) -> Result<Text, ClientError<L::Error>> {
        let mut out = None;
        self.exchange(command, |mut strings| {
            let s = strings
                .next()
                .ok_or(Bad::Protocol)?
                .map_err(|_| Bad::Protocol)?;
            out = Some(owned(s)?);
            Ok(())
        })
        .await?;
        out.ok_or(ClientError::Protocol)
    }

    /// Send `command` and feed each result that answers it to `on_result`.
    async fn exchange(
        &mut self,
        command: Command<'_>,
        mut on_result: impl FnMut(Strings<'_>) -> Result<(), Bad>,
    ) -> Result<(), ClientError<L::Error>> {
        let t = L::TRANSPORT;
        let len = command.encode(t, &mut self.tx).map_err(|e| match e {
            EncodeError::WrongTransport => ClientError::Unsupported,
            _ => ClientError::TooLong,
        })?;
        if !self.session.begin(&command) {
            return Err(ClientError::Unsupported);
        }
        if let Err(e) = self.link.send_command(&self.tx[..len]).await {
            self.session.abort();
            return Err(ClientError::Link(e));
        }

        loop {
            let incoming = match self.link.recv(&mut self.rx).await {
                Ok(i) => i,
                Err(e) => {
                    self.session.abort();
                    return Err(ClientError::Link(e));
                }
            };
            let result_len = match incoming {
                Incoming::Result(n) => Some(n),
                _ => None,
            };
            let event = match self.event(incoming) {
                Ok(Some(ev)) => ev,
                Ok(None) => continue,
                Err(e) => {
                    self.session.abort();
                    return Err(e);
                }
            };
            match self.session.on_event(event) {
                Step::Continue => {}
                Step::Done => return Ok(()),
                Step::Failed(code) => return Err(ClientError::Device(code)),
                Step::Result { last } => {
                    let frame = &self.rx[..result_len.unwrap_or(0)];
                    let raw = RawRpc::parse(frame, t).map_err(|_| ClientError::Protocol)?;
                    if let Err(bad) = on_result(raw.strings()) {
                        self.session.abort();
                        return Err(bad.into());
                    }
                    if last {
                        return Ok(());
                    }
                }
            }
        }
    }

    /// Turn a link message into a session event. Unknown state values are
    /// skipped (`Ok(None)`), unknown error values count as `Unknown`.
    fn event(&self, incoming: Incoming) -> Result<Option<Event>, ClientError<L::Error>> {
        Ok(match incoming {
            Incoming::State(v) => State::try_from(v).ok().map(Event::State),
            Incoming::Error(v) => Some(Event::Error(
                ErrorCode::try_from(v).unwrap_or(ErrorCode::Unknown),
            )),
            Incoming::Result(n) => {
                let raw =
                    RawRpc::parse(self.rx.get(..n).ok_or(ClientError::Protocol)?, L::TRANSPORT)
                        .map_err(|_| ClientError::Protocol)?;
                Some(Event::Result {
                    command: raw.command,
                    empty: raw.data.is_empty(),
                })
            }
            Incoming::Malformed => return Err(ClientError::Protocol),
        })
    }
}

fn owned(s: &str) -> Result<Text, Bad> {
    Text::try_from(s).map_err(|_| Bad::TooLong)
}

fn scanned(t: Transport, ssid: &str, rssi: &str, auth: &str) -> Result<ScannedNetwork, Bad> {
    let ssid = Ssid::try_from(ssid).map_err(|_| Bad::TooLong)?;
    let rssi: i16 = rssi.trim().parse().map_err(|_| Bad::Protocol)?;
    let rssi_dbm = rssi.clamp(i8::MIN.into(), i8::MAX.into()) as i8;
    let (requires_passphrase, auth) = match t {
        Transport::Serial => (auth.eq_ignore_ascii_case("YES"), None),
        Transport::Ble => (
            !auth.eq_ignore_ascii_case("NO"),
            Some(heapless::String::try_from(auth).map_err(|_| Bad::TooLong)?),
        ),
    };
    Ok(ScannedNetwork {
        ssid,
        rssi_dbm,
        requires_passphrase,
        auth,
    })
}
