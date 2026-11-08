//! Serial ports.
//!
//! # DTR, RTS and accidental resets
//!
//! Many ESP32 boards wire DTR and RTS to the chip's reset (EN) and boot
//! (IO0) pins through two transistors, and ESP32 chips with built-in USB
//! emulate the same circuit. Asserting RTS *alone* holds the chip in reset.
//!
//! [`open`] leaves both lines as the operating system sets them. Linux and
//! macOS assert both together on open, which this circuit ignores, so opening
//! the port doesn't reset the device. Changing one line at a time is what
//! causes surprise resets, so this module never does that except in
//! [`hard_reset`], deliberately.

use std::time::Duration;

use embedded_io_adapters::tokio_1::FromTokio;
use tokio_serial::{SerialPort, SerialPortBuilderExt, SerialStream};

pub use tokio_serial::{available_ports, SerialPortInfo, SerialPortType};

/// The baud rate Improv devices use by convention.
pub const DEFAULT_BAUD: u32 = 115_200;

/// An open serial port, ready for
/// [`SerialClientLink`](impromptu_wifi_serial::SerialClientLink).
pub type Port = FromTokio<SerialStream>;

/// Open `path` at `baud` without touching DTR or RTS.
pub fn open(path: &str, baud: u32) -> tokio_serial::Result<Port> {
    let stream = tokio_serial::new(path, baud)
        .preserve_dtr_on_open()
        .open_native_async()?;
    Ok(FromTokio::new(stream))
}

/// Reset the device into normal boot, the way esptool's hard reset does:
/// RTS asserted with DTR released holds the chip in reset with the boot pin
/// high; releasing RTS lets it boot. The device then prints its boot log,
/// which the client's reader skips.
pub async fn hard_reset(port: &mut Port) -> tokio_serial::Result<()> {
    let stream = port.inner_mut();
    stream.write_data_terminal_ready(false)?;
    stream.write_request_to_send(true)?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    stream.write_request_to_send(false)?;
    Ok(())
}

/// A one-line description of a port for listings.
pub fn describe(port: &SerialPortInfo) -> String {
    match &port.port_type {
        SerialPortType::UsbPort(usb) => {
            let product = usb.product.as_deref().unwrap_or("USB serial");
            let maker = usb
                .manufacturer
                .as_deref()
                .map(|m| format!(", {m}"))
                .unwrap_or_default();
            format!(
                "{} ({product}{maker}, {:04x}:{:04x})",
                port.port_name, usb.vid, usb.pid
            )
        }
        SerialPortType::BluetoothPort => format!("{} (Bluetooth)", port.port_name),
        SerialPortType::PciPort => format!("{} (PCI)", port.port_name),
        SerialPortType::Unknown => port.port_name.clone(),
    }
}
