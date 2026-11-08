//! The page behind the redirect URL: `http://<address>/` answers with a
//! short "it works" page.
//!
//! Improv Wi-Fi clients (ESP Web Tools, Home Assistant, the CLI) send the
//! user to the redirect URL after provisioning, so something should answer
//! there. HTTP/1.1 with `Connection: close`, the same page for every path
//! and method.
//!
//! smoltcp has no listen backlog: a connection that arrives while no socket
//! is listening is refused. [`SOCKETS`] tasks each listen on port 80, so a
//! browser's parallel connections and a request that follows another
//! closely are served.

use alloc::format;
use alloc::string::String;
use embassy_net::tcp::TcpSocket;
use embassy_net::Stack;
use embassy_time::{with_timeout, Duration, Instant};
use embedded_io_async::Write;
use impromptu_wifi::device::DeviceInfoSource;
use log::warn;

/// What the page shows about the device.
pub struct About {
    pub name: &'static str,
    pub firmware: &'static str,
    pub version: &'static str,
    pub chip: &'static str,
}

impl About {
    /// Copy the details from the device object.
    pub fn of<D: DeviceInfoSource>(device: &'static D) -> Self {
        Self {
            name: device.device_name(),
            firmware: device.firmware_name(),
            version: device.firmware_version(),
            chip: device.chip(),
        }
    }
}

/// Sockets listening on port 80, one task each.
pub const SOCKETS: usize = 3;

/// How long to wait for the client to close the connection.
const CLOSE_WAIT: Duration = Duration::from_millis(500);

/// Serve the page on port 80 from one socket, forever. Run [`SOCKETS`] of
/// these.
pub async fn serve(stack: Stack<'static>, about: About) -> ! {
    let mut rx = [0u8; 1024];
    let mut tx = [0u8; 2048];
    loop {
        let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
        socket.set_timeout(Some(Duration::from_secs(10)));
        if let Err(e) = socket.accept(80).await {
            warn!("web: accept: {e:?}");
            continue;
        }
        if let Err(e) = answer(&mut socket, stack, &about).await {
            warn!("web: {e:?}");
        }
        // Send our FIN, then give the client a moment to close its side,
        // which it does after reading the whole response. Without the
        // wait, dropping the socket would reset the connection; waiting
        // for the full close would keep the port unanswered meanwhile.
        socket.close();
        let mut rest = [0u8; 64];
        let _ = with_timeout(CLOSE_WAIT, async {
            while let Ok(n) = socket.read(&mut rest).await {
                if n == 0 {
                    break;
                }
            }
        })
        .await;
        socket.abort();
    }
}

async fn answer(
    socket: &mut TcpSocket<'_>,
    stack: Stack<'static>,
    about: &About,
) -> Result<(), embassy_net::tcp::Error> {
    // Read the request head and ignore it: every request gets the page.
    let mut head = [0u8; 512];
    let mut len = 0;
    while len < head.len() {
        let n = socket.read(&mut head[len..]).await?;
        if n == 0 {
            break;
        }
        len += n;
        if head[..len].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }

    let body = page(stack, about);
    let header = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(header.as_bytes()).await?;
    socket.write_all(body.as_bytes()).await?;
    socket.flush().await
}

fn page(stack: Stack<'static>, about: &About) -> String {
    let address = stack
        .config_v4()
        .map(|c| format!("{}", c.address.address()))
        .unwrap_or_else(|| "-".into());
    let up = Instant::now().as_secs();
    let uptime = format!("{}h {:02}m {:02}s", up / 3600, up / 60 % 60, up % 60);
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{name}</title>
<style>
:root {{ color-scheme: light dark; --fg: #1d2125; --bg: #f6f7f9; --card: #fff; --muted: #5d6670; --ok: #1f8a4c; }}
@media (prefers-color-scheme: dark) {{ :root {{ --fg: #e8eaed; --bg: #15181b; --card: #1f2326; --muted: #9aa3ab; --ok: #4cc27f; }} }}
body {{ margin: 0; min-height: 100vh; display: grid; place-items: center; background: var(--bg); color: var(--fg);
       font: 16px/1.5 system-ui, -apple-system, "Segoe UI", sans-serif; }}
main {{ background: var(--card); border-radius: 12px; padding: 28px 32px; margin: 16px; max-width: 420px; width: 100%;
       box-shadow: 0 1px 3px rgba(0,0,0,.12); box-sizing: border-box; }}
h1 {{ margin: 0 0 4px; font-size: 1.6rem; }}
h1 span {{ color: var(--ok); }}
p {{ margin: 0 0 20px; color: var(--muted); }}
dl {{ display: grid; grid-template-columns: auto 1fr; gap: 6px 16px; margin: 0; }}
dt {{ color: var(--muted); }}
dd {{ margin: 0; font-variant-numeric: tabular-nums; overflow-wrap: anywhere; }}
</style>
</head>
<body>
<main>
<h1><span>&#10003;</span> It works</h1>
<p>{name} is on the network, provisioned with Improv Wi-Fi.</p>
<dl>
<dt>Address</dt><dd>{address}</dd>
<dt>Firmware</dt><dd>{firmware} {version}</dd>
<dt>Chip</dt><dd>{chip}</dd>
<dt>Up</dt><dd>{uptime}</dd>
</dl>
</main>
</body>
</html>
"#,
        name = about.name,
        firmware = about.firmware,
        version = about.version,
        chip = about.chip,
    )
}
