//! The GAP and GATT services, with the characteristics that let clients
//! cache this server's GATT layout safely.
//!
//! Clients such as BlueZ and ESPHome Bluetooth proxies cache a device's GATT
//! layout by address. trouble-host's `GapConfig` adds an empty GATT service,
//! which gives a client no way to notice that the layout changed, for
//! example after a firmware update, so it keeps using the old handles.
//! [`add_gap_and_gatt`] adds the GATT service with the three characteristics
//! of robust caching (Bluetooth Core Specification, Vol 3, Part G, 7):
//!
//! | Characteristic | UUID | Properties |
//! |---|---|---|
//! | Service Changed | `0x2A05` | indicate |
//! | Client Supported Features | `0x2B29` | read, write |
//! | Database Hash | `0x2B2A` | read |
//!
//! A client reads the Database Hash on connecting and discovers the services
//! again if it differs from the one it cached.
//!
//! ```ignore
//! #[gatt_server(attribute_table_size = GAP_GATT_ATTRIBUTE_COUNT + ImprovWifiService::ATTRIBUTE_COUNT)]
//! struct Server {
//!     improv_wifi: ImprovWifiService,
//! }
//!
//! let mut table = AttributeTable::new();
//! let hash = add_gap_and_gatt(&mut table, "Impromptu", &appearance::UNKNOWN);
//! let server = Server::new(table);
//! hash.update(server.table())?;
//! ```
//!
//! # Limits
//!
//! The layout never changes while the firmware runs, so the server never
//! indicates Service Changed, and it doesn't track which clients are
//! change-aware (the "Database Out Of Sync" error of robust caching).
//! Client Supported Features is stored once, not per client.

use aes::Aes128;
use cmac::{Cmac, KeyInit, Mac};
use embassy_sync::blocking_mutex::raw::RawMutex;
use trouble_host::attribute::{AttributeHandle, AttributeTable, CharacteristicProp, Service};
use trouble_host::prelude::BluetoothUuid16;
use trouble_host::types::uuid::Uuid;

/// Attributes [`add_gap_and_gatt`] adds: the GAP service with Device Name
/// and Appearance (5), and the GATT service with Service Changed and its
/// CCCD, Client Supported Features and Database Hash (8). Use it in
/// `#[gatt_server(attribute_table_size = ...)]`.
pub const GAP_GATT_ATTRIBUTE_COUNT: usize = 13;

const GAP: u16 = 0x1800;
const GATT: u16 = 0x1801;
const DEVICE_NAME: u16 = 0x2A00;
const APPEARANCE: u16 = 0x2A01;
const SERVICE_CHANGED: u16 = 0x2A05;
const CLIENT_SUPPORTED_FEATURES: u16 = 0x2B29;
const DATABASE_HASH: u16 = 0x2B2A;

/// The Database Hash characteristic, to fill in once every service is in
/// the table.
#[derive(Debug, Clone, Copy)]
pub struct DatabaseHash {
    value_handle: u16,
}

/// Add the GAP service (device name and appearance) and the GATT service
/// with robust-caching characteristics to an empty `table`, in place of
/// trouble-host's `GapConfig`. Add the other services afterwards, then call
/// [`DatabaseHash::update`].
pub fn add_gap_and_gatt<'d, M: RawMutex, const MAX: usize>(
    table: &mut AttributeTable<'d, M, MAX>,
    name: &'d str,
    appearance: &'d BluetoothUuid16,
) -> DatabaseHash {
    let mut gap = table.add_service(Service::new(Uuid::new_short(GAP)));
    gap.add_characteristic_ro(Uuid::new_short(DEVICE_NAME), name);
    gap.add_characteristic_ro(Uuid::new_short(APPEARANCE), appearance);
    gap.build();

    let mut gatt = table.add_service(Service::new(Uuid::new_short(GATT)));
    // The affected handle range; never sent, as the layout doesn't change
    // at runtime.
    gatt.add_characteristic_small(
        Uuid::new_short(SERVICE_CHANGED),
        [CharacteristicProp::Indicate],
        [0u8; 4],
    )
    .build();
    gatt.add_characteristic_small(
        Uuid::new_short(CLIENT_SUPPORTED_FEATURES),
        [CharacteristicProp::Read, CharacteristicProp::Write],
        [0u8; 1],
    )
    .build();
    let hash = gatt
        .add_characteristic_small(
            Uuid::new_short(DATABASE_HASH),
            [CharacteristicProp::Read],
            [0u8; 16],
        )
        .build();
    gatt.build();

    DatabaseHash {
        value_handle: hash.handle(),
    }
}

impl DatabaseHash {
    /// Compute the hash of `table` and store it as the characteristic's
    /// value. Call it after the last service is added. Returns the hash.
    pub fn update<M: RawMutex, const MAX: usize>(
        &self,
        table: &AttributeTable<'_, M, MAX>,
    ) -> Result<u128, trouble_host::Error> {
        let hash = database_hash(table);
        // Sent least significant byte first, like every multi-octet value.
        table.write(self.value_handle, 0, &hash.to_le_bytes())?;
        Ok(hash)
    }
}

/// The Database Hash of `table`: AES-CMAC with a zero key over the handle,
/// type and (for declarations) value of the attributes that define the
/// layout (Bluetooth Core Specification, Vol 3, Part G, 7.3.1).
pub fn database_hash<M: RawMutex, const MAX: usize>(table: &AttributeTable<'_, M, MAX>) -> u128 {
    let mut mac = <Cmac<Aes128> as KeyInit>::new(&Default::default());
    let mut value = [0u8; 32];
    for handle in 1..=table.len() as u16 {
        let Some(uuid) = table.uuid(handle) else {
            continue;
        };
        let raw = uuid.as_raw();
        let short = match raw {
            [lo, hi] => u16::from_le_bytes([*lo, *hi]),
            _ => continue,
        };
        match short {
            // Primary and secondary service, include, characteristic
            // declarations and Characteristic Extended Properties: handle,
            // type and value.
            0x2800 | 0x2801 | 0x2802 | 0x2803 | 0x2900 => {
                mac.update(&handle.to_le_bytes());
                mac.update(raw);
                let len = table.read(handle, 0, &mut value).unwrap_or(0);
                mac.update(&value[..len]);
            }
            // User Description, Client and Server Characteristic
            // Configuration, Presentation and Aggregate Format: handle and
            // type only.
            0x2901..=0x2905 => {
                mac.update(&handle.to_le_bytes());
                mac.update(raw);
            }
            _ => {}
        }
    }
    u128::from_be_bytes(mac.finalize().into_bytes().into())
}

#[cfg(test)]
mod tests {
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use trouble_host::prelude::appearance;

    use super::*;

    // Reference values from trouble-host 0.7.0's own `database_hash` test,
    // which were computed with `openssl mac -cipher AES-128-CBC CMAC`.
    #[test]
    fn hash_matches_reference_values() {
        let mut table: AttributeTable<'static, NoopRawMutex, 8> = AttributeTable::new();
        let gap = table.add_service(Service::new(Uuid::new_short(GAP)));
        gap.build();
        assert_eq!(database_hash(&table), 0xd4cdec10804db3f147b4d7d10baa0120);

        let mut table: AttributeTable<'static, NoopRawMutex, 8> = AttributeTable::new();
        let mut gap = table.add_service(Service::new(Uuid::new_short(GAP)));
        gap.add_characteristic_ro(Uuid::new_short(DEVICE_NAME), "");
        gap.add_characteristic_ro(Uuid::new_short(APPEARANCE), "");
        gap.build();
        assert_eq!(database_hash(&table), 0x6c329e3f1d52c03f174980f6b4704875);
    }

    #[test]
    fn the_hash_is_stored_least_significant_byte_first() {
        let mut table: AttributeTable<'static, NoopRawMutex, 16> = AttributeTable::new();
        let hash = add_gap_and_gatt(&mut table, "Test", &appearance::UNKNOWN);
        assert_eq!(table.len(), GAP_GATT_ATTRIBUTE_COUNT);
        let value = hash.update(&table).unwrap();

        let mut read = [0u8; 16];
        assert_eq!(table.read(hash.value_handle, 0, &mut read).unwrap(), 16);
        assert_eq!(read, value.to_le_bytes());
    }

    #[test]
    fn values_that_are_not_declarations_do_not_change_the_hash() {
        let build = |name: &'static str| {
            let mut table: AttributeTable<'static, NoopRawMutex, 16> = AttributeTable::new();
            add_gap_and_gatt(&mut table, name, &appearance::UNKNOWN);
            database_hash(&table)
        };
        assert_eq!(build("one"), build("another"));
    }
}
