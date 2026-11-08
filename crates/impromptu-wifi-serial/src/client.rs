use embedded_io_async::{Read, Write};
use impromptu_wifi_client::{ClientLink, Incoming};
use impromptu_wifi_core::proto::serial::{PacketType, MAX_PACKET_LEN};
use impromptu_wifi_core::proto::Transport;

use crate::{write_packet, Next, PacketReader, SerialError};

/// The client end of a serial line.
///
/// Devices often print logs on the same line; the reader skips them. The
/// client doesn't append newlines, since nothing reads its output as text.
pub struct SerialClientLink<IO> {
    io: IO,
    reader: PacketReader,
    tx: [u8; MAX_PACKET_LEN],
}

impl<IO: Read + Write> SerialClientLink<IO> {
    /// Wrap a stream.
    pub fn new(io: IO) -> Self {
        Self {
            io,
            reader: PacketReader::new(),
            tx: [0; MAX_PACKET_LEN],
        }
    }

    /// Give back the stream.
    pub fn into_inner(self) -> IO {
        self.io
    }
}

impl<IO: Read + Write> ClientLink for SerialClientLink<IO> {
    type Error = SerialError<IO::Error>;
    const TRANSPORT: Transport = Transport::Serial;

    async fn send_command(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        write_packet(&mut self.io, &mut self.tx, PacketType::Rpc, frame, false).await
    }

    async fn recv(&mut self, buf: &mut [u8]) -> Result<Incoming, Self::Error> {
        loop {
            match self.reader.next(&mut self.io).await? {
                Next::Corrupt => return Ok(Incoming::Malformed),
                Next::Packet => {
                    let Some(packet) = self.reader.packet() else {
                        continue;
                    };
                    let first = packet.data.first().copied();
                    return Ok(match (packet.ty, first) {
                        (PacketType::CurrentState, Some(v)) => Incoming::State(v),
                        (PacketType::ErrorState, Some(v)) => Incoming::Error(v),
                        (PacketType::RpcResult, _) => {
                            let Some(dst) = buf.get_mut(..packet.data.len()) else {
                                return Ok(Incoming::Malformed);
                            };
                            dst.copy_from_slice(packet.data);
                            Incoming::Result(packet.data.len())
                        }
                        // Commands from another client on a shared line: not for us.
                        (PacketType::Rpc, _) => continue,
                        _ => Incoming::Malformed,
                    });
                }
            }
        }
    }
}
