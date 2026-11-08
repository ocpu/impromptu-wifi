//! Improv over USB CDC-ACM with `embassy-usb`.
//!
//! [`CdcAcmSerial`] turns a [`CdcAcmClass`] into one stream implementing
//! `embedded-io-async` `Read` and `Write`, ready for
//! [`SerialLink`](impromptu_wifi_serial::SerialLink). Over the raw class it adds:
//!
//! * **Zero-length packets.** A USB bulk transfer that ends on a full-size
//!   packet isn't complete until a zero-length packet follows. Hosts that
//!   read in transfers larger than one packet otherwise hold back an Improv
//!   packet whose length is a multiple of the packet size, until the next
//!   write, which may never come. `embassy-usb`'s own `flush` does nothing,
//!   so [`ZlpWriter`] sends the zero-length packet on flush when needed;
//!   `SerialLink` flushes after every packet.
//! * **One object.** The class splits into a sender and a buffered receiver;
//!   `CdcAcmSerial` holds both.
//!
//! Unplugging the cable (or a USB reset) makes reads and writes fail with
//! `NotConnected`, which ends the driver loop. Keep the stream across
//! connections and give each session a fresh link that borrows it:
//!
//! ```ignore
//! static RX_BUF: StaticCell<[u8; 64]> = StaticCell::new();
//! let mut serial = CdcAcmSerial::new(class, RX_BUF.init([0; 64]));
//! loop {
//!     serial.wait_connection().await;
//!     let mut driver = Driver::new(SerialLink::new(&mut serial), &mut wifi, &mut device, false);
//!     let _ = driver.run(&mut NoAuth).await; // returns when the host goes away
//! }
//! ```
//!
//! While no program on the host has the port open, writes wait for it, so
//! the driver's first state update simply goes out when a client connects.

use embassy_usb::class::cdc_acm::{BufferedReceiver, CdcAcmClass, CdcAcmError, Sender};
use embassy_usb::driver::{Driver, EndpointError};
use embedded_io_async::{ErrorType, Read, Write};

/// Something that sends USB packets: implemented for `embassy-usb`'s CDC-ACM
/// [`Sender`], and by test doubles.
pub trait PacketSink {
    /// Largest packet the endpoint sends.
    fn max_packet_size(&self) -> usize;

    /// Send one packet of at most [`max_packet_size`](Self::max_packet_size)
    /// bytes. An empty `data` sends a zero-length packet.
    async fn write_packet(&mut self, data: &[u8]) -> Result<(), EndpointError>;
}

impl<'d, D: Driver<'d>> PacketSink for Sender<'d, D> {
    fn max_packet_size(&self) -> usize {
        usize::from(Sender::max_packet_size(self))
    }

    async fn write_packet(&mut self, data: &[u8]) -> Result<(), EndpointError> {
        Sender::write_packet(self, data).await
    }
}

/// A packet writer whose `flush` ends the transfer properly: if the last
/// packet sent was full-size, it sends a zero-length packet.
pub struct ZlpWriter<S> {
    sink: S,
    ends_full: bool,
}

impl<S: PacketSink> ZlpWriter<S> {
    /// Wrap a packet sink.
    pub fn new(sink: S) -> Self {
        Self {
            sink,
            ends_full: false,
        }
    }

    /// The sink.
    pub fn sink_mut(&mut self) -> &mut S {
        &mut self.sink
    }
}

fn io_error(_: EndpointError) -> CdcAcmError {
    // `BufferOverflow` can't happen: writes never exceed the packet size.
    CdcAcmError::NotConnected
}

impl<S: PacketSink> ErrorType for ZlpWriter<S> {
    type Error = CdcAcmError;
}

impl<S: PacketSink> Write for ZlpWriter<S> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, CdcAcmError> {
        if buf.is_empty() {
            return Ok(0);
        }
        let max = self.sink.max_packet_size().max(1);
        let n = buf.len().min(max);
        self.sink.write_packet(&buf[..n]).await.map_err(io_error)?;
        self.ends_full = n == max;
        Ok(n)
    }

    async fn flush(&mut self) -> Result<(), CdcAcmError> {
        if self.ends_full {
            self.sink.write_packet(&[]).await.map_err(io_error)?;
            self.ends_full = false;
        }
        Ok(())
    }
}

/// A USB CDC-ACM port as one `embedded-io-async` stream.
pub struct CdcAcmSerial<'d, D: Driver<'d>> {
    rx: BufferedReceiver<'d, D>,
    tx: ZlpWriter<Sender<'d, D>>,
}

impl<'d, D: Driver<'d>> CdcAcmSerial<'d, D> {
    /// Wrap a CDC-ACM class. `rx_buf` must hold at least one packet (64
    /// bytes at full speed).
    pub fn new(class: CdcAcmClass<'d, D>, rx_buf: &'d mut [u8]) -> Self {
        let (tx, rx) = class.split();
        Self {
            rx: rx.into_buffered(rx_buf),
            tx: ZlpWriter::new(tx),
        }
    }

    /// Wait until the host has configured the interface (the cable is
    /// plugged in and enumerated).
    pub async fn wait_connection(&mut self) {
        self.rx.wait_connection().await;
        self.tx.ends_full = false;
    }

    /// Whether the host asserts DTR. Most programs do while they hold the
    /// port open, but not all, so the driver doesn't depend on it.
    pub fn dtr(&self) -> bool {
        self.rx.dtr()
    }
}

impl<'d, D: Driver<'d>> ErrorType for CdcAcmSerial<'d, D> {
    type Error = CdcAcmError;
}

impl<'d, D: Driver<'d>> Read for CdcAcmSerial<'d, D> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, CdcAcmError> {
        self.rx.read(buf).await
    }
}

impl<'d, D: Driver<'d>> Write for CdcAcmSerial<'d, D> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, CdcAcmError> {
        self.tx.write(buf).await
    }

    async fn flush(&mut self) -> Result<(), CdcAcmError> {
        self.tx.flush().await
    }
}
