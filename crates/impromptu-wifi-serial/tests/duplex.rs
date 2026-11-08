//! `Duplex` joins halves, and a `SerialLink` runs over it.

extern crate std;
use std::vec::Vec;

use embassy_futures::block_on;
use embedded_io_async::{ErrorType, Read, Write};
use impromptu_wifi_core::proto::serial::{Feed, Framer, PacketType};
use impromptu_wifi_core::proto::State;
use impromptu_wifi_core::Link;
use impromptu_wifi_serial::{Duplex, SerialLink};

struct Rx(Vec<u8>);
struct Tx(Vec<u8>, usize);

impl ErrorType for Rx {
    type Error = core::convert::Infallible;
}
impl ErrorType for Tx {
    type Error = core::convert::Infallible;
}
impl Read for Rx {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let n = buf.len().min(self.0.len());
        buf[..n].copy_from_slice(&self.0[..n]);
        self.0.drain(..n);
        Ok(n)
    }
}
impl Write for Tx {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }
    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.1 += 1;
        Ok(())
    }
}

#[test]
fn halves_are_joined() {
    let mut io = Duplex::new(Rx(b"abc".to_vec()), Tx(Vec::new(), 0));
    let mut buf = [0u8; 8];
    assert_eq!(block_on(io.read(&mut buf)).unwrap(), 3);
    block_on(io.write_all(b"xyz")).unwrap();
    block_on(io.flush()).unwrap();
    let (_, tx) = io.into_parts();
    assert_eq!((tx.0.as_slice(), tx.1), (&b"xyz"[..], 1));
}

#[test]
fn serial_link_over_a_duplex_flushes_every_packet() {
    let mut io = Duplex::new(Rx(Vec::new()), Tx(Vec::new(), 0));
    {
        // A link per session, borrowing the stream, as a reconnect loop would.
        let mut link = SerialLink::new(&mut io);
        block_on(link.send_state(State::Authorized)).unwrap();
        block_on(link.send_state(State::Provisioning)).unwrap();
    }
    let (_, tx) = io.into_parts();
    assert_eq!(tx.1, 2, "one flush per packet");

    let mut framer: Framer = Framer::new();
    let mut states = Vec::new();
    for &b in &tx.0 {
        if framer.feed(b) == Feed::Packet {
            let p = framer.packet().unwrap();
            if p.ty == PacketType::CurrentState {
                states.push(p.data[0]);
            }
        }
    }
    assert_eq!(states, [State::Authorized as u8, State::Provisioning as u8]);
}
