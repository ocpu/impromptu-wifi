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
//!
//! # Stale output
//!
//! A device on USB-Serial-JTAG (ESP32-C3, -C6, -S3 and others) keeps what it
//! sent while no program had the port open, because the host only collects
//! data from an open port. The next program to open it reads that output
//! first: a state from minutes ago, for example. Call [`discard_pending`]
//! after [`open`] to drop it.

use std::time::Duration;

use embedded_io_adapters::tokio_1::FromTokio;
use tokio::io::{AsyncRead, AsyncReadExt};
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

/// How long the port must stay silent before [`discard_pending`] returns.
pub const DISCARD_QUIET: Duration = Duration::from_millis(100);

/// The longest [`discard_pending`] reads, for a device that never stops
/// sending.
pub const DISCARD_MAX: Duration = Duration::from_secs(1);

/// Read and drop whatever the device sent before the port was opened, until
/// the port has been silent for [`DISCARD_QUIET`] or [`DISCARD_MAX`] has
/// passed. Returns the number of bytes dropped.
///
/// Answers to requests sent afterwards are then fresh. See [Stale
/// output](self#stale-output).
pub async fn discard_pending(port: &mut Port) -> std::io::Result<usize> {
    discard(port.inner_mut(), DISCARD_QUIET, DISCARD_MAX).await
}

async fn discard<R: AsyncRead + Unpin>(
    reader: &mut R,
    quiet: Duration,
    max: Duration,
) -> std::io::Result<usize> {
    let deadline = tokio::time::Instant::now() + max;
    let mut buf = [0u8; 256];
    let mut dropped = 0;
    loop {
        let wait = quiet.min(deadline.saturating_duration_since(tokio::time::Instant::now()));
        match tokio::time::timeout(wait, reader.read(&mut buf)).await {
            // Silent for `quiet`, or out of time.
            Err(_) => return Ok(dropped),
            Ok(Ok(0)) => return Ok(dropped),
            Ok(Ok(n)) => dropped += n,
            Ok(Err(e)) => return Err(e),
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(dropped);
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn discard_drops_what_is_pending_and_stops_when_quiet() {
        let (mut device, mut host) = tokio::io::duplex(1024);
        device.write_all(b"stale state frame").await.unwrap();
        let dropped = discard(&mut host, Duration::from_millis(30), Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(dropped, 17);

        // Later output is left for the client.
        device.write_all(b"fresh").await.unwrap();
        let mut buf = [0u8; 5];
        host.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"fresh");
    }

    #[tokio::test]
    async fn discard_gives_up_on_a_device_that_never_stops() {
        let (mut device, mut host) = tokio::io::duplex(64);
        let chatter = tokio::spawn(async move {
            while device.write_all(b"x").await.is_ok() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
        let start = tokio::time::Instant::now();
        discard(
            &mut host,
            Duration::from_millis(50),
            Duration::from_millis(200),
        )
        .await
        .unwrap();
        let took = start.elapsed();
        assert!(took < Duration::from_millis(400), "took {took:?}");
        chatter.abort();
    }
}
