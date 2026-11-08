//! Improv Wi-Fi over serial, for both ends of the line.
//!
//! * [`SerialLink`] is the device side: it implements
//!   [`impromptu_wifi_core::Link`] for [`impromptu_wifi_core::Driver`].
//! * [`SerialClientLink`] is the client side: it implements
//!   [`impromptu_wifi_client::ClientLink`] for [`impromptu_wifi_client::Client`].
//!
//! Both work on any stream implementing [`embedded_io_async::Read`] and
//! [`Write`]; join split halves with [`Duplex`]. Which HAL types qualify:
//!
//! * nRF, RP, STM32: the *buffered* UARTs (`BufferedUarte`, `BufferedUart`),
//!   or on STM32 `RingBufferedUartRx` plus `UartTx` in a [`Duplex`]. Their
//!   plain async UARTs implement only `Write`.
//! * ESP32: async `Uart` and `UsbSerialJtag` (esp-hal `unstable` feature).
//! * USB CDC-ACM through `embassy-usb`: use `impromptu-wifi-embassy`'s `usb`
//!   feature, which adds the zero-length packets USB needs.
//! * Hosts: a tokio serial port through `embedded-io-adapters`.
//!
//! A line shared with log output works, with one caveat: a log line written
//! *while* a packet is being sent lands inside it and corrupts it. The
//! receiver detects that by checksum (the device answers "invalid RPC", the
//! client gets a protocol error) and recovers at the next packet, but it
//! costs a retry. Logging from another task or an interrupt can do this;
//! route logs elsewhere (for example defmt over a debug probe) if it matters.
//!
//! ```ignore
//! // Device
//! let mut driver = Driver::new(SerialLink::new(uart), wifi, device, false);
//! driver.run(&mut NoAuth).await?;
//!
//! // Client
//! let mut client = Client::new(SerialClientLink::new(port));
//! let url = client.provision("home", "hunter22").await?;
//! ```
//!
//! The line can be shared with log output: noise is skipped when reading,
//! and the device side ends packets with a newline by default so they don't
//! run into the next log line on a console.
#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod client;
mod device;
mod duplex;

pub use client::SerialClientLink;
pub use device::SerialLink;
pub use duplex::Duplex;

pub use impromptu_wifi_client;
pub use impromptu_wifi_core;

use embedded_io_async::{Read, Write};
use impromptu_wifi_core::proto::serial::{encode_packet, Feed, Framer, Packet, PacketType};
use impromptu_wifi_core::proto::DecodeError;

/// A serial link failure.
#[derive(Debug)]
pub enum SerialError<E> {
    /// The underlying stream failed.
    Io(E),
    /// The stream reached end of file.
    Eof,
    /// An outgoing frame was too large for a packet. Indicates a bug.
    Oversize,
}

/// What [`PacketReader::next`] found.
enum Next {
    /// A valid packet; read it with [`PacketReader::packet`].
    Packet,
    /// A packet arrived with a bad checksum.
    Corrupt,
}

/// Buffered, cancel-safe packet reading shared by both ends.
struct PacketReader {
    framer: Framer,
    chunk: [u8; 64],
    pos: usize,
    end: usize,
}

impl PacketReader {
    const fn new() -> Self {
        Self {
            framer: Framer::new(),
            chunk: [0; 64],
            pos: 0,
            end: 0,
        }
    }

    /// Read until a packet completes. Bytes already read stay in `self`,
    /// so dropping this future between reads loses nothing.
    async fn next<IO: Read>(&mut self, io: &mut IO) -> Result<Next, SerialError<IO::Error>> {
        loop {
            while self.pos < self.end {
                let byte = self.chunk[self.pos];
                self.pos += 1;
                match self.framer.feed(byte) {
                    Feed::Pending => {}
                    Feed::Packet => return Ok(Next::Packet),
                    // A real packet arrived damaged.
                    Feed::Error(DecodeError::Checksum) => return Ok(Next::Corrupt),
                    // Anything else is most likely noise that happened to
                    // start with the header.
                    Feed::Error(_) => {}
                }
            }
            let n = io.read(&mut self.chunk).await.map_err(SerialError::Io)?;
            if n == 0 {
                return Err(SerialError::Eof);
            }
            self.pos = 0;
            self.end = n;
        }
    }

    fn packet(&self) -> Option<Packet<'_>> {
        self.framer.packet()
    }
}

/// Encode and write one packet, optionally followed by `\n`.
async fn write_packet<IO: Write>(
    io: &mut IO,
    tx: &mut [u8],
    ty: PacketType,
    data: &[u8],
    newline: bool,
) -> Result<(), SerialError<IO::Error>> {
    let mut n = encode_packet(tx, ty, data).map_err(|_| SerialError::Oversize)?;
    if newline {
        *tx.get_mut(n).ok_or(SerialError::Oversize)? = b'\n';
        n += 1;
    }
    io.write_all(&tx[..n]).await.map_err(SerialError::Io)?;
    io.flush().await.map_err(SerialError::Io)
}
