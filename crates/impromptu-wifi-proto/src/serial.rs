//! The serial packet envelope.
//!
//! ```text
//! "IMPROV" | version (1) | type | length | data … | checksum
//! ```
//!
//! The checksum covers every byte before it, header included.

use crate::{checksum, DecodeError, EncodeError, ErrorCode, RawRpc, State, Transport};

/// Packet header.
pub const HEADER: [u8; 6] = *b"IMPROV";
/// Supported protocol version.
pub const VERSION: u8 = 1;
/// Bytes in a packet besides its data: header, version, type, length, checksum.
pub const OVERHEAD: usize = HEADER.len() + 4;
/// Largest possible packet.
pub const MAX_PACKET_LEN: usize = OVERHEAD + 255;

/// Serial packet types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum PacketType {
    /// Device → client: one [`State`] byte.
    CurrentState = 0x01,
    /// Device → client: one [`ErrorCode`] byte.
    ErrorState = 0x02,
    /// Client → device: an RPC command frame.
    Rpc = 0x03,
    /// Device → client: an RPC result frame.
    RpcResult = 0x04,
}

impl TryFrom<u8> for PacketType {
    type Error = u8;
    fn try_from(v: u8) -> Result<Self, u8> {
        Ok(match v {
            0x01 => PacketType::CurrentState,
            0x02 => PacketType::ErrorState,
            0x03 => PacketType::Rpc,
            0x04 => PacketType::RpcResult,
            other => return Err(other),
        })
    }
}

/// A decoded packet borrowing its data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Packet<'a> {
    /// Packet type.
    pub ty: PacketType,
    /// Data section.
    pub data: &'a [u8],
}

impl<'a> Packet<'a> {
    /// Decode one packet from the start of `buf`, returning it and the
    /// number of bytes it occupied.
    pub fn decode(buf: &'a [u8]) -> Result<(Self, usize), DecodeError> {
        let prefix = buf.len().min(HEADER.len());
        if buf[..prefix] != HEADER[..prefix] {
            return Err(DecodeError::BadHeader);
        }
        if buf.len() < OVERHEAD - 1 {
            return Err(DecodeError::Incomplete);
        }
        if buf[6] != VERSION {
            return Err(DecodeError::BadVersion(buf[6]));
        }
        let ty = PacketType::try_from(buf[7]).map_err(DecodeError::UnknownPacketType)?;
        let total = OVERHEAD + usize::from(buf[8]);
        if buf.len() < total {
            return Err(DecodeError::Incomplete);
        }
        if checksum(&buf[..total - 1]) != buf[total - 1] {
            return Err(DecodeError::Checksum);
        }
        Ok((
            Packet {
                ty,
                data: &buf[9..total - 1],
            },
            total,
        ))
    }

    /// Interpret an [`PacketType::Rpc`] packet's data as an RPC frame.
    pub fn rpc(&self) -> Option<Result<RawRpc<'a>, DecodeError>> {
        (self.ty == PacketType::Rpc).then(|| RawRpc::parse(self.data, Transport::Serial))
    }
}

/// Encode a packet into `out`, returning the number of bytes written.
pub fn encode_packet(out: &mut [u8], ty: PacketType, data: &[u8]) -> Result<usize, EncodeError> {
    let total = OVERHEAD + data.len();
    if data.len() > 255 || out.len() < total {
        return Err(EncodeError::Overflow);
    }
    out[..6].copy_from_slice(&HEADER);
    out[6] = VERSION;
    out[7] = ty as u8;
    out[8] = data.len() as u8;
    out[9..9 + data.len()].copy_from_slice(data);
    out[total - 1] = checksum(&out[..total - 1]);
    Ok(total)
}

/// Encode a current-state packet.
pub fn encode_state(out: &mut [u8], state: State) -> Result<usize, EncodeError> {
    encode_packet(out, PacketType::CurrentState, &[state as u8])
}

/// Encode an error-state packet.
pub fn encode_error(out: &mut [u8], error: ErrorCode) -> Result<usize, EncodeError> {
    encode_packet(out, PacketType::ErrorState, &[error as u8])
}

/// What happened after feeding a byte to a [`Framer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Feed {
    /// Nothing complete yet (or the byte was skipped as noise).
    Pending,
    /// A valid packet is ready; read it with [`Framer::packet`].
    Packet,
    /// A packet started but was invalid. The framer has reset and is
    /// hunting for the next header.
    Error(DecodeError),
}

/// Finds packets in a byte stream that may also contain other traffic.
///
/// Serial lines are often shared with log output, so the framer skips
/// anything that isn't an `IMPROV` header and resynchronises after errors.
/// It holds no references and can be fed across `await` points, which makes
/// it cancel-safe when stored in a transport struct.
///
/// `N` must be at least [`OVERHEAD`]; the default fits any packet.
#[derive(Debug, Clone)]
pub struct Framer<const N: usize = MAX_PACKET_LEN> {
    buf: [u8; N],
    len: usize,
    ready: bool,
}

impl<const N: usize> Default for Framer<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Framer<N> {
    const SIZE_OK: () = assert!(
        N >= OVERHEAD,
        "Framer buffer must hold at least OVERHEAD bytes"
    );

    /// An empty framer.
    pub const fn new() -> Self {
        #[allow(clippy::let_unit_value)]
        let () = Self::SIZE_OK;
        Self {
            buf: [0; N],
            len: 0,
            ready: false,
        }
    }

    /// Discard any partial packet.
    pub fn reset(&mut self) {
        self.len = 0;
        self.ready = false;
    }

    /// Feed one byte.
    pub fn feed(&mut self, byte: u8) -> Feed {
        if self.ready {
            self.reset();
        }

        // Hunting for the header. "IMPROV" has no repeated prefix, so on a
        // mismatch the only possible restart point is this byte.
        if self.len < HEADER.len() {
            if byte == HEADER[self.len] {
                self.buf[self.len] = byte;
                self.len += 1;
            } else if byte == HEADER[0] {
                self.buf[0] = byte;
                self.len = 1;
            } else {
                self.len = 0;
            }
            return Feed::Pending;
        }

        self.buf[self.len] = byte;
        self.len += 1;

        if self.len == HEADER.len() + 1 && byte != VERSION {
            self.reset();
            return Feed::Error(DecodeError::BadVersion(byte));
        }
        if self.len < OVERHEAD - 1 {
            return Feed::Pending;
        }
        let total = OVERHEAD + usize::from(self.buf[8]);
        if total > N {
            self.reset();
            return Feed::Error(DecodeError::TooLarge);
        }
        if self.len < total {
            return Feed::Pending;
        }
        match Packet::decode(&self.buf[..total]) {
            Ok(_) => {
                self.ready = true;
                Feed::Packet
            }
            Err(e) => {
                self.reset();
                Feed::Error(e)
            }
        }
    }

    /// The packet completed by the last [`feed`](Self::feed), if any.
    /// Valid until the next byte is fed.
    pub fn packet(&self) -> Option<Packet<'_>> {
        if !self.ready {
            return None;
        }
        Packet::decode(&self.buf[..self.len]).ok().map(|(p, _)| p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Command;

    fn rpc_packet(cmd: Command<'_>, out: &mut [u8]) -> usize {
        let mut rpc = [0u8; 258];
        let n = cmd.encode(Transport::Serial, &mut rpc).unwrap();
        encode_packet(out, PacketType::Rpc, &rpc[..n]).unwrap()
    }

    #[test]
    fn encode_decode_roundtrip() {
        let mut buf = [0u8; MAX_PACKET_LEN];
        let n = encode_state(&mut buf, State::Provisioned).unwrap();
        assert_eq!(&buf[..n - 1], b"IMPROV\x01\x01\x01\x04");
        let (p, used) = Packet::decode(&buf[..n]).unwrap();
        assert_eq!(used, n);
        assert_eq!(p.ty, PacketType::CurrentState);
        assert_eq!(p.data, &[State::Provisioned as u8]);
    }

    #[test]
    fn framer_skips_log_noise() {
        let mut stream = [0u8; 600];
        let noise = b"[I][wifi:123] IMPRESSIVE log line\r\nIIMPROV";
        stream[..noise.len()].copy_from_slice(noise);
        // The packet begins with the last "IMPROV" above minus the extra 'I'.
        let start = noise.len() - HEADER.len();
        let n = rpc_packet(Command::DeviceInfo, &mut stream[start..]);
        let end = start + n;

        let mut framer: Framer = Framer::new();
        let mut found = 0;
        for &b in &stream[..end] {
            if framer.feed(b) == Feed::Packet {
                let p = framer.packet().unwrap();
                let raw = p.rpc().unwrap().unwrap();
                assert_eq!(
                    Command::parse(Transport::Serial, raw),
                    Ok(Command::DeviceInfo)
                );
                found += 1;
            }
        }
        assert_eq!(found, 1);
    }

    #[test]
    fn framer_reports_bad_checksum_then_recovers() {
        let mut buf = [0u8; 64];
        let n = rpc_packet(Command::ScanNetworks, &mut buf);
        let mut framer: Framer = Framer::new();

        let mut corrupt = buf;
        corrupt[n - 1] ^= 0x55;
        let results: [Feed; 2] = [&corrupt[..n], &buf[..n]]
            .map(|bytes| bytes.iter().map(|&b| framer.feed(b)).last().unwrap());
        assert_eq!(results, [Feed::Error(DecodeError::Checksum), Feed::Packet]);
    }

    #[test]
    fn framer_rejects_oversize_for_small_buffer() {
        let mut buf = [0u8; 64];
        let n = encode_packet(&mut buf, PacketType::RpcResult, &[0u8; 40]).unwrap();
        let mut framer = Framer::<20>::new();
        let last = buf[..n]
            .iter()
            .map(|&b| framer.feed(b))
            .find(|f| *f != Feed::Pending);
        assert_eq!(last, Some(Feed::Error(DecodeError::TooLarge)));
    }
}
