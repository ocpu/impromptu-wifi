//! Serial ports for the device side: a UART, or USB gadget mode (on a
//! Raspberry Pi Zero, the `g_serial` or ConfigFS ACM function appears as
//! `/dev/ttyGS0`).

use embedded_io_adapters::tokio_1::FromTokio;
use tokio_serial::{SerialPortBuilderExt, SerialStream};

/// An open port, ready for [`SerialLink`](impromptu_wifi_serial::SerialLink).
pub type Port = FromTokio<SerialStream>;

/// Open `path` at `baud`.
pub fn open(path: &str, baud: u32) -> tokio_serial::Result<Port> {
    Ok(FromTokio::new(
        tokio_serial::new(path, baud).open_native_async()?,
    ))
}
