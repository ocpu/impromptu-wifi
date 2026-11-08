use embedded_io_async::{Read, Write};
use impromptu_wifi_core::proto::serial::{PacketType, MAX_PACKET_LEN};
use impromptu_wifi_core::proto::{ErrorCode, State, Transport};
use impromptu_wifi_core::Link;

use crate::{write_packet, Next, PacketReader, SerialError};

/// The device end of a serial line.
pub struct SerialLink<IO> {
    io: IO,
    reader: PacketReader,
    tx: [u8; MAX_PACKET_LEN + 1],
    newline: bool,
}

impl<IO: Read + Write> SerialLink<IO> {
    /// Wrap a stream.
    pub fn new(io: IO) -> Self {
        Self {
            io,
            reader: PacketReader::new(),
            tx: [0; MAX_PACKET_LEN + 1],
            newline: true,
        }
    }

    /// Whether to send `\n` after each packet (default `true`).
    pub fn trailing_newline(mut self, enabled: bool) -> Self {
        self.newline = enabled;
        self
    }

    /// Give back the stream.
    pub fn into_inner(self) -> IO {
        self.io
    }
}

impl<IO: Read + Write> Link for SerialLink<IO> {
    type Error = SerialError<IO::Error>;
    const TRANSPORT: Transport = Transport::Serial;

    async fn recv(&mut self, buf: &mut [u8]) -> Result<Option<usize>, Self::Error> {
        loop {
            match self.reader.next(&mut self.io).await? {
                Next::Corrupt => return Ok(None),
                Next::Packet => {
                    let Some(packet) = self.reader.packet() else {
                        continue;
                    };
                    // Ignore device-to-client types (for example our own echo).
                    if packet.ty != PacketType::Rpc {
                        continue;
                    }
                    let Some(dst) = buf.get_mut(..packet.data.len()) else {
                        return Ok(None);
                    };
                    dst.copy_from_slice(packet.data);
                    return Ok(Some(packet.data.len()));
                }
            }
        }
    }

    async fn send_state(&mut self, state: State) -> Result<(), Self::Error> {
        write_packet(
            &mut self.io,
            &mut self.tx,
            PacketType::CurrentState,
            &[state as u8],
            self.newline,
        )
        .await
    }

    async fn send_error(&mut self, error: ErrorCode) -> Result<(), Self::Error> {
        write_packet(
            &mut self.io,
            &mut self.tx,
            PacketType::ErrorState,
            &[error as u8],
            self.newline,
        )
        .await
    }

    async fn send_result(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        write_packet(
            &mut self.io,
            &mut self.tx,
            PacketType::RpcResult,
            frame,
            self.newline,
        )
        .await
    }
}
