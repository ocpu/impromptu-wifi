use impromptu_wifi_client::{ClientLink, Incoming};
use impromptu_wifi_core::proto::ble::write_chunks;
use impromptu_wifi_core::proto::Transport;

/// A notification from one of the Improv characteristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notification {
    /// The current-state characteristic changed to this value.
    State(u8),
    /// The error-state characteristic changed to this value.
    Error(u8),
    /// The RPC result characteristic changed; its value (this many bytes)
    /// was copied into the buffer.
    RpcResult(usize),
}

/// The parts of a GATT client that Improv needs, for an already connected
/// device whose Improv service has been discovered.
///
/// The implementation subscribes to notifications on the current-state,
/// error-state and RPC result characteristics (UUIDs in
/// [`impromptu_wifi_core::proto::ble`]) before the first call.
pub trait GattClient {
    /// Stack failure or disconnect. Fatal to the current call.
    type Error: core::fmt::Debug;

    /// The largest value one write may carry: the negotiated ATT MTU minus
    /// 3. Defaults to the 20 bytes every connection supports.
    fn max_write_len(&self) -> usize {
        impromptu_wifi_core::proto::ble::DEFAULT_WRITE_LEN
    }

    /// Write one chunk to the RPC command characteristic. Use a write *with*
    /// response, so chunks can't be reordered or dropped.
    async fn write_rpc_command(&mut self, chunk: &[u8]) -> Result<(), Self::Error>;

    /// Wait for the next notification. For the RPC result, deliver the
    /// characteristic's full value: if the notification was truncated to
    /// the MTU, read the characteristic (a long read) instead. Must be
    /// cancel-safe.
    async fn notification(&mut self, buf: &mut [u8]) -> Result<Notification, Self::Error>;

    /// Read the current-state characteristic.
    async fn read_state(&mut self) -> Result<u8, Self::Error>;

    /// Read the capabilities characteristic.
    async fn read_capabilities(&mut self) -> Result<u8, Self::Error>;
}

/// The client end of a BLE connection.
pub struct BleClientLink<G> {
    gatt: G,
}

impl<G: GattClient> BleClientLink<G> {
    /// Wrap a connected GATT client.
    pub fn new(gatt: G) -> Self {
        Self { gatt }
    }

    /// The GATT client.
    pub fn gatt_mut(&mut self) -> &mut G {
        &mut self.gatt
    }

    /// Give back the GATT client.
    pub fn into_inner(self) -> G {
        self.gatt
    }
}

impl<G: GattClient> ClientLink for BleClientLink<G> {
    type Error = G::Error;
    const TRANSPORT: Transport = Transport::Ble;

    async fn send_command(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        let max = self.gatt.max_write_len();
        for chunk in write_chunks(frame, max) {
            self.gatt.write_rpc_command(chunk).await?;
        }
        Ok(())
    }

    async fn recv(&mut self, buf: &mut [u8]) -> Result<Incoming, Self::Error> {
        Ok(match self.gatt.notification(buf).await? {
            Notification::State(v) => Incoming::State(v),
            Notification::Error(v) => Incoming::Error(v),
            Notification::RpcResult(n) if n <= buf.len() => Incoming::Result(n),
            Notification::RpcResult(_) => Incoming::Malformed,
        })
    }

    async fn read_state(&mut self) -> Result<Option<u8>, Self::Error> {
        self.gatt.read_state().await.map(Some)
    }

    async fn read_capabilities(&mut self) -> Result<Option<u8>, Self::Error> {
        self.gatt.read_capabilities().await.map(Some)
    }
}
