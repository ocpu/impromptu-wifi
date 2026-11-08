//! Zero-length packets on USB CDC-ACM.

use std::{cell::RefCell, rc::Rc};

use embassy_futures::block_on;
use embassy_usb::driver::EndpointError;
use embedded_io_async::{ErrorType, Read, Write};
use impromptu_wifi_core::Link;
use impromptu_wifi_embassy::usb::{PacketSink, ZlpWriter};
use impromptu_wifi_serial::{Duplex, SerialLink};

/// Records every USB packet sent; 64-byte packets, as at full speed.
#[derive(Clone, Default)]
struct Wire(Rc<RefCell<Vec<usize>>>, bool);

impl PacketSink for Wire {
    fn max_packet_size(&self) -> usize {
        64
    }
    async fn write_packet(&mut self, data: &[u8]) -> Result<(), EndpointError> {
        assert!(data.len() <= 64, "oversize packet");
        if self.1 {
            return Err(EndpointError::Disabled);
        }
        self.0.borrow_mut().push(data.len());
        Ok(())
    }
}

impl Wire {
    fn packets(&self) -> Vec<usize> {
        self.0.borrow().clone()
    }
}

fn send(len: usize) -> Vec<usize> {
    let wire = Wire::default();
    let mut w = ZlpWriter::new(wire.clone());
    block_on(w.write_all(&vec![0x55; len])).unwrap();
    block_on(w.flush()).unwrap();
    block_on(w.flush()).unwrap(); // a second flush adds nothing
    wire.packets()
}

#[test]
fn zero_length_packet_only_after_a_full_final_packet() {
    assert_eq!(send(63), [63]);
    assert_eq!(send(64), [64, 0]);
    assert_eq!(send(65), [64, 1]);
    assert_eq!(send(128), [64, 64, 0]);
    assert_eq!(send(0), Vec::<usize>::new());
}

#[test]
fn unplugged_is_not_connected() {
    let mut w = ZlpWriter::new(Wire(Default::default(), true));
    let err = block_on(w.write(b"x")).unwrap_err();
    assert_eq!(
        embedded_io_async::Error::kind(&err),
        embedded_io_async::ErrorKind::NotConnected
    );
}

// --- through a real SerialLink ----------------------------------------------

struct NoInput;
impl ErrorType for NoInput {
    type Error = embassy_usb::class::cdc_acm::CdcAcmError;
}
impl Read for NoInput {
    async fn read(&mut self, _buf: &mut [u8]) -> Result<usize, Self::Error> {
        Ok(0)
    }
}

/// Send one RPC result whose frame is `frame_len` bytes, and return the USB
/// packets that went out. A serial packet is 10 bytes of envelope plus the
/// frame, plus the trailing newline.
fn result_packets(frame_len: usize) -> Vec<usize> {
    let wire = Wire::default();
    let mut io = Duplex::new(NoInput, ZlpWriter::new(wire.clone()));
    let mut link = SerialLink::new(&mut io);
    block_on(link.send_result(&vec![0u8; frame_len])).unwrap();
    wire.packets()
}

#[test]
fn improv_wifi_packet_of_exactly_64_bytes_is_terminated() {
    assert_eq!(result_packets(53), [64, 0], "10 + 53 + newline = 64");
    assert_eq!(result_packets(52), [63]);
    assert_eq!(result_packets(117), [64, 64, 0], "10 + 117 + newline = 128");
}
