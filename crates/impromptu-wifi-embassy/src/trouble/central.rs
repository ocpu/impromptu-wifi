//! The client side: a [`GattClient`](impromptu_wifi_ble::GattClient) on
//! `trouble-host`, for firmware that provisions other devices.
//!
//! ```ignore
//! let client = GattClient::<_, _, 4>::new(&stack, &conn).await?;
//! let gatt = TroubleGattClient::new(&client, &conn).await?;
//! let mut improv_wifi = impromptu_wifi_client::Client::new(BleClientLink::new(gatt));
//! // trouble-host only delivers notifications while its client task runs.
//! select(client.task(), improv_wifi.provision("home", "hunter22")).await;
//! ```
//!
//! # Required configuration
//!
//! An Improv device sends several notifications in a burst (error cleared,
//! provisioning, provisioned, result), and `trouble-host`'s client keeps
//! only one queued by default, silently dropping the rest. Enable a larger
//! queue in your application, for example the `trouble-host` feature
//! `gatt-client-notification-queue-size-8`. Using [`TroubleGattClient`]
//! with a queue smaller than [`MIN_NOTIFICATION_QUEUE`] fails the build with
//! a message saying so. (It shows up in `cargo build`, not `cargo check`,
//! which doesn't evaluate the assertion.)
//!
//! [`TroubleGattClient`] holds `trouble-host`'s catch-all notification
//! listener. With the default of one notification subscriber, nothing else
//! on that client can listen at the same time.

use impromptu_wifi_ble::{GattClient as ImprovWifiGattClient, Notification};
use trouble_host::config::GATT_CLIENT_NOTIFICATION_QUEUE_SIZE;

/// The notification buffer size trouble-host 0.7 uses for every listener.
const NOTIFICATION_MTU: usize = 512;
use trouble_host::prelude::*;

/// Smallest notification queue that holds a full Improv burst.
pub const MIN_NOTIFICATION_QUEUE: usize = 4;

const SERVICE: Uuid = uuid!("00467768-6228-2272-4663-277478268000");
const STATE: Uuid = uuid!("00467768-6228-2272-4663-277478268001");
const ERROR: Uuid = uuid!("00467768-6228-2272-4663-277478268002");
const RPC_COMMAND: Uuid = uuid!("00467768-6228-2272-4663-277478268003");
const RPC_RESULT: Uuid = uuid!("00467768-6228-2272-4663-277478268004");
const CAPABILITIES: Uuid = uuid!("00467768-6228-2272-4663-277478268005");

/// Why the client side failed.
#[derive(Debug)]
pub enum CentralError<E> {
    /// The BLE host or controller failed.
    Ble(BleHostError<E>),
    /// The peer has no Improv service.
    NoImprovWifiService,
}

impl<E> From<BleHostError<E>> for CentralError<E> {
    fn from(e: BleHostError<E>) -> Self {
        CentralError::Ble(e)
    }
}

/// An Improv [`GattClient`](impromptu_wifi_ble::GattClient) on one `trouble-host`
/// connection.
pub struct TroubleGattClient<'a, 'r, C: Controller, P: PacketPool, const MAX_SERVICES: usize> {
    client: &'a GattClient<'r, C, P, MAX_SERVICES>,
    conn: &'a Connection<'r, P>,
    state: Characteristic<[u8]>,
    error: Characteristic<[u8]>,
    rpc_command: Characteristic<[u8]>,
    rpc_result: Characteristic<[u8]>,
    capabilities: Characteristic<[u8]>,
    listener: NotificationListener<'a, NOTIFICATION_MTU>,
}

impl<'a, 'r, C: Controller, P: PacketPool, const MAX_SERVICES: usize>
    TroubleGattClient<'a, 'r, C, P, MAX_SERVICES>
{
    const QUEUE_LARGE_ENOUGH: () = assert!(
        GATT_CLIENT_NOTIFICATION_QUEUE_SIZE >= MIN_NOTIFICATION_QUEUE,
        "impromptu-wifi-embassy: trouble-host's notification queue is too small for Improv; \
         enable a trouble-host feature such as `gatt-client-notification-queue-size-8`"
    );

    /// Discover the Improv service on `conn` and subscribe to its
    /// notifications. Run `client.task()` concurrently from here on.
    pub async fn new(
        client: &'a GattClient<'r, C, P, MAX_SERVICES>,
        conn: &'a Connection<'r, P>,
    ) -> Result<Self, CentralError<C::Error>> {
        #[allow(clippy::let_unit_value)]
        let () = Self::QUEUE_LARGE_ENOUGH;

        let services = client.services_by_uuid(&SERVICE).await?;
        let service = services.first().ok_or(CentralError::NoImprovWifiService)?;
        let state = client
            .characteristic_by_uuid::<[u8]>(service, &STATE)
            .await?;
        let error = client
            .characteristic_by_uuid::<[u8]>(service, &ERROR)
            .await?;
        let rpc_command = client
            .characteristic_by_uuid::<[u8]>(service, &RPC_COMMAND)
            .await?;
        let rpc_result = client
            .characteristic_by_uuid::<[u8]>(service, &RPC_RESULT)
            .await?;
        let capabilities = client
            .characteristic_by_uuid::<[u8]>(service, &CAPABILITIES)
            .await?;

        // `subscribe` writes each CCCD; its per-characteristic listener is
        // dropped at once so the single catch-all listener can take the slot.
        for c in [&state, &error, &rpc_result] {
            drop(client.subscribe(c, false).await?);
        }
        let listener = client.listen_all()?;

        Ok(Self {
            client,
            conn,
            state,
            error,
            rpc_command,
            rpc_result,
            capabilities,
            listener,
        })
    }

    async fn read_byte(&self, c: &Characteristic<[u8]>) -> Result<u8, CentralError<C::Error>> {
        let mut value = [0u8; 1];
        let n = self.client.read_characteristic(c, &mut value).await?;
        Ok(if n == 0 { 0 } else { value[0] })
    }
}

impl<C, P, const MAX_SERVICES: usize> ImprovWifiGattClient
    for TroubleGattClient<'_, '_, C, P, MAX_SERVICES>
where
    C: Controller,
    C::Error: core::fmt::Debug,
    P: PacketPool,
{
    type Error = CentralError<C::Error>;

    fn max_write_len(&self) -> usize {
        usize::from(self.conn.att_mtu()).saturating_sub(3).max(1)
    }

    async fn write_rpc_command(&mut self, chunk: &[u8]) -> Result<(), Self::Error> {
        // Write *with* response, as the trait requires.
        Ok(self
            .client
            .write_characteristic(&self.rpc_command, chunk)
            .await?)
    }

    async fn notification(&mut self, buf: &mut [u8]) -> Result<Notification, Self::Error> {
        loop {
            let n = self.listener.next().await;
            let first = n.as_ref().first().copied().unwrap_or(0);
            let handle = n.handle();
            if handle == self.state.handle {
                return Ok(Notification::State(first));
            }
            if handle == self.error.handle {
                return Ok(Notification::Error(first));
            }
            if handle == self.rpc_result.handle {
                // The notification may be truncated to the MTU (and devices
                // using impromptu-wifi-embassy deliberately notify only a prefix), so
                // always read the full value.
                let len = self
                    .client
                    .read_handle_blob(self.rpc_result.handle, 0, buf)
                    .await?;
                return Ok(Notification::RpcResult(len));
            }
            // Some other characteristic on the same peer: not ours.
        }
    }

    async fn read_state(&mut self) -> Result<u8, Self::Error> {
        self.read_byte(&self.state).await
    }

    async fn read_capabilities(&mut self) -> Result<u8, Self::Error> {
        self.read_byte(&self.capabilities).await
    }
}
