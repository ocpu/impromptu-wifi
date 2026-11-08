//! Wire format for the [Improv Wi-Fi](https://www.improv-wifi.com) provisioning protocol.
//!
//! This is the bottom layer of the `impromptu-wifi-*` stack. It has no opinions about
//! I/O, async runtimes, or network stacks: it only turns bytes into typed
//! values and back, into caller-provided buffers, without allocating.
//!
//! * [`types`]: states, error codes, command ids, and the [`Transport`] switch.
//! * [`rpc`]: RPC command/result frames, shared by both transports.
//! * [`serial`]: the `IMPROV` packet envelope and a byte-at-a-time [`serial::Framer`].
//! * [`ble`]: GATT UUIDs, capabilities, service data, and chunk reassembly.
//!
//! Decoding is total: malformed input produces a [`DecodeError`], never a panic.
#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod ble;
mod error;
pub mod rpc;
pub mod serial;
pub mod types;

pub use error::{DecodeError, EncodeError};
pub use rpc::{Command, RawRpc, ResultWriter};
pub use types::{command_id, ErrorCode, State, Transport, MAX_PASSWORD_LEN, MAX_SSID_LEN};

/// The protocol's checksum: the sum of all bytes, keeping only the low byte.
pub fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |acc, b| acc.wrapping_add(*b))
}

/// Whether `hostname` is acceptable for the hostname RPC command.
///
/// The spec allows only ASCII letters, digits and hyphens, up to 255
/// characters. Following RFC 1123, a hostname may not start or end with a
/// hyphen either.
pub fn is_valid_hostname(hostname: &str) -> bool {
    let b = hostname.as_bytes();
    !b.is_empty()
        && b.len() <= 255
        && b.first() != Some(&b'-')
        && b.last() != Some(&b'-')
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_wraps() {
        assert_eq!(checksum(&[0xFF, 0x02]), 0x01);
        assert_eq!(checksum(&[]), 0);
    }

    #[test]
    fn hostnames() {
        assert!(is_valid_hostname("living-room-1"));
        assert!(!is_valid_hostname(""));
        assert!(!is_valid_hostname("-lead"));
        assert!(!is_valid_hostname("trail-"));
        assert!(!is_valid_hostname("has.dot"));
        assert!(!is_valid_hostname("spa ce"));
    }
}
