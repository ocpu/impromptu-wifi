//! End-to-end: a scripted client talks to `Driver` + `SerialLink`.

extern crate std;
use std::{string::String, vec::Vec};

use embassy_futures::block_on;
use embedded_io_async::{ErrorType, Read, Write};
use impromptu_wifi_core::proto::serial::{encode_packet, Feed, Framer, PacketType};
use impromptu_wifi_core::proto::{Command, ErrorCode, RawRpc, State, Transport};
use impromptu_wifi_core::{
    unsupported, ConnectError, DeviceInfoSource, Driver, NetworkInfo, NoAuth, ScanError, Url,
    WifiAuth, WifiConnect, WifiScanner,
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
