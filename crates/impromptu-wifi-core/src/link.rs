//! The transport abstraction.

use impromptu_wifi_proto::{ErrorCode, State, Transport};

/// Moves Improv frames between the device and a client.
///
/// A link only frames bytes; it never interprets commands. The driver does
/// the parsing, so a link can be tested with nothing but byte buffers.
pub trait Link {
    /// Transport failure (I/O error, disconnect…). Fatal to the driver loop.
    type Error: core::fmt::Debug;

    /// Which flavour of the protocol this link carries.
    const TRANSPORT: Transport;

    /// Wait for the next RPC command and copy its frame into `buf`.
    ///
    /// The frame is as [`RawRpc::parse`](impromptu_wifi_proto::RawRpc::parse)
    /// expects for [`Self::TRANSPORT`]: on BLE including its checksum, on
    /// serial the data section of an RPC packet. Return `Ok(None)` if a
    /// frame arrived but was malformed, so the driver can report
    /// `InvalidRpc`.
    ///
    /// **Must be cancel-safe**: the driver races this against authorization
    /// events and drops it when they win. Keep partial-frame state in
    /// `self`, not in locals.
    async fn recv(&mut self, buf: &mut [u8]) -> Result<Option<usize>, Self::Error>;

    /// Publish the current state.
    async fn send_state(&mut self, state: State) -> Result<(), Self::Error>;

    /// Publish the current error.
    async fn send_error(&mut self, error: ErrorCode) -> Result<(), Self::Error>;

    /// Publish an RPC result frame, already encoded for [`Self::TRANSPORT`]
    /// by [`ResultWriter`](impromptu_wifi_proto::ResultWriter).
    async fn send_result(&mut self, frame: &[u8]) -> Result<(), Self::Error>;
}
