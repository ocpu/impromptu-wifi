//! End-to-end: a scripted client talks to `Driver` + `SerialLink`.

extern crate std;
use std::{string::String, vec::Vec};

use embassy_futures::block_on;
use embedded_io_async::{ErrorType, Read, Write};
use impromptu_wifi_core::proto::serial::{encode_packet, Feed, Framer, PacketType};
use impromptu_wifi_core::proto::{Command, ErrorCode, RawRpc, State, Transport};
use impromptu_wifi_core::{
    unsupported, ConnectError, DeviceEvent, DeviceInfoSource, Driver, EventSource, NetworkInfo,
    NoAuth, NoEvents, NotifyOnJoin, ScanError, Url, WifiAuth, WifiConnect, WifiScanner,
};
use impromptu_wifi_serial::{SerialError, SerialLink};

/// Plays back `input`, then reports EOF; records everything written.
struct Script {
    input: Vec<u8>,
    pos: usize,
    output: Vec<u8>,
}

impl ErrorType for Script {
    type Error = core::convert::Infallible;
}

impl Read for Script {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        // Dribble out a few bytes at a time to exercise reassembly.
        let n = buf.len().min(7).min(self.input.len() - self.pos);
        buf[..n].copy_from_slice(&self.input[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Write for Script {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.output.extend_from_slice(buf);
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

struct FakeWifi;

impl WifiConnect for FakeWifi {
    async fn connect(&mut self, ssid: &str, password: &str) -> Result<Option<Url>, ConnectError> {
        match (ssid, password) {
            ("home", "hunter22") => Ok(Some(Url::try_from("http://192.168.1.50").unwrap())),
            ("home", _) => Err(ConnectError::InvalidCredentials),
            _ => Err(ConnectError::NetworkNotFound),
        }
    }
}

impl WifiScanner for FakeWifi {
    async fn scan<const N: usize>(
        &mut self,
        out: &mut heapless::Vec<NetworkInfo, N>,
    ) -> Result<(), ScanError> {
        let found = [
            ("home", -40, WifiAuth::Wpa2Personal),
            ("cafe", -71, WifiAuth::Open),
        ];
        for (ssid, rssi, auth) in found {
            let _ = out.push(NetworkInfo::new(ssid, rssi, auth).unwrap());
        }
        Ok(())
    }
}

unsupported!(FakeWifi: NetworkStatus);

struct FakeDevice;

impl DeviceInfoSource for FakeDevice {
    fn firmware_name(&self) -> &str {
        "demo-fw"
    }
    fn firmware_version(&self) -> &str {
        "0.1.0"
    }
    fn chip(&self) -> &str {
        "esp32-c6"
    }
    fn device_name(&self) -> &str {
        "Desk Lamp"
    }
}

unsupported!(FakeDevice: Identify, HostnameProvider);

fn client_packet(cmd: Command<'_>) -> Vec<u8> {
    let mut rpc = [0u8; 258];
    let n = cmd.encode(Transport::Serial, &mut rpc).unwrap();
    let mut out = [0u8; 300];
    let m = encode_packet(&mut out, PacketType::Rpc, &rpc[..n]).unwrap();
    out[..m].to_vec()
}

#[derive(Debug, PartialEq)]
enum Seen {
    State(State),
    Error(ErrorCode),
    Result(u8, Vec<String>),
}

fn decode_output(bytes: &[u8]) -> Vec<Seen> {
    let mut framer: Framer = Framer::new();
    let mut seen = Vec::new();
    for &b in bytes {
        if framer.feed(b) != Feed::Packet {
            continue;
        }
        let p = framer.packet().unwrap();
        seen.push(match p.ty {
            PacketType::CurrentState => Seen::State(State::try_from(p.data[0]).unwrap()),
            PacketType::ErrorState => Seen::Error(ErrorCode::try_from(p.data[0]).unwrap()),
            PacketType::RpcResult => {
                let raw = RawRpc::parse(p.data, Transport::Serial).unwrap();
                let strings = raw.strings().map(|s| String::from(s.unwrap())).collect();
                Seen::Result(raw.command, strings)
            }
            PacketType::Rpc => panic!("device sent an RPC command"),
        });
    }
    seen
}

fn run_session(input: Vec<u8>) -> Vec<Seen> {
    run_session_with_driver(input).0
}

fn run_session_with_driver(input: Vec<u8>) -> (Vec<Seen>, Option<ConnectError>) {
    let link = SerialLink::new(Script {
        input,
        pos: 0,
        output: Vec::new(),
    });
    let mut driver = Driver::new(link, FakeWifi, FakeDevice, false);
    let err = block_on(driver.run(&mut NoAuth)).unwrap_err();
    assert!(matches!(err, SerialError::Eof));
    let connect_error = driver.last_connect_error();
    let (link, _, _) = driver.into_parts();
    (decode_output(&link.into_inner().output), connect_error)
}

fn strings(s: &[&str]) -> Vec<String> {
    s.iter().map(|s| String::from(*s)).collect()
}

#[test]
fn provisions_with_log_noise_on_the_line() {
    let mut input = b"boot: ok\r\n".to_vec();
    input.extend(client_packet(Command::WifiSettings {
        ssid: "home",
        password: "hunter22",
    }));

    assert_eq!(
        run_session(input),
        [
            Seen::Error(ErrorCode::None),
            Seen::State(State::Authorized),
            Seen::Error(ErrorCode::None),
            Seen::State(State::Provisioning),
            Seen::State(State::Provisioned),
            Seen::Result(0x01, strings(&["http://192.168.1.50"])),
        ]
    );
}

#[test]
fn wrong_password_reports_unable_to_connect() {
    let input = client_packet(Command::WifiSettings {
        ssid: "home",
        password: "nope",
    });
    let (seen, connect_error) = run_session_with_driver(input);
    assert_eq!(connect_error, Some(ConnectError::InvalidCredentials));
    assert_eq!(
        &seen[2..],
        [
            Seen::Error(ErrorCode::None),
            Seen::State(State::Provisioning),
            Seen::Error(ErrorCode::UnableToConnect),
            Seen::State(State::Authorized),
        ]
    );
}

#[test]
fn device_info_and_scan() {
    let mut input = client_packet(Command::DeviceInfo);
    input.extend(client_packet(Command::ScanNetworks));
    let seen = run_session(input);
    assert_eq!(
        &seen[2..],
        [
            Seen::Error(ErrorCode::None),
            Seen::Result(
                0x03,
                strings(&["demo-fw", "0.1.0", "esp32-c6", "Desk Lamp"])
            ),
            Seen::Error(ErrorCode::None),
            Seen::Result(0x04, strings(&["home", "-40", "YES"])),
            Seen::Result(0x04, strings(&["cafe", "-71", "NO"])),
            Seen::Result(0x04, Vec::new()),
        ]
    );
}

#[test]
fn unsupported_and_corrupt_commands() {
    // Both are declared unsupported via `unsupported!`.
    let mut input = client_packet(Command::Hostname(None));
    input.extend(client_packet(Command::NetworkState));
    let mut corrupt = client_packet(Command::DeviceInfo);
    *corrupt.last_mut().unwrap() ^= 0xFF;
    input.extend(corrupt);

    let seen = run_session(input);
    assert_eq!(
        &seen[2..],
        [
            Seen::Error(ErrorCode::None),
            Seen::Error(ErrorCode::UnknownCommand),
            Seen::Error(ErrorCode::None),
            Seen::Error(ErrorCode::UnknownCommand),
            Seen::Error(ErrorCode::InvalidRpc),
        ]
    );
}

/// Like `Script`, but waits for more input instead of reporting EOF, so an
/// availability change can win the driver's race.
struct Waiting {
    input: std::collections::VecDeque<u8>,
    output: Vec<u8>,
}

impl ErrorType for Waiting {
    type Error = core::convert::Infallible;
}

impl Read for Waiting {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if self.input.is_empty() {
            core::future::pending::<()>().await;
        }
        let n = buf.len().min(self.input.len());
        for b in &mut buf[..n] {
            *b = self.input.pop_front().unwrap();
        }
        Ok(n)
    }
}

impl Write for Waiting {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.output.extend_from_slice(buf);
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Reports the queued events, one per call, then nothing.
struct Events(std::collections::VecDeque<DeviceEvent>);

impl EventSource for Events {
    async fn next(&mut self) -> DeviceEvent {
        match self.0.pop_front() {
            Some(e) => e,
            None => core::future::pending().await,
        }
    }
}

fn radio_switch(on: bool) -> Events {
    Events([DeviceEvent::Available(on)].into())
}

#[test]
fn radio_off_reports_stopped_and_refuses_credentials() {
    let link = SerialLink::new(Waiting {
        input: Default::default(),
        output: Vec::new(),
    });
    let mut driver = Driver::new(link, FakeWifi, FakeDevice, false);

    // The radio goes off while the device is idle.
    let mut radio = radio_switch(false);
    block_on(driver.step_with(&mut NoAuth, &mut radio)).unwrap();
    assert_eq!(driver.machine().state(), State::Stopped);

    // Credentials while stopped are refused. The driver owns the link, so
    // take it apart to queue the command, keeping the machine stopped.
    let packet = client_packet(Command::WifiSettings {
        ssid: "home",
        password: "hunter22",
    });
    let (link, wifi, device) = driver.into_parts();
    let mut io = link.into_inner();
    io.input.extend(packet);
    let mut driver = Driver::new(SerialLink::new(io), wifi, device, false);
    driver.machine_mut().set_available(false);
    block_on(driver.step_with(&mut NoAuth, &mut Events(Default::default()))).unwrap();
    assert_eq!(driver.machine().error(), ErrorCode::Unknown);

    // The radio comes back.
    block_on(driver.step_with(&mut NoAuth, &mut radio_switch(true))).unwrap();
    assert_eq!(driver.machine().state(), State::Authorized);

    let (link, _, _) = driver.into_parts();
    let seen = decode_output(&link.into_inner().output);
    assert!(seen.contains(&Seen::State(State::Stopped)), "{seen:?}");
    assert_eq!(
        seen.last(),
        Some(&Seen::State(State::Authorized)),
        "{seen:?}"
    );
}

#[test]
fn provisioned_elsewhere_is_reported_and_answered() {
    let link = SerialLink::new(Waiting {
        input: Default::default(),
        output: Vec::new(),
    });
    let mut driver = Driver::new(link, FakeWifi, FakeDevice, false);
    let url = Url::try_from("http://192.168.1.50").unwrap();

    // Another transport provisioned the device while this one was idle.
    let mut elsewhere = Events([DeviceEvent::Provisioned(Some(url.clone()))].into());
    block_on(driver.step_with(&mut NoAuth, &mut elsewhere)).unwrap();
    assert_eq!(driver.machine().state(), State::Provisioned);

    // A client asking for the state now hears "provisioned" with the URL,
    // as after a provisioning on this transport.
    let (link, wifi, device) = driver.into_parts();
    let mut io = link.into_inner();
    io.input.extend(client_packet(Command::GetCurrentState));
    let mut driver = Driver::new(SerialLink::new(io), wifi, device, false);
    driver.mark_provisioned(Some(url));
    block_on(driver.step_with(&mut NoAuth, &mut NoEvents)).unwrap();

    let (link, _, _) = driver.into_parts();
    let seen = decode_output(&link.into_inner().output);
    assert!(seen.contains(&Seen::State(State::Provisioned)), "{seen:?}");
    assert_eq!(
        seen.last(),
        Some(&Seen::Result(0x01, strings(&["http://192.168.1.50"]))),
        "{seen:?}"
    );
}

#[test]
fn notify_on_join_reports_successful_joins_only() {
    let mut joined = Vec::new();
    {
        let mut wifi = NotifyOnJoin::new(FakeWifi, |url: &Option<Url>| {
            joined.push(url.as_deref().map(String::from));
        });
        assert!(block_on(wifi.connect("home", "wrong")).is_err());
        assert!(block_on(wifi.connect("home", "hunter22")).is_ok());
    }
    assert_eq!(joined, [Some(String::from("http://192.168.1.50"))]);

    const { assert!(<NotifyOnJoin<FakeWifi, fn(&Option<Url>)> as WifiScanner>::SUPPORTED) };
}

#[test]
fn a_pair_of_event_sources_yields_from_either() {
    let mut pair = (
        Events(Default::default()),
        Events([DeviceEvent::Available(false)].into()),
    );
    assert_eq!(block_on(pair.next()), DeviceEvent::Available(false));
    pair.0 .0.push_back(DeviceEvent::Provisioned(None));
    pair.1 .0.push_back(DeviceEvent::Available(true));
    assert_eq!(
        block_on(pair.next()),
        DeviceEvent::Provisioned(None),
        "the first source wins a tie"
    );
}
