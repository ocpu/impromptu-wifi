//! A Web Serial port as an `embedded-io-async` stream.

use embedded_io_async::{ErrorType, Read, Write};
use js_sys::futures::JsFuture;
use js_sys::{Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{
    ReadableStreamDefaultReader, SerialOptions, SerialPort, WritableStreamDefaultWriter,
};

use super::time::{sleep, with_timeout};
use crate::chunks::ChunkBuffer;
use crate::WebError;

/// Stop discarding stale input after this long without new bytes.
const DISCARD_QUIET_MS: u32 = 100;
/// Never discard for longer than this.
const DISCARD_MAX_MS: u32 = 1000;

/// An open Web Serial port.
///
/// Reads are cancel-safe: a pending `read()` promise is kept across calls,
/// so bytes that arrive after a caller gave up are delivered to the next
/// read instead of being lost.
pub struct WebSerialPort {
    port: SerialPort,
    reader: ReadableStreamDefaultReader,
    writer: WritableStreamDefaultWriter,
    pending: Option<JsFuture>,
    buf: ChunkBuffer,
}

impl WebSerialPort {
    /// Ask the user to choose a port, and open it at `baud`.
    ///
    /// DTR and RTS are left as the browser and operating system set them, as
    /// `impromptu-wifi-host` does: on ESP32 boards these lines drive the
    /// reset and boot pins, and changing one line at a time can reset the
    /// chip.
    pub async fn request(baud: u32) -> Result<Self, JsValue> {
        let window = web_sys::window().ok_or("no window")?;
        let serial = window.navigator().serial();
        let port: SerialPort = serial.request_port().await?;
        port.open(&SerialOptions::new(baud)).await?;
        let reader = port
            .readable()
            .get_reader()
            .dyn_into::<ReadableStreamDefaultReader>()?;
        let writer = port.writable().get_writer()?;
        Ok(Self {
            port,
            reader,
            writer,
            pending: None,
            buf: ChunkBuffer::default(),
        })
    }

    /// Read and drop whatever the device sent before the port was opened,
    /// as `impromptu-wifi-host` does: until 100 ms pass without input, for
    /// at most 1 s. Returns the number of bytes dropped.
    pub async fn discard_pending(&mut self) -> usize {
        let mut dropped = 0;
        let mut scratch = [0u8; 256];
        let deadline = with_timeout(DISCARD_MAX_MS, async {
            loop {
                match with_timeout(DISCARD_QUIET_MS, self.read(&mut scratch)).await {
                    Some(Ok(n)) if n > 0 => dropped += n,
                    _ => break,
                }
            }
        });
        deadline.await;
        dropped
    }

    /// Release the streams and close the port.
    pub async fn close(self) {
        let _ = self.reader.cancel().await;
        self.reader.release_lock();
        let _ = self.writer.close().await;
        self.writer.release_lock();
        let _ = self.port.close().await;
        // Give the browser a moment before the port can be opened again.
        sleep(50).await;
    }

    /// Wait for the next chunk from the port into `self.buf`.
    async fn fill(&mut self) -> Result<(), WebError> {
        let reader = &self.reader;
        let pending = self
            .pending
            .get_or_insert_with(|| JsFuture::from(reader.read()));
        let result = pending.await;
        self.pending = None;
        let result = result.map_err(|_| WebError::Disconnected)?;
        let done = Reflect::get(&result, &"done".into())
            .ok()
            .and_then(|d| d.as_bool())
            .unwrap_or(false);
        if done {
            return Err(WebError::Disconnected);
        }
        let value = Reflect::get(&result, &"value".into()).map_err(js_error)?;
        let chunk: Uint8Array = value.dyn_into().map_err(js_error)?;
        self.buf.push(&chunk.to_vec());
        Ok(())
    }
}

fn js_error(e: JsValue) -> WebError {
    WebError::Js(format!("{e:?}"))
}

impl ErrorType for WebSerialPort {
    type Error = WebError;
}

impl Read for WebSerialPort {
    async fn read(&mut self, out: &mut [u8]) -> Result<usize, WebError> {
        if out.is_empty() {
            return Ok(0);
        }
        while self.buf.is_empty() {
            self.fill().await?;
        }
        Ok(self.buf.take(out))
    }
}

impl Write for WebSerialPort {
    async fn write(&mut self, data: &[u8]) -> Result<usize, WebError> {
        let chunk = Uint8Array::from(data);
        JsFuture::from(self.writer.write_with_chunk(&chunk))
            .await
            .map_err(|_| WebError::Disconnected)?;
        Ok(data.len())
    }

    async fn flush(&mut self) -> Result<(), WebError> {
        // Each write already waited for the stream to accept its chunk.
        Ok(())
    }
}
