use impromptu_wifi_proto::Transport;

/// One message from the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Incoming {
    /// A current-state update (raw byte; unknown values are ignored).
    State(u8),
    /// An error-state update (raw byte; unknown values count as `Unknown`).
    Error(u8),
    /// An RPC result, copied into the receive buffer: this many bytes, in
    /// the form [`RawRpc::parse`](impromptu_wifi_proto::RawRpc::parse) expects for
    /// the transport.
    Result(usize),
    /// A message arrived damaged.
    Malformed,
}

/// Moves Improv frames between a client and a device.
pub trait ClientLink {
    /// Transport failure. Fatal to the current call.
    type Error: core::fmt::Debug;

    /// Which flavour of the protocol this link carries.
    const TRANSPORT: Transport;

    /// Send an RPC command frame, already encoded for [`Self::TRANSPORT`]
    /// by [`Command::encode`](impromptu_wifi_proto::Command::encode).
    async fn send_command(&mut self, frame: &[u8]) -> Result<(), Self::Error>;

    /// Wait for the next message from the device. Result frames are copied
    /// into `buf`. Must be cancel-safe.
    async fn recv(&mut self, buf: &mut [u8]) -> Result<Incoming, Self::Error>;

    /// Read the device's state directly, on transports where it is readable
    /// (the BLE state characteristic). `Ok(None)` means "not readable here;
    /// ask with a command instead", which is the default.
    async fn read_state(&mut self) -> Result<Option<u8>, Self::Error> {
        Ok(None)
    }

    /// Read the capabilities byte, on transports that have one (BLE).
    /// `Ok(None)` means the transport has no capabilities, the default.
    async fn read_capabilities(&mut self) -> Result<Option<u8>, Self::Error> {
        Ok(None)
    }
}
