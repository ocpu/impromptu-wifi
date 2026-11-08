//! BLE-specific constants and helpers. The GATT server itself lives
//! higher up the stack; this module only knows the bytes.

use core::ops::BitOr;

use crate::{DecodeError, RawRpc, State, Transport};

/// Improv primary service.
pub const SERVICE_UUID: u128 = 0x00467768_6228_2272_4663_277478268000;
/// Current state characteristic (read, notify).
pub const CURRENT_STATE_UUID: u128 = 0x00467768_6228_2272_4663_277478268001;
/// Error state characteristic (read, notify).
pub const ERROR_STATE_UUID: u128 = 0x00467768_6228_2272_4663_277478268002;
/// RPC command characteristic (write).
pub const RPC_COMMAND_UUID: u128 = 0x00467768_6228_2272_4663_277478268003;
/// RPC result characteristic (read, notify).
pub const RPC_RESULT_UUID: u128 = 0x00467768_6228_2272_4663_277478268004;
/// Capabilities characteristic (read).
pub const CAPABILITIES_UUID: u128 = 0x00467768_6228_2272_4663_277478268005;
/// 16-bit UUID under which [`service_data`] is advertised.
pub const SERVICE_DATA_UUID16: u16 = 0x4677;
/// [`SERVICE_DATA_UUID16`] expanded to 128 bits on the Bluetooth base UUID,
/// as most host BLE APIs report it.
pub const SERVICE_DATA_UUID: u128 = 0x00004677_0000_1000_8000_00805f9b34fb;

/// The capabilities bitfield.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Capabilities(u8);

impl Capabilities {
    /// No optional commands.
    pub const NONE: Self = Self(0);
    /// Supports the identify command.
    pub const IDENTIFY: Self = Self(1 << 0);
    /// Supports the device info command.
    pub const DEVICE_INFO: Self = Self(1 << 1);
    /// Supports the scan command.
    pub const SCAN: Self = Self(1 << 2);
    /// Supports the hostname command.
    pub const HOSTNAME: Self = Self(1 << 3);

    /// From a raw characteristic value. Unknown bits are kept, so newer
    /// capabilities survive a round trip.
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// The raw characteristic value.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Both sets combined. Usable in `const` contexts, unlike `|`.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether every capability in `other` is present.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for Capabilities {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// The 6-byte service data advertised alongside [`SERVICE_UUID`].
pub const fn service_data(state: State, caps: Capabilities) -> [u8; 6] {
    [state as u8, caps.bits(), 0, 0, 0, 0]
}

/// Length of a legacy BLE advertisement payload.
pub const ADVERTISING_DATA_LEN: usize = 31;

/// A complete legacy advertisement payload for an Improv Wi-Fi device, in
/// the AD structure format of the Bluetooth Core Specification (Vol 3,
/// Part C, Section 11), ready to hand to any BLE stack:
///
/// | Bytes | AD type | Content |
/// |---|---|---|
/// | 0–2 | `0x01` Flags | `0x06`: LE General Discoverable, BR/EDR not supported |
/// | 3–20 | `0x07` Complete list of 128-bit service UUIDs | [`SERVICE_UUID`] |
/// | 21–30 | `0x16` Service data, 16-bit UUID | `0x4677` and [`service_data`] |
///
/// The spec requires the service UUID and the service data in the same
/// advertisement, not the scan response; together with the flags they fill
/// all 31 bytes, so a device name has to go in the scan response.
pub const fn advertising_data(state: State, caps: Capabilities) -> [u8; ADVERTISING_DATA_LEN] {
    let mut ad = [0u8; ADVERTISING_DATA_LEN];
    // Flags.
    ad[0] = 2;
    ad[1] = 0x01;
    ad[2] = 0x06;
    // Complete list of 128-bit service UUIDs, least significant byte first.
    ad[3] = 17;
    ad[4] = 0x07;
    let uuid = SERVICE_UUID.to_le_bytes();
    let mut i = 0;
    while i < 16 {
        ad[5 + i] = uuid[i];
        i += 1;
    }
    // Service data for the 16-bit UUID 0x4677.
    ad[21] = 9;
    ad[22] = 0x16;
    let id = SERVICE_DATA_UUID16.to_le_bytes();
    ad[23] = id[0];
    ad[24] = id[1];
    let data = service_data(state, caps);
    let mut j = 0;
    while j < 6 {
        ad[25 + j] = data[j];
        j += 1;
    }
    ad
}

/// Read the state and capabilities from advertised service data, so a client
/// can list devices without connecting.
///
/// The spec defines six bytes (state, capabilities, four reserved); only the
/// first two are needed, so shorter data from lax devices is accepted.
/// Returns `None` if they're missing or the state is unknown.
pub fn parse_service_data(data: &[u8]) -> Option<(State, Capabilities)> {
    let state = State::try_from(*data.first()?).ok()?;
    let caps = Capabilities::from_bits(*data.get(1)?);
    Some((state, caps))
}

/// Largest write a BLE client can assume without negotiating a larger MTU:
/// the default ATT MTU of 23 bytes minus the 3-byte write header.
pub const DEFAULT_WRITE_LEN: usize = 20;

/// Split an encoded RPC command into characteristic writes of at most
/// `max_write_len` bytes, for a client to send in order. The device side
/// reassembles them with [`RpcAssembler`].
///
/// ```
/// use impromptu_wifi_proto::{ble, Command, Transport};
/// let mut buf = [0u8; ble::MAX_RPC_FRAME];
/// let n = Command::WifiSettings { ssid: "MyWirelessAP", password: "mysecurepassword" }
///     .encode(Transport::Ble, &mut buf).unwrap();
/// assert_eq!(ble::write_chunks(&buf[..n], ble::DEFAULT_WRITE_LEN).count(), 2);
/// ```
pub fn write_chunks(frame: &[u8], max_write_len: usize) -> core::slice::Chunks<'_, u8> {
    frame.chunks(max_write_len.max(1))
}

/// Largest BLE RPC frame: command, length, 255 data bytes, checksum.
pub const MAX_RPC_FRAME: usize = 3 + 255;

/// What happened after pushing a chunk to an [`RpcAssembler`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Assembly {
    /// More chunks are needed.
    Pending,
    /// A complete, checksum-valid frame is ready.
    Complete,
    /// The frame was invalid; the assembler has reset.
    Error(DecodeError),
}

/// Reassembles RPC commands written in several chunks.
///
/// Clients write in pieces of about 20 bytes (one ATT write at the default
/// MTU), so a credentials frame usually spans several writes.
///
/// Platform code should [`reset`](Self::reset) on disconnect, and ideally
/// after a short idle timeout, so that a half-written frame can't poison
/// the next one.
#[derive(Debug, Clone)]
pub struct RpcAssembler<const N: usize = MAX_RPC_FRAME> {
    buf: [u8; N],
    len: usize,
    ready: bool,
}

impl<const N: usize> Default for RpcAssembler<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> RpcAssembler<N> {
    /// An empty assembler.
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
            ready: false,
        }
    }

    /// Discard any partial frame.
    pub fn reset(&mut self) {
        self.len = 0;
        self.ready = false;
    }

    /// Append one characteristic write.
    pub fn push(&mut self, chunk: &[u8]) -> Assembly {
        if self.ready {
            self.reset();
        }
        let end = self.len + chunk.len();
        if end > N {
            self.reset();
            return Assembly::Error(DecodeError::TooLarge);
        }
        self.buf[self.len..end].copy_from_slice(chunk);
        self.len = end;

        if self.len < 2 {
            return Assembly::Pending;
        }
        let total = 3 + usize::from(self.buf[1]);
        if total > N {
            self.reset();
            return Assembly::Error(DecodeError::TooLarge);
        }
        if self.len < total {
            return Assembly::Pending;
        }
        if self.len > total {
            self.reset();
            return Assembly::Error(DecodeError::LengthMismatch);
        }
        match RawRpc::parse(&self.buf[..total], Transport::Ble) {
            Ok(_) => {
                self.ready = true;
                Assembly::Complete
            }
            Err(e) => {
                self.reset();
                Assembly::Error(e)
            }
        }
    }

    /// The completed frame, checksum included. Valid until the next push.
    pub fn frame(&self) -> Option<&[u8]> {
        self.ready.then(|| &self.buf[..self.len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertising_data_is_three_ad_structures_in_31_bytes() {
        let caps = Capabilities::DEVICE_INFO | Capabilities::SCAN;
        let ad = advertising_data(State::Authorized, caps);
        // Walk the AD structures: each is length, type, data.
        let mut found: [(u8, &[u8]); 3] = [(0, &[]); 3];
        let (mut i, mut n) = (0, 0);
        while i < ad.len() {
            let len = usize::from(ad[i]);
            found[n] = (ad[i + 1], &ad[i + 2..i + 1 + len]);
            i += 1 + len;
            n += 1;
        }
        assert_eq!(n, 3, "three AD structures");
        assert_eq!(i, ADVERTISING_DATA_LEN, "the structures fill it exactly");
        assert_eq!(found[0], (0x01, &[0x06][..]));
        assert_eq!(found[1].0, 0x07);
        assert_eq!(
            found[1].1,
            &[
                0x00, 0x80, 0x26, 0x78, 0x74, 0x27, 0x63, 0x46, 0x72, 0x22, 0x28, 0x62, 0x68, 0x77,
                0x46, 0x00
            ][..],
            "00467768-6228-2272-4663-277478268000, least significant byte first"
        );
        assert_eq!(found[2].0, 0x16);
        assert_eq!(
            found[2].1,
            &[0x77, 0x46, 0x02, caps.bits(), 0, 0, 0, 0][..],
            "UUID 0x4677, then state and capabilities"
        );
    }
    use crate::Command;

    #[test]
    fn reassembles_20_byte_chunks() {
        let mut buf = [0u8; MAX_RPC_FRAME];
        let cmd = Command::WifiSettings {
            ssid: "MyWirelessAP",
            password: "mysecurepassword",
        };
        let n = cmd.encode(Transport::Ble, &mut buf).unwrap();
        assert!(n > 20);

        let mut asm: RpcAssembler = RpcAssembler::new();
        let mut chunks = buf[..n].chunks(20).peekable();
        while let Some(chunk) = chunks.next() {
            let r = asm.push(chunk);
            if chunks.peek().is_some() {
                assert_eq!(r, Assembly::Pending);
            } else {
                assert_eq!(r, Assembly::Complete);
            }
        }
        let raw = RawRpc::parse(asm.frame().unwrap(), Transport::Ble).unwrap();
        assert_eq!(Command::parse(Transport::Ble, raw), Ok(cmd));
    }

    #[test]
    fn chunker_and_assembler_agree_at_every_size() {
        let mut buf = [0u8; MAX_RPC_FRAME];
        let cmd = Command::WifiSettings {
            ssid: "a-reasonably-long-ssid",
            password: "and-a-passphrase-too",
        };
        let n = cmd.encode(Transport::Ble, &mut buf).unwrap();
        for size in [0, 1, 7, 20, 64, 512] {
            let mut asm: RpcAssembler = RpcAssembler::new();
            let last = write_chunks(&buf[..n], size).map(|c| asm.push(c)).last();
            assert_eq!(last, Some(Assembly::Complete), "write size {size}");
            assert_eq!(asm.frame(), Some(&buf[..n]));
        }
    }

    #[test]
    fn overlong_write_is_rejected() {
        let mut asm: RpcAssembler = RpcAssembler::new();
        assert_eq!(
            asm.push(&[0x03, 0x00, 0x03, 0xAA]),
            Assembly::Error(DecodeError::LengthMismatch)
        );
        assert_eq!(asm.push(&[0x03, 0x00, 0x03]), Assembly::Complete);
    }

    #[test]
    fn service_data_round_trip() {
        let caps = Capabilities::IDENTIFY | Capabilities::SCAN;
        let data = service_data(State::AuthorizationRequired, caps);
        assert_eq!(
            parse_service_data(&data),
            Some((State::AuthorizationRequired, caps))
        );
        assert_eq!(
            parse_service_data(&[0x04, 0x02]),
            Some((State::Provisioned, Capabilities::DEVICE_INFO))
        );
        assert_eq!(parse_service_data(&[0x09, 0x00]), None, "unknown state");
        assert_eq!(parse_service_data(&[0x02]), None, "too short");
    }

    #[test]
    fn service_data_layout() {
        let caps = Capabilities::IDENTIFY | Capabilities::SCAN;
        assert_eq!(
            service_data(State::Authorized, caps),
            [0x02, 0x05, 0, 0, 0, 0]
        );
    }
}
