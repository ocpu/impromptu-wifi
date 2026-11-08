use embedded_io_async::{ErrorType, Read, Write};

/// Joins a read half and a write half into one stream for [`SerialLink`]
/// or [`SerialClientLink`].
///
/// Many HALs hand out split halves, or only offer `Read` on a different
/// type than `Write`. On STM32, for example, the DMA-friendly pairing is a
/// `RingBufferedUartRx` with a `UartTx`; USB CDC-ACM splits into a sender
/// and a receiver too. Both halves must use the same error type.
///
/// [`SerialLink`]: crate::SerialLink
/// [`SerialClientLink`]: crate::SerialClientLink
pub struct Duplex<R, W> {
    /// The read half.
    pub rx: R,
    /// The write half.
    pub tx: W,
}

impl<R, W> Duplex<R, W> {
    /// Join two halves.
    pub fn new(rx: R, tx: W) -> Self {
        Self { rx, tx }
    }

    /// Split back into halves.
    pub fn into_parts(self) -> (R, W) {
        (self.rx, self.tx)
    }
}

impl<R: ErrorType, W: ErrorType<Error = R::Error>> ErrorType for Duplex<R, W> {
    type Error = R::Error;
}

impl<R: Read, W: ErrorType<Error = R::Error>> Read for Duplex<R, W> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.rx.read(buf).await
    }
}

impl<R: ErrorType, W: Write<Error = R::Error>> Write for Duplex<R, W> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.tx.write(buf).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.tx.flush().await
    }
}
