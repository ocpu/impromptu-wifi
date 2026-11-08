//! Improv over Bluetooth LE through BlueZ, using `bluer`.
//!
//! [`BluezGatt::start`] registers the Improv GATT service and implements
//! [`GattServer`] for [`BleLink`](impromptu_wifi_ble::BleLink). Dropping it
//! unregisters the service and stops advertising.
//!
//! * **Reads** honour the offset BlueZ passes for long reads: an RPC result
//!   can be 258 bytes, more than one packet, and BlueZ fetches the rest with
//!   increasing offsets. Ignoring the offset would hand the client the start
//!   of the value repeatedly.
//! * **Notifications** carry the whole value; BlueZ cuts them to the MTU,
//!   and clients read the full RPC result after being notified.
//! * **Advertising** carries the service UUID and the service data, which
//!   together fill a legacy 31-byte advertisement exactly, so the device
//!   name isn't included. The spec requires both in the same advertisement
//!   rather than the scan response. Each state change re-registers the
//!   advertisement with fresh service data.
//! * One client at a time, as Improv intends: a new subscriber replaces the
//!   previous one's notifications. A client that disconnects mid-command
//!   can leave a partial frame, which the next client sees as one invalid
//!   command before things recover.
//!
//! Needs BlueZ (`bluetoothd`) and permission to use it over D-Bus: root, or
//! a user in the `bluetooth` group on most distributions.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex};

use bluer::adv::{Advertisement, AdvertisementHandle, Type};
use bluer::gatt::local::{
    Application, ApplicationHandle, Characteristic, CharacteristicNotifier, CharacteristicNotify,
    CharacteristicNotifyMethod, CharacteristicRead, CharacteristicWrite, CharacteristicWriteMethod,
    ReqError, Service,
};
use bluer::{Adapter, Uuid};
use futures::FutureExt;
use impromptu_wifi_ble::GattServer;
use impromptu_wifi_core::proto::ble::{
    Capabilities, CAPABILITIES_UUID, CURRENT_STATE_UUID, ERROR_STATE_UUID, RPC_COMMAND_UUID,
    RPC_RESULT_UUID, SERVICE_DATA_UUID, SERVICE_UUID,
};
use tokio::sync::mpsc;

/// Why the BlueZ side failed.
#[derive(Debug)]
pub enum BluezError {
    /// BlueZ refused or failed an operation.
    Bluer(bluer::Error),
    /// The GATT application was unregistered underneath us.
    Closed,
}

impl fmt::Display for BluezError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bluer(e) => write!(f, "BlueZ: {e}"),
            Self::Closed => f.write_str("the Improv GATT service was unregistered"),
        }
    }
}

impl std::error::Error for BluezError {}

impl From<bluer::Error> for BluezError {
    fn from(e: bluer::Error) -> Self {
        Self::Bluer(e)
    }
}

/// Current characteristic values, for reads.
#[derive(Default)]
struct Values {
    state: u8,
    error: u8,
    result: Vec<u8>,
}

/// Where a subscriber's notifier is kept, per characteristic.
type Slot = Arc<tokio::sync::Mutex<Option<CharacteristicNotifier>>>;

/// The Improv GATT service on one BlueZ adapter.
pub struct BluezGatt {
    adapter: Adapter,
    values: Arc<Mutex<Values>>,
    writes: mpsc::Receiver<Vec<u8>>,
    state_slot: Slot,
    error_slot: Slot,
    result_slot: Slot,
    advertisement: Option<AdvertisementHandle>,
    _app: ApplicationHandle,
}

/// The part of `value` a read at `offset` returns.
pub(crate) fn read_at(value: &[u8], offset: u16) -> Result<Vec<u8>, ReqError> {
    value
        .get(usize::from(offset)..)
        .map(<[u8]>::to_vec)
        .ok_or(ReqError::InvalidOffset)
}

fn readable(values: &Arc<Mutex<Values>>, get: fn(&Values) -> Vec<u8>) -> CharacteristicRead {
    let values = values.clone();
    CharacteristicRead {
        read: true,
        fun: Box::new(move |req| {
            let value = get(&values.lock().unwrap());
            async move { read_at(&value, req.offset) }.boxed()
        }),
        ..Default::default()
    }
}

fn notifying(slot: &Slot) -> CharacteristicNotify {
    let slot = slot.clone();
    CharacteristicNotify {
        notify: true,
        method: CharacteristicNotifyMethod::Fun(Box::new(move |notifier| {
            let slot = slot.clone();
            async move {
                *slot.lock().await = Some(notifier);
            }
            .boxed()
        })),
        ..Default::default()
    }
}

async fn notify(slot: &Slot, value: Vec<u8>) {
    let mut guard = slot.lock().await;
    if let Some(n) = guard.as_mut() {
        if n.notify(value).await.is_err() {
            // The client unsubscribed or disconnected.
            *guard = None;
        }
    }
}

impl BluezGatt {
    /// Power on `adapter` and register the Improv service with `caps` (see
    /// [`impromptu_wifi_ble::capabilities`]). Advertising starts with the driver's
    /// first state update.
    pub async fn start(adapter: &Adapter, caps: Capabilities) -> Result<Self, BluezError> {
        adapter.set_powered(true).await?;

        let values = Arc::new(Mutex::new(Values::default()));
        let (tx, writes) = mpsc::channel::<Vec<u8>>(16);
        let (state_slot, error_slot, result_slot) =
            (Slot::default(), Slot::default(), Slot::default());

        let rpc_command = Characteristic {
            uuid: Uuid::from_u128(RPC_COMMAND_UUID),
            write: Some(CharacteristicWrite {
                write: true,
                write_without_response: true,
                method: CharacteristicWriteMethod::Fun(Box::new(move |value, _req| {
                    // Chunks arrive in order (long writes too), so they can go
                    // straight to the driver's reassembly.
                    let tx = tx.clone();
                    async move { tx.send(value).await.map_err(|_| ReqError::Failed) }.boxed()
                })),
                ..Default::default()
            }),
            ..Default::default()
        };
        let caps_bits = caps.bits();
        let service = Service {
            uuid: Uuid::from_u128(SERVICE_UUID),
            primary: true,
            characteristics: vec![
                Characteristic {
                    uuid: Uuid::from_u128(CURRENT_STATE_UUID),
                    read: Some(readable(&values, |v| vec![v.state])),
                    notify: Some(notifying(&state_slot)),
                    ..Default::default()
                },
                Characteristic {
                    uuid: Uuid::from_u128(ERROR_STATE_UUID),
                    read: Some(readable(&values, |v| vec![v.error])),
                    notify: Some(notifying(&error_slot)),
                    ..Default::default()
                },
                rpc_command,
                Characteristic {
                    uuid: Uuid::from_u128(RPC_RESULT_UUID),
                    read: Some(readable(&values, |v| v.result.clone())),
                    notify: Some(notifying(&result_slot)),
                    ..Default::default()
                },
                Characteristic {
                    uuid: Uuid::from_u128(CAPABILITIES_UUID),
                    read: Some(CharacteristicRead {
                        read: true,
                        fun: Box::new(move |req| {
                            async move { read_at(&[caps_bits], req.offset) }.boxed()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let app = adapter
            .serve_gatt_application(Application {
                services: vec![service],
                ..Default::default()
            })
            .await?;

        Ok(Self {
            adapter: adapter.clone(),
            values,
            writes,
            state_slot,
            error_slot,
            result_slot,
            advertisement: None,
            _app: app,
        })
    }
}

impl GattServer for BluezGatt {
    type Error = BluezError;

    async fn rpc_command_written(&mut self, buf: &mut [u8]) -> Result<usize, BluezError> {
        let chunk = self.writes.recv().await.ok_or(BluezError::Closed)?;
        let n = chunk.len().min(buf.len());
        buf[..n].copy_from_slice(&chunk[..n]);
        Ok(n)
    }

    async fn set_state(&mut self, value: u8) -> Result<(), BluezError> {
        self.values.lock().unwrap().state = value;
        notify(&self.state_slot, vec![value]).await;
        Ok(())
    }

    async fn set_error(&mut self, value: u8) -> Result<(), BluezError> {
        self.values.lock().unwrap().error = value;
        notify(&self.error_slot, vec![value]).await;
        Ok(())
    }

    async fn set_rpc_result(&mut self, value: &[u8]) -> Result<(), BluezError> {
        self.values.lock().unwrap().result = value.to_vec();
        notify(&self.result_slot, value.to_vec()).await;
        Ok(())
    }

    async fn set_service_data(&mut self, data: [u8; 6]) -> Result<(), BluezError> {
        // Release the old advertisement first: adapters allow only a few.
        self.advertisement = None;
        let advertisement = Advertisement {
            advertisement_type: Type::Peripheral,
            service_uuids: BTreeSet::from([Uuid::from_u128(SERVICE_UUID)]),
            service_data: BTreeMap::from([(Uuid::from_u128(SERVICE_DATA_UUID), data.to_vec())]),
            discoverable: Some(true),
            ..Default::default()
        };
        match self.adapter.advertise(advertisement).await {
            Ok(handle) => self.advertisement = Some(handle),
            // Provisioning still works for a client that's already connected,
            // so a failed advertisement shouldn't end the session.
            Err(e) => log::warn!("advertising failed: {e}"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_reads_return_the_rest_of_the_value() {
        let value: Vec<u8> = (0..=255).collect();
        assert_eq!(read_at(&value, 0).unwrap().len(), 256);
        assert_eq!(read_at(&value, 22).unwrap(), &value[22..]);
        assert_eq!(
            read_at(&value, 256).unwrap(),
            Vec::<u8>::new(),
            "reading at the end is empty, not an error"
        );
        assert!(matches!(read_at(&value, 257), Err(ReqError::InvalidOffset)));
    }
}
