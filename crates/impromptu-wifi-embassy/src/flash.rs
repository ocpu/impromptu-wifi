//! A [`CredentialStore`] in one erase sector of NOR flash.
//!
//! Works with any blocking `embedded-storage` 0.3 [`NorFlash`], such as
//! `esp_storage::FlashStorage` on the ESP32 family.
//!
//! # Record format
//!
//! One record at the start of the sector, 128 bytes:
//!
//! | Offset | Length | Content |
//! |---|---|---|
//! | 0 | 4 | Magic `IWC1` |
//! | 4 | 1 | SSID length |
//! | 5 | 1 | Passphrase length |
//! | 6 | 32 | SSID, zero-padded |
//! | 38 | 64 | Passphrase, zero-padded |
//! | 102 | 4 | CRC-32 (IEEE 802.3) of bytes 0..102, little-endian |
//! | 106 | 22 | `0xFF` |
//!
//! A sector without a valid record (erased, or written by something else)
//! loads as no credentials.
//!
//! # Limits
//!
//! * **Power loss during a save loses the credentials.** A save erases the
//!   sector, then writes the record; if power fails in between, the device
//!   boots unprovisioned and asks to be provisioned again. Saves happen
//!   only when the network changes, so the window is rare.
//! * **The passphrase is stored in plain text**, as ESP-IDF's NVS does by
//!   default. Anyone who can read the flash can read it; use flash
//!   encryption where that matters.

use embedded_storage::nor_flash::NorFlash;
use impromptu_wifi_core::traits::{CredentialStore, Credentials, MAX_PASSPHRASE_LEN, MAX_SSID_LEN};

const MAGIC: [u8; 4] = *b"IWC1";
const SSID_AT: usize = 6;
const PASS_AT: usize = SSID_AT + MAX_SSID_LEN;
const CRC_AT: usize = PASS_AT + MAX_PASSPHRASE_LEN;
/// The record, padded to a size every common flash read and write size
/// divides.
const RECORD_LEN: usize = 128;

/// Why a [`FlashStore`] operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum FlashStoreError<E> {
    /// The offset is not the start of an erase sector inside the flash, or
    /// the flash's sector or write sizes don't fit a 128-byte record.
    Region,
    /// The flash reported an error.
    Flash(E),
}

/// Credentials in the erase sector at `offset` of `flash`.
///
/// ```ignore
/// let mut flash = esp_storage::FlashStorage::new(peripherals.FLASH);
/// let offset = /* the start of a sector no one else uses, such as the `nvs` partition */;
/// let store = FlashStore::new(flash, offset)?;
/// ```
pub struct FlashStore<F> {
    flash: F,
    offset: u32,
}

impl<F: NorFlash> FlashStore<F> {
    /// Use the erase sector starting at `offset`. The whole sector is
    /// erased on every save.
    pub fn new(flash: F, offset: u32) -> Result<Self, FlashStoreError<F::Error>> {
        let sector = F::ERASE_SIZE as u32;
        let fits = F::ERASE_SIZE >= RECORD_LEN
            && RECORD_LEN.is_multiple_of(F::WRITE_SIZE)
            && RECORD_LEN.is_multiple_of(F::READ_SIZE)
            && offset.is_multiple_of(sector)
            && (offset as usize)
                .checked_add(F::ERASE_SIZE)
                .is_some_and(|end| end <= flash.capacity());
        if !fits {
            return Err(FlashStoreError::Region);
        }
        Ok(Self { flash, offset })
    }

    /// Give back the flash.
    pub fn into_inner(self) -> F {
        self.flash
    }

    fn erase(&mut self) -> Result<(), FlashStoreError<F::Error>> {
        self.flash
            .erase(self.offset, self.offset + F::ERASE_SIZE as u32)
            .map_err(FlashStoreError::Flash)
    }
}

impl<F: NorFlash> CredentialStore for FlashStore<F> {
    type Error = FlashStoreError<F::Error>;

    async fn load(&mut self) -> Result<Option<Credentials>, Self::Error> {
        let mut record = [0u8; RECORD_LEN];
        self.flash
            .read(self.offset, &mut record)
            .map_err(FlashStoreError::Flash)?;
        Ok(decode(&record))
    }

    async fn save(&mut self, credentials: &Credentials) -> Result<(), Self::Error> {
        let record = encode(credentials);
        self.erase()?;
        self.flash
            .write(self.offset, &record)
            .map_err(FlashStoreError::Flash)
    }

    async fn clear(&mut self) -> Result<(), Self::Error> {
        self.erase()
    }
}

fn encode(c: &Credentials) -> [u8; RECORD_LEN] {
    let mut r = [0u8; RECORD_LEN];
    let (ssid, pass) = (c.ssid.as_bytes(), c.password.as_bytes());
    r[..4].copy_from_slice(&MAGIC);
    // Both fit in a byte: at most 32 and 64.
    r[4] = ssid.len() as u8;
    r[5] = pass.len() as u8;
    r[SSID_AT..SSID_AT + ssid.len()].copy_from_slice(ssid);
    r[PASS_AT..PASS_AT + pass.len()].copy_from_slice(pass);
    let crc = crc32(&r[..CRC_AT]);
    r[CRC_AT..CRC_AT + 4].copy_from_slice(&crc.to_le_bytes());
    r[CRC_AT + 4..].fill(0xFF);
    r
}

fn decode(r: &[u8; RECORD_LEN]) -> Option<Credentials> {
    if r[..4] != MAGIC {
        return None;
    }
    let crc = u32::from_le_bytes(r[CRC_AT..CRC_AT + 4].try_into().ok()?);
    if crc != crc32(&r[..CRC_AT]) {
        return None;
    }
    let (ssid_len, pass_len) = (usize::from(r[4]), usize::from(r[5]));
    if ssid_len > MAX_SSID_LEN || pass_len > MAX_PASSPHRASE_LEN {
        return None;
    }
    let ssid = core::str::from_utf8(&r[SSID_AT..SSID_AT + ssid_len]).ok()?;
    let pass = core::str::from_utf8(&r[PASS_AT..PASS_AT + pass_len]).ok()?;
    Credentials::new(ssid, pass)
}

/// CRC-32 as in IEEE 802.3 (reflected, polynomial 0xEDB88320).
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_check_value() {
        // The standard check value for CRC-32/ISO-HDLC.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn records_round_trip_and_reject_damage() {
        let c = Credentials::new("home", "hunter22").unwrap();
        let mut r = encode(&c);
        assert_eq!(decode(&r), Some(c));
        r[SSID_AT] ^= 1;
        assert_eq!(decode(&r), None);
        assert_eq!(decode(&[0xFF; RECORD_LEN]), None);
    }
}
