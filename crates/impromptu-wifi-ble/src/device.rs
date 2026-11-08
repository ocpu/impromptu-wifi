use impromptu_wifi_core::proto::ble::{service_data, Assembly, Capabilities, RpcAssembler};
use impromptu_wifi_core::proto::{ErrorCode, State, Transport};
use impromptu_wifi_core::{HostnameProvider, Identify, Link, WifiScanner};

/// The parts of a GATT server that Improv needs.
///
/// The implementation registers the Improv service with its five
/// characteristics (UUIDs in [`impromptu_wifi_core::proto::ble`]), sets the
/// capabilities characteristic once, and advertises the service UUID
/// together with the service data passed to
/// [`set_service_data`](Self::set_service_data).
pub trait GattServer {
    /// Stack failure. Fatal to the driver loop.
    type Error: core::fmt::Debug;

    /// Wait for the next write to the RPC command characteristic and copy
    /// it into `buf`. Must be cancel-safe.
    async fn rpc_command_written(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error>;

    /// Set the current-state characteristic and notify subscribers.
    async fn set_state(&mut self, value: u8) -> Result<(), Self::Error>;

    /// Set the error-state characteristic and notify subscribers.
    async fn set_error(&mut self, value: u8) -> Result<(), Self::Error>;

    /// Set the RPC result characteristic and notify subscribers.
    async fn set_rpc_result(&mut self, value: &[u8]) -> Result<(), Self::Error>;

    /// Update the advertised service data (under UUID `0x4677`).
    async fn set_service_data(&mut self, data: [u8; 6]) -> Result<(), Self::Error>;
}

/// The capabilities to advertise, derived from which optional traits the
/// Wi-Fi object `W` and device object `D` support. Evaluated at compile time:
///
/// ```ignore
/// const CAPS: Capabilities = impromptu_wifi_ble::capabilities::<MyWifi, MyDevice>();
/// ```
pub const fn capabilities<W: WifiScanner, D: Identify + HostnameProvider>() -> Capabilities {
    let mut caps = Capabilities::DEVICE_INFO;
    if <D as Identify>::SUPPORTED {
        caps = caps.union(Capabilities::IDENTIFY);
    }
    if <W as WifiScanner>::SUPPORTED {
        caps = caps.union(Capabilities::SCAN);
    }
    if <D as HostnameProvider>::SUPPORTED {
        caps = caps.union(Capabilities::HOSTNAME);
    }
    caps
}

/// An Improv link over a GATT server.
pub struct BleLink<G> {
    gatt: G,
    caps: Capabilities,
    assembler: RpcAssembler,
    chunk: [u8; 256],
}

impl<G: GattServer> BleLink<G> {
    /// Wrap a GATT server.
    pub fn new(gatt: G, caps: Capabilities) -> Self {
        Self {
            gatt,
            caps,
            assembler: RpcAssembler::new(),
            chunk: [0; 256],
        }
    }

    /// The advertised capabilities.
    pub fn capabilities(&self) -> Capabilities {
        self.caps
    }

    /// The GATT server.
    pub fn gatt_mut(&mut self) -> &mut G {
        &mut self.gatt
    }

    /// Drop any half-written command. Call on disconnect.
    pub fn reset(&mut self) {
        self.assembler.reset();
    }

    /// Give back the GATT server.
    pub fn into_inner(self) -> G {
        self.gatt
    }
}

impl<G: GattServer> Link for BleLink<G> {
    type Error = G::Error;
    const TRANSPORT: Transport = Transport::Ble;

    async fn recv(&mut self, buf: &mut [u8]) -> Result<Option<usize>, Self::Error> {
        loop {
            let n = self.gatt.rpc_command_written(&mut self.chunk).await?;
            match self.assembler.push(&self.chunk[..n]) {
                Assembly::Pending => {}
                Assembly::Error(_) => return Ok(None),
                Assembly::Complete => {
                    let Some(frame) = self.assembler.frame() else {
                        return Ok(None);
                    };
                    let Some(dst) = buf.get_mut(..frame.len()) else {
                        return Ok(None);
                    };
                    dst.copy_from_slice(frame);
                    return Ok(Some(frame.len()));
                }
            }
        }
    }

    async fn send_state(&mut self, state: State) -> Result<(), Self::Error> {
        self.gatt.set_state(state as u8).await?;
        self.gatt
            .set_service_data(service_data(state, self.caps))
            .await
    }

    async fn send_error(&mut self, error: ErrorCode) -> Result<(), Self::Error> {
        self.gatt.set_error(error as u8).await
    }

    async fn send_result(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        self.gatt.set_rpc_result(frame).await
    }
}
