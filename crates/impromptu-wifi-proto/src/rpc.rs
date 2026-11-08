//! RPC frames: `[command] [data length] [data …]`, plus a checksum on BLE.
//!
//! Commands flow client → device, results device → client. Result data is
//! a list of length-prefixed UTF-8 strings.

use core::fmt::{self, Write as _};

use crate::types::{command_id as id, MAX_PASSWORD_LEN, MAX_SSID_LEN};
use crate::{checksum, DecodeError, EncodeError, ErrorCode, Transport};

/// A structurally valid RPC frame whose command has not been interpreted yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawRpc<'a> {
    /// Command id.
    pub command: u8,
    /// Data bytes (checksum excluded).
    pub data: &'a [u8],
}

impl<'a> RawRpc<'a> {
    /// Parse one complete RPC frame as defined for `transport`.
    ///
    /// On BLE `frame` includes the trailing checksum, which is verified. On
    /// serial `frame` is the data section of an RPC packet. The frame must
    /// be exactly as long as its length field says.
    pub fn parse(frame: &'a [u8], transport: Transport) -> Result<Self, DecodeError> {
        let cs = usize::from(transport.rpc_has_checksum());
        if frame.len() < 2 + cs {
            return Err(DecodeError::Incomplete);
        }
        let len = usize::from(frame[1]);
        let total = 2 + len + cs;
        if frame.len() < total {
            return Err(DecodeError::Incomplete);
        }
        if frame.len() > total {
            return Err(DecodeError::LengthMismatch);
        }
        if cs == 1 && checksum(&frame[..total - 1]) != frame[total - 1] {
            return Err(DecodeError::Checksum);
        }
        Ok(RawRpc {
            command: frame[0],
            data: &frame[2..2 + len],
        })
    }

    /// Iterate over the data as length-prefixed strings.
    pub fn strings(&self) -> Strings<'a> {
        Strings::new(self.data)
    }
}

/// Iterator over length-prefixed UTF-8 strings.
///
/// Stops after yielding the first error.
#[derive(Debug, Clone)]
pub struct Strings<'a> {
    rest: &'a [u8],
}

impl<'a> Strings<'a> {
    /// Iterate over `data`.
    pub fn new(data: &'a [u8]) -> Self {
        Self { rest: data }
    }
}

impl<'a> Iterator for Strings<'a> {
    type Item = Result<&'a str, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        let (&len, tail) = self.rest.split_first()?;
        let len = usize::from(len);
        if tail.len() < len {
            self.rest = &[];
            return Some(Err(DecodeError::LengthMismatch));
        }
        let (s, rest) = tail.split_at(len);
        self.rest = rest;
        Some(core::str::from_utf8(s).map_err(|_| {
            self.rest = &[];
            DecodeError::BadUtf8
        }))
    }
}

/// An interpreted RPC command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Command<'a> {
    /// Connect to this network.
    WifiSettings {
        /// Network name, 1 to [`MAX_SSID_LEN`] bytes.
        ssid: &'a str,
        /// Passphrase, up to [`MAX_PASSWORD_LEN`] bytes; empty for open networks.
        password: &'a str,
    },
    /// Serial only: report the current state.
    GetCurrentState,
    /// BLE only: make the device visibly or audibly identify itself.
    Identify,
    /// Report firmware and hardware information.
    DeviceInfo,
    /// Report visible Wi-Fi networks.
    ScanNetworks,
    /// Get (`None`) or set (`Some`) the hostname.
    Hostname(Option<&'a str>),
    /// Get (`None`) or set (`Some`) the device name.
    DeviceName(Option<&'a str>),
    /// Serial only: report general network connectivity.
    NetworkState,
}

impl<'a> Command<'a> {
    /// Interpret `raw` for `transport`.
    ///
    /// The error is the [`ErrorCode`] the device should report:
    /// [`ErrorCode::UnknownCommand`] for ids this transport doesn't define,
    /// [`ErrorCode::InvalidRpc`] for malformed payloads.
    pub fn parse(transport: Transport, raw: RawRpc<'a>) -> Result<Self, ErrorCode> {
        use Transport::*;
        let empty = |cmd: Command<'a>| {
            if raw.data.is_empty() {
                Ok(cmd)
            } else {
                Err(ErrorCode::InvalidRpc)
            }
        };
        let opt_str = |data: &'a [u8]| -> Result<Option<&'a str>, ErrorCode> {
            if data.is_empty() {
                Ok(None)
            } else {
                core::str::from_utf8(data)
                    .map(Some)
                    .map_err(|_| ErrorCode::InvalidRpc)
            }
        };

        match (raw.command, transport) {
            (id::WIFI_SETTINGS, _) => {
                let mut it = raw.strings();
                let ssid = it
                    .next()
                    .ok_or(ErrorCode::InvalidRpc)?
                    .map_err(|_| ErrorCode::InvalidRpc)?;
                let password = it
                    .next()
                    .ok_or(ErrorCode::InvalidRpc)?
                    .map_err(|_| ErrorCode::InvalidRpc)?;
                if it.next().is_some()
                    || ssid.is_empty()
                    || ssid.len() > MAX_SSID_LEN
                    || password.len() > MAX_PASSWORD_LEN
                {
                    return Err(ErrorCode::InvalidRpc);
                }
                Ok(Command::WifiSettings { ssid, password })
            }
            (id::GET_CURRENT_STATE, Serial) => empty(Command::GetCurrentState),
            (id::IDENTIFY, Ble) => empty(Command::Identify),
            (id::DEVICE_INFO, _) => empty(Command::DeviceInfo),
            (id::SCAN_NETWORKS, _) => empty(Command::ScanNetworks),
            (id::HOSTNAME, _) => opt_str(raw.data).map(Command::Hostname),
            (id::DEVICE_NAME, _) => opt_str(raw.data).map(Command::DeviceName),
            (id::NETWORK_STATE, Serial) => empty(Command::NetworkState),
            _ => Err(ErrorCode::UnknownCommand),
        }
    }

    /// The command id of this command on the wire.
    pub const fn id(&self) -> u8 {
        match self {
            Command::WifiSettings { .. } => id::WIFI_SETTINGS,
            Command::GetCurrentState => id::GET_CURRENT_STATE,
            Command::Identify => id::IDENTIFY,
            Command::DeviceInfo => id::DEVICE_INFO,
            Command::ScanNetworks => id::SCAN_NETWORKS,
            Command::Hostname(_) => id::HOSTNAME,
            Command::DeviceName(_) => id::DEVICE_NAME,
            Command::NetworkState => id::NETWORK_STATE,
        }
    }

    /// Whether this command exists on `transport`. Command `0x02` means
    /// different things on serial and BLE, so sending a command on the
    /// wrong transport would silently ask for something else.
    pub const fn valid_for(&self, transport: Transport) -> bool {
        match self {
            Command::GetCurrentState | Command::NetworkState => {
                matches!(transport, Transport::Serial)
            }
            Command::Identify => matches!(transport, Transport::Ble),
            _ => true,
        }
    }

    /// Encode as a client sends it on `transport`, returning the number of
    /// bytes written. Fails with [`EncodeError::WrongTransport`] if the
    /// command doesn't exist on `transport`.
    pub fn encode(&self, transport: Transport, out: &mut [u8]) -> Result<usize, EncodeError> {
        if !self.valid_for(transport) {
            return Err(EncodeError::WrongTransport);
        }
        let mut w = FrameWriter::new(out, transport, self.id())?;
        match self {
            Command::WifiSettings { ssid, password } => {
                w.string(ssid)?;
                w.string(password)?;
            }
            Command::Hostname(Some(s)) | Command::DeviceName(Some(s)) => w.raw(s.as_bytes())?,
            _ => {}
        }
        Ok(w.finish()?.len())
    }
}

/// Builds an RPC result frame (a list of strings) in a caller-provided buffer.
///
/// A failed push leaves the writer unchanged, so callers can fill a frame
/// until it's full and then [`finish`](Self::finish).
///
/// ```
/// use impromptu_wifi_proto::{ResultWriter, Transport, command_id};
/// let mut buf = [0u8; 64];
/// let mut w = ResultWriter::new(&mut buf, Transport::Ble, command_id::WIFI_SETTINGS).unwrap();
/// w.push("http://device.local").unwrap();
/// let frame = w.finish().unwrap();
/// assert_eq!(frame[0], command_id::WIFI_SETTINGS);
/// ```
#[derive(Debug)]
pub struct ResultWriter<'b> {
    inner: FrameWriter<'b>,
}

impl<'b> ResultWriter<'b> {
    /// Start a result for `command`.
    pub fn new(buf: &'b mut [u8], transport: Transport, command: u8) -> Result<Self, EncodeError> {
        Ok(Self {
            inner: FrameWriter::new(buf, transport, command)?,
        })
    }

    /// Append a string.
    pub fn push(&mut self, s: &str) -> Result<&mut Self, EncodeError> {
        self.inner.string(s)?;
        Ok(self)
    }

    /// Append the `Display` form of `value` as a string (for RSSI values,
    /// network flags and so on).
    pub fn push_display(&mut self, value: impl fmt::Display) -> Result<&mut Self, EncodeError> {
        self.inner.display(value)?;
        Ok(self)
    }

    /// A position to [`rewind`](Self::rewind) to, for multi-string groups
    /// that must be written all-or-nothing.
    pub fn mark(&self) -> usize {
        self.inner.len
    }

    /// Drop everything pushed since `mark`.
    pub fn rewind(&mut self, mark: usize) {
        if mark >= 2 && mark <= self.inner.len {
            self.inner.len = mark;
        }
    }

    /// Fill in the length (and checksum on BLE) and return the frame.
    pub fn finish(self) -> Result<&'b [u8], EncodeError> {
        self.inner.finish()
    }
}

/// Shared frame-building logic for commands and results.
#[derive(Debug)]
struct FrameWriter<'b> {
    buf: &'b mut [u8],
    len: usize,
    transport: Transport,
}

impl<'b> FrameWriter<'b> {
    fn new(buf: &'b mut [u8], transport: Transport, command: u8) -> Result<Self, EncodeError> {
        if buf.len() < 2 {
            return Err(EncodeError::Overflow);
        }
        buf[0] = command;
        buf[1] = 0;
        Ok(Self {
            buf,
            len: 2,
            transport,
        })
    }

    /// One past the last index the data section may use.
    fn limit(&self) -> usize {
        let reserve = usize::from(self.transport.rpc_has_checksum());
        (2 + self.transport.max_rpc_data()).min(self.buf.len().saturating_sub(reserve))
    }

    fn raw(&mut self, bytes: &[u8]) -> Result<(), EncodeError> {
        let end = self.len + bytes.len();
        if end > self.limit() {
            return Err(EncodeError::Overflow);
        }
        self.buf[self.len..end].copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }

    fn string(&mut self, s: &str) -> Result<(), EncodeError> {
        self.display(s)
    }

    fn display(&mut self, value: impl fmt::Display) -> Result<(), EncodeError> {
        struct Cursor<'c> {
            buf: &'c mut [u8],
            len: usize,
        }
        impl fmt::Write for Cursor<'_> {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                let end = self.len + s.len();
                let dst = self.buf.get_mut(self.len..end).ok_or(fmt::Error)?;
                dst.copy_from_slice(s.as_bytes());
                self.len = end;
                Ok(())
            }
        }

        let start = self.len;
        let limit = self.limit().min(start + 1 + 255);
        if start >= limit {
            return Err(EncodeError::Overflow);
        }
        let mut cur = Cursor {
            buf: &mut self.buf[start + 1..limit],
            len: 0,
        };
        write!(cur, "{value}").map_err(|_| EncodeError::Overflow)?;
        let written = cur.len;
        self.buf[start] = written as u8;
        self.len = start + 1 + written;
        Ok(())
    }

    fn finish(self) -> Result<&'b [u8], EncodeError> {
        let FrameWriter {
            buf,
            mut len,
            transport,
        } = self;
        buf[1] = (len - 2) as u8;
        if transport.rpc_has_checksum() {
            if len >= buf.len() {
                return Err(EncodeError::Overflow);
            }
            buf[len] = checksum(&buf[..len]);
            len += 1;
        }
        let buf: &'b [u8] = buf;
        Ok(&buf[..len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worked example from the spec.
    #[test]
    fn spec_wifi_example() {
        let mut buf = [0u8; 64];
        let cmd = Command::WifiSettings {
            ssid: "MyWirelessAP",
            password: "mysecurepassword",
        };
        let n = cmd.encode(Transport::Serial, &mut buf).unwrap();
        assert_eq!(&buf[..3], &[0x01, 0x1E, 0x0C]);
        assert_eq!(&buf[3..15], b"MyWirelessAP");
        assert_eq!(buf[15], 0x10);
        assert_eq!(n, 2 + 0x1E);

        let raw = RawRpc::parse(&buf[..n], Transport::Serial).unwrap();
        assert_eq!(Command::parse(Transport::Serial, raw), Ok(cmd));
    }

    #[test]
    fn ble_frames_carry_checksum() {
        let mut buf = [0u8; 64];
        let n = Command::DeviceInfo
            .encode(Transport::Ble, &mut buf)
            .unwrap();
        assert_eq!(&buf[..n], &[0x03, 0x00, 0x03]);
        assert!(RawRpc::parse(&buf[..n], Transport::Ble).is_ok());

        buf[2] ^= 0xFF;
        assert_eq!(
            RawRpc::parse(&buf[..n], Transport::Ble),
            Err(DecodeError::Checksum)
        );
    }

    #[test]
    fn command_0x02_depends_on_transport() {
        let raw = RawRpc {
            command: 0x02,
            data: &[],
        };
        assert_eq!(
            Command::parse(Transport::Serial, raw),
            Ok(Command::GetCurrentState)
        );
        assert_eq!(Command::parse(Transport::Ble, raw), Ok(Command::Identify));
    }

    #[test]
    fn network_state_is_serial_only() {
        let raw = RawRpc {
            command: 0x07,
            data: &[],
        };
        assert_eq!(
            Command::parse(Transport::Serial, raw),
            Ok(Command::NetworkState)
        );
        assert_eq!(
            Command::parse(Transport::Ble, raw),
            Err(ErrorCode::UnknownCommand)
        );
    }

    #[test]
    fn rejects_bad_wifi_payloads() {
        let bad: &[&[u8]] = &[
            &[],                    // no strings
            &[3, b'a', b'b', b'c'], // no password
            &[5, b'a'],             // truncated string
            &[0, 0],                // empty SSID
            &[1, b'a', 0, 1, b'x'], // trailing string
            &[1, 0xFF, 0],          // invalid UTF-8
        ];
        for data in bad {
            let raw = RawRpc {
                command: 0x01,
                data,
            };
            assert_eq!(
                Command::parse(Transport::Serial, raw),
                Err(ErrorCode::InvalidRpc),
                "{data:?}"
            );
        }
        let long = [33u8; 34];
        let raw = RawRpc {
            command: 0x01,
            data: &long,
        };
        assert_eq!(
            Command::parse(Transport::Serial, raw),
            Err(ErrorCode::InvalidRpc)
        );
    }

    #[test]
    fn encode_refuses_wrong_transport() {
        let mut buf = [0u8; 8];
        assert_eq!(
            Command::Identify.encode(Transport::Serial, &mut buf),
            Err(EncodeError::WrongTransport)
        );
        assert_eq!(
            Command::GetCurrentState.encode(Transport::Ble, &mut buf),
            Err(EncodeError::WrongTransport)
        );
        assert!(Command::GetCurrentState
            .encode(Transport::Serial, &mut buf)
            .is_ok());
    }

    #[test]
    fn hostname_get_and_set() {
        let get = RawRpc {
            command: 0x05,
            data: &[],
        };
        assert_eq!(
            Command::parse(Transport::Ble, get),
            Ok(Command::Hostname(None))
        );
        let set = RawRpc {
            command: 0x05,
            data: b"kitchen",
        };
        assert_eq!(
            Command::parse(Transport::Ble, set),
            Ok(Command::Hostname(Some("kitchen")))
        );
    }

    #[test]
    fn result_writer_strings_and_rewind() {
        let mut buf = [0u8; 32];
        let mut w = ResultWriter::new(&mut buf, Transport::Ble, 0x04).unwrap();
        w.push("net").unwrap().push_display(-60i8).unwrap();
        let mark = w.mark();
        w.push("discard").unwrap();
        w.rewind(mark);
        let frame = w.finish().unwrap();

        let raw = RawRpc::parse(frame, Transport::Ble).unwrap();
        let mut it = raw.strings();
        assert_eq!(it.next(), Some(Ok("net")));
        assert_eq!(it.next(), Some(Ok("-60")));
        assert_eq!(it.next(), None);
    }

    #[test]
    fn result_writer_failed_push_is_noop() {
        let mut buf = [0u8; 8];
        let mut w = ResultWriter::new(&mut buf, Transport::Ble, 0x03).unwrap();
        w.push("ab").unwrap();
        assert!(matches!(w.push("too long"), Err(EncodeError::Overflow)));
        let frame = w.finish().unwrap();
        assert_eq!(frame.len(), 2 + 3 + 1);
    }

    #[test]
    fn serial_result_respects_packet_limit() {
        let mut buf = [0u8; 512];
        let mut w = ResultWriter::new(&mut buf, Transport::Serial, 0x03).unwrap();
        let s = core::str::from_utf8(&[b'x'; 200]).unwrap();
        w.push(s).unwrap();
        assert!(w.push(&s[..60]).is_err()); // 201 + 61 > 253
        assert!(w.push(&s[..51]).is_ok()); // 201 + 52 = 253
        assert_eq!(w.finish().unwrap().len(), 255);
    }
}
