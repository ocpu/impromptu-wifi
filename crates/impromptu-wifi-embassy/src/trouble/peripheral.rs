//! The device side: the Improv GATT service and a [`GattServer`] for
//! `trouble-host`.
//!
//! `trouble-host` serves GATT through one event loop per connection
//! (`GattConnection::next`). If the Improv driver ran that loop itself, the
//! client's reads would go unanswered during a Wi-Fi join, which can take
//! many seconds. So the work is split across two futures sharing an
//! [`ImprovWifiBridge`]:
//!
//! * [`serve`] (or your own loop calling [`ImprovWifiBridge::forward`]) answers
//!   all GATT traffic and forwards RPC command writes into the bridge.
//! * [`TroubleGattServer`] implements [`GattServer`] for the driver: it takes
//!   writes from the bridge and sends notifications directly.
//!
//! Per connection:
//!
//! ```ignore
//! #[gatt_server]
//! struct Server {
//!     improv_wifi: ImprovWifiService,
//! }
//!
//! static BRIDGE: ImprovWifiBridge<CriticalSectionRawMutex> = ImprovWifiBridge::new();
//! const CAPS: Capabilities = impromptu_wifi_ble::capabilities::<MyWifi, MyDevice>();
//!
//! let gatt = TroubleGattServer::new(&conn, &server.improv_wifi, &BRIDGE, CAPS)?;
//! let mut driver = Driver::new(BleLink::new(gatt, CAPS), &mut wifi, &mut device, true);
//! select(serve(&conn, &server.improv_wifi, &BRIDGE), driver.provision(&mut button)).await;
//! ```
//!
//! While advertising, include [`ImprovWifiBridge::service_data`] under the
//! 16-bit UUID `0x4677`, alongside the service UUID, as the spec requires.

use core::cell::Cell;

use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::channel::Channel;
use impromptu_wifi_ble::GattServer;
use impromptu_wifi_core::proto::ble::{Capabilities, MAX_RPC_FRAME};
use trouble_host::prelude::*;

/// One RPC command write, as received.
pub type RpcWrite = heapless::Vec<u8, MAX_RPC_FRAME>;

pub use service::ImprovWifiService;

// The macro generates undocumented items, so it gets a module of its own
// where the docs lint is relaxed.
#[allow(missing_docs)]
mod service {
    use trouble_host::prelude::*;

    /// The Improv GATT service. Add it as a field of your `#[gatt_server]`.
    #[gatt_service(uuid = "00467768-6228-2272-4663-277478268000")]
    pub struct ImprovWifiService {
        #[characteristic(uuid = "00467768-6228-2272-4663-277478268001", read, notify)]
        pub state: u8,
        #[characteristic(uuid = "00467768-6228-2272-4663-277478268002", read, notify)]
        pub error: u8,
        #[characteristic(uuid = "00467768-6228-2272-4663-277478268003", write)]
        pub rpc_command: heapless::Vec<u8, 258>,
        #[characteristic(uuid = "00467768-6228-2272-4663-277478268004", read, notify)]
        pub rpc_result: heapless::Vec<u8, 258>,
        #[characteristic(uuid = "00467768-6228-2272-4663-277478268005", read)]
        pub capabilities: u8,
    }
}

/// Connects the GATT event loop to the driver, and holds the current
/// advertisement service data.
///
/// Usually a `static`. `M` is the mutex flavour: `NoopRawMutex` if
/// everything runs on one executor, `CriticalSectionRawMutex` otherwise.
pub struct ImprovWifiBridge<M: RawMutex> {
    writes: Channel<M, RpcWrite, 4>,
    service_data: Mutex<M, Cell<[u8; 6]>>,
}

impl<M: RawMutex> Default for ImprovWifiBridge<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: RawMutex> ImprovWifiBridge<M> {
    /// An empty bridge.
    pub const fn new() -> Self {
        Self {
            writes: Channel::new(),
            service_data: Mutex::new(Cell::new([0; 6])),
        }
    }

    /// If `event` is a write to the RPC command characteristic, queue its
    /// data for the driver and return `true`. The caller still accepts the
    /// event as usual, which sends the write response.
    ///
    /// Call this from your own event loop if you don't use [`serve`].
    pub fn forward<P: PacketPool>(
        &self,
        improv_wifi: &ImprovWifiService,
        event: &GattEvent<'_, '_, P>,
    ) -> bool {
        let GattEvent::Write(write) = event else {
            return false;
        };
        if write.handle() != improv_wifi.rpc_command.handle {
            return false;
        }
        write.with_data(|offset, data| {
            // Improv clients send a frame as whole-chunk writes, so an offset
            // write is dropped; the driver's reassembly then reports the
            // frame as invalid.
            if offset == 0 {
                if let Ok(chunk) = RpcWrite::from_slice(data) {
                    // A full queue means the driver is far behind; dropping
                    // is better than blocking the GATT loop.
                    let _ = self.writes.try_send(chunk);
                }
            }
        });
        true
    }

    /// The service data to advertise under UUID `0x4677`.
    pub fn service_data(&self) -> [u8; 6] {
        self.service_data.lock(Cell::get)
    }

    /// Drop queued writes, for example after a disconnect.
    pub fn clear(&self) {
        self.writes.clear();
    }
}

/// Answer all GATT traffic on `conn`, forwarding Improv RPC writes to
/// `bridge`, until the connection drops.
///
/// Handles only what Improv needs. If your server has other services with
/// their own logic, write your own loop and call [`ImprovWifiBridge::forward`].
pub async fn serve<M: RawMutex, P: PacketPool>(
    conn: &GattConnection<'_, '_, P>,
    improv_wifi: &ImprovWifiService,
    bridge: &ImprovWifiBridge<M>,
) {
    loop {
        match conn.next().await {
            GattConnectionEvent::Disconnected { .. } => {
                bridge.clear();
                return;
            }
            GattConnectionEvent::Gatt { event } => {
                bridge.forward(improv_wifi, &event);
                if let Ok(reply) = event.accept() {
                    reply.send().await;
                }
            }
            _ => {}
        }
    }
}

/// A [`GattServer`] on one `trouble-host` connection.
pub struct TroubleGattServer<'a, 'stack, 'server, M: RawMutex, P: PacketPool> {
    conn: &'a GattConnection<'stack, 'server, P>,
    improv_wifi: &'a ImprovWifiService,
    bridge: &'a ImprovWifiBridge<M>,
}

impl<'a, 'stack, 'server, M: RawMutex, P: PacketPool> TroubleGattServer<'a, 'stack, 'server, M, P> {
    /// Bind to a connection and publish the capabilities characteristic.
    pub fn new(
        conn: &'a GattConnection<'stack, 'server, P>,
        improv_wifi: &'a ImprovWifiService,
        bridge: &'a ImprovWifiBridge<M>,
        caps: Capabilities,
    ) -> Result<Self, Error> {
        conn.set(&improv_wifi.capabilities, &caps.bits())?;
        Ok(Self {
            conn,
            improv_wifi,
            bridge,
        })
    }
}

impl<M: RawMutex, P: PacketPool> GattServer for TroubleGattServer<'_, '_, '_, M, P> {
    type Error = Error;

    async fn rpc_command_written(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        let chunk = self.bridge.writes.receive().await;
        let n = chunk.len().min(buf.len());
        buf[..n].copy_from_slice(&chunk[..n]);
        Ok(n)
    }

    async fn set_state(&mut self, value: u8) -> Result<(), Error> {
        self.improv_wifi.state.notify(self.conn, &value, true).await
    }

    async fn set_error(&mut self, value: u8) -> Result<(), Error> {
        self.improv_wifi.error.notify(self.conn, &value, true).await
    }

    async fn set_rpc_result(&mut self, value: &[u8]) -> Result<(), Error> {
        // Store the whole frame for reads, but notify only what fits one
        // packet: trouble-host fails rather than truncates a value larger
        // than its packet buffer. Clients read the full value on notification.
        let full =
            heapless::Vec::<u8, 258>::from_slice(value).map_err(|_| Error::InsufficientSpace)?;
        self.conn.set(&self.improv_wifi.rpc_result, &full)?;
        let fits = usize::from(self.conn.raw().att_mtu()).saturating_sub(3);
        let head = &value[..value.len().min(fits)];
        self.improv_wifi
            .rpc_result
            .notify_raw(self.conn, head, false)
            .await
    }

    async fn set_service_data(&mut self, data: [u8; 6]) -> Result<(), Error> {
        self.bridge.service_data.lock(|c| c.set(data));
        Ok(())
    }
}
