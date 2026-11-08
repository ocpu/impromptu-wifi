//! The async loop that ties a [`Link`] and the backend traits to the
//! [`Machine`].

use core::convert::Infallible;

use embassy_futures::select::{select, Either};
use impromptu_wifi_proto::{
    command_id, Command, EncodeError, ErrorCode, RawRpc, ResultWriter, State, Transport,
};
use impromptu_wifi_traits::{
    AuthEvent, AuthSource, ConnectError, DeviceInfoSource, DeviceNameError, HostnameError,
    HostnameProvider, Identify, NetworkInfo, NetworkStatus, Url, WifiAuth, WifiConnect,
    WifiScanner,
};

use crate::link::Link;
use crate::machine::{Decision, Machine};

/// Size of the driver's receive and transmit buffers: the largest RPC frame.
pub const FRAME_BUF: usize = impromptu_wifi_proto::ble::MAX_RPC_FRAME;

/// Most networks reported per scan.
pub const SCAN_CAPACITY: usize = 16;

/// Drives the protocol over one link.
///
/// `W` is the Wi-Fi object and `D` the device object. Each implements its
/// optional traits either for real or with
/// [`unsupported!`](impromptu_wifi_traits::unsupported), and the driver answers
/// only the commands that are supported.
///
/// ```ignore
/// let mut driver = Driver::new(link, wifi, device, false);
/// let url = driver.provision(&mut NoAuth).await?;
/// ```
pub struct Driver<L, W, D> {
    link: L,
    wifi: W,
    device: D,
    machine: Machine,
    rx: [u8; FRAME_BUF],
    tx: [u8; FRAME_BUF],
    url: Option<Url>,
    last_connect_error: Option<ConnectError>,
}

impl<L, W, D> Driver<L, W, D>
where
    L: Link,
    W: WifiConnect + WifiScanner + NetworkStatus,
    D: DeviceInfoSource + Identify + HostnameProvider,
{
    /// A new driver. `require_auth` only has an effect on BLE.
    pub fn new(link: L, wifi: W, device: D, require_auth: bool) -> Self {
        Self {
            link,
            wifi,
            device,
            machine: Machine::new(L::TRANSPORT, require_auth),
            rx: [0; FRAME_BUF],
            tx: [0; FRAME_BUF],
            url: None,
            last_connect_error: None,
        }
    }

    /// The state machine, for inspection.
    pub fn machine(&self) -> &Machine {
        &self.machine
    }

    /// The state machine, for out-of-band changes such as
    /// [`Machine::set_available`]. Changes are published on the next step.
    pub fn machine_mut(&mut self) -> &mut Machine {
        &mut self.machine
    }

    /// Start out provisioned, for a device that was already online when the
    /// driver started (a rebooted Raspberry Pi, say). Clients asking for the
    /// state then hear "provisioned", with `url` as the redirect URL.
    pub fn mark_provisioned(&mut self, url: Option<Url>) {
        self.machine.mark_provisioned();
        self.url = url;
    }

    /// Why the most recent connection attempt failed, if it did. The client
    /// only ever sees "unable to connect"; this keeps the detail for logs.
    pub fn last_connect_error(&self) -> Option<ConnectError> {
        self.last_connect_error
    }

    /// The Wi-Fi object.
    pub fn wifi(&self) -> &W {
        &self.wifi
    }

    /// The device object.
    pub fn device(&self) -> &D {
        &self.device
    }

    /// Take the driver apart.
    pub fn into_parts(self) -> (L, W, D) {
        (self.link, self.wifi, self.device)
    }

    /// Serve commands until provisioned, then return the redirect URL.
    ///
    /// BLE devices typically stop the service after this. Serial devices
    /// often keep serving with [`run`](Self::run) so that tools can still
    /// query them.
    pub async fn provision<A: AuthSource>(
        &mut self,
        auth: &mut A,
    ) -> Result<Option<Url>, L::Error> {
        loop {
            self.step(auth).await?;
            if self.machine.state() == State::Provisioned {
                return Ok(self.url.clone());
            }
        }
    }

    /// Serve commands until the link fails.
    pub async fn run<A: AuthSource>(&mut self, auth: &mut A) -> Result<Infallible, L::Error> {
        loop {
            self.step(auth).await?;
        }
    }

    /// Publish pending updates, handle one command or authorization event,
    /// and publish again.
    pub async fn step<A: AuthSource>(&mut self, auth: &mut A) -> Result<(), L::Error> {
        publish(&mut self.link, &mut self.machine).await?;
        match select(self.link.recv(&mut self.rx), auth.next()).await {
            Either::First(received) => match received? {
                Some(len) => self.handle(len).await?,
                None => self.machine.on_rpc_error(ErrorCode::InvalidRpc),
            },
            Either::Second(AuthEvent::Authorize) => self.machine.authorize(),
            Either::Second(AuthEvent::Revoke) => self.machine.revoke(),
        }
        publish(&mut self.link, &mut self.machine).await
    }

    async fn handle(&mut self, len: usize) -> Result<(), L::Error> {
        let t = L::TRANSPORT;
        let Self {
            link,
            wifi,
            device,
            machine,
            rx,
            tx,
            url,
            last_connect_error,
        } = self;

        let parsed = RawRpc::parse(&rx[..len], t)
            .map_err(|_| ErrorCode::InvalidRpc)
            .and_then(|raw| Command::parse(t, raw));
        let command = match parsed {
            Ok(c) => c,
            Err(code) => {
                machine.on_rpc_error(code);
                return Ok(());
            }
        };
        let id = command.id();

        let decision = machine.on_command(command);
        if !matches!(decision, Decision::Reject(_)) {
            // The spec wants the error cleared before any result, and the
            // client should see "provisioning" before we block on a join.
            publish(link, machine).await?;
        }

        match decision {
            Decision::Reject(_) => {}

            Decision::Connect { ssid, password } => match wifi.connect(ssid, password).await {
                Ok(new_url) => {
                    *url = new_url;
                    *last_connect_error = None;
                    machine.on_connect_result(true);
                    publish(link, machine).await?;
                    let current = url.as_deref();
                    send_strings(link, machine, tx, id, current.as_slice()).await?;
                }
                Err(e) => {
                    *last_connect_error = Some(e);
                    machine.on_connect_result(false);
                }
            },

            Decision::ReportState => {
                if machine.state() == State::Provisioned {
                    let current = url.as_deref();
                    send_strings(
                        link,
                        machine,
                        tx,
                        command_id::WIFI_SETTINGS,
                        current.as_slice(),
                    )
                    .await?;
                }
            }

            Decision::Identify => {
                if <D as Identify>::SUPPORTED {
                    device.identify().await;
                }
            }

            Decision::DeviceInfo => {
                let mut all = [
                    device.firmware_name(),
                    device.firmware_version(),
                    device.chip(),
                    device.device_name(),
                    "",
                    "",
                ];
                let n = match (device.os_name(), device.os_version()) {
                    (Some(name), version) => {
                        all[4] = name;
                        all[5] = version.unwrap_or("");
                        6
                    }
                    (None, _) => 4,
                };
                send_strings(link, machine, tx, id, &all[..n]).await?;
            }

            Decision::Scan if !<W as WifiScanner>::SUPPORTED => {
                machine.on_rpc_error(ErrorCode::UnknownCommand)
            }
            Decision::Scan => {
                let mut networks = heapless::Vec::<NetworkInfo, SCAN_CAPACITY>::new();
                match wifi.scan(&mut networks).await {
                    Ok(()) => send_scan(link, machine, tx, id, &networks).await?,
                    Err(_) => machine.on_rpc_error(ErrorCode::Unknown),
                }
            }

            Decision::GetHostname | Decision::SetHostname(_)
                if !<D as HostnameProvider>::SUPPORTED =>
            {
                machine.on_rpc_error(ErrorCode::UnknownCommand)
            }
            Decision::GetHostname => {
                send_strings(link, machine, tx, id, &[device.hostname()]).await?
            }
            Decision::SetHostname(h) => match device.set_hostname(h).await {
                Ok(()) => send_strings(link, machine, tx, id, &[device.hostname()]).await?,
                Err(e) => machine.on_rpc_error(hostname_error_code(e)),
            },

            Decision::GetDeviceName => {
                send_strings(link, machine, tx, id, &[device.device_name()]).await?
            }
            Decision::SetDeviceName(n) => match device.set_device_name(n).await {
                Ok(()) => send_strings(link, machine, tx, id, &[device.device_name()]).await?,
                Err(e) => machine.on_rpc_error(device_name_error_code(e)),
            },

            Decision::NetworkState if !<W as NetworkStatus>::SUPPORTED => {
                machine.on_rpc_error(ErrorCode::UnknownCommand)
            }
            Decision::NetworkState => {
                let flags = wifi.network_flags().await;
                let online_url = match (flags.is_online(), url.clone()) {
                    (false, _) => None,
                    (true, Some(u)) => Some(u),
                    (true, None) => wifi.current_url().await,
                };
                let frame = build_network_state(tx, t, id, flags.0, online_url.as_deref());
                send_frame(link, machine, frame).await?;
            }
        }
        Ok(())
    }
}

fn hostname_error_code(e: HostnameError) -> ErrorCode {
    match e {
        HostnameError::InvalidName => ErrorCode::BadHostname,
        HostnameError::Unavailable => ErrorCode::UnknownCommand,
        _ => ErrorCode::Unknown,
    }
}

fn device_name_error_code(e: DeviceNameError) -> ErrorCode {
    match e {
        DeviceNameError::Unavailable => ErrorCode::UnknownCommand,
        _ => ErrorCode::Unknown,
    }
}

/// The auth-type string used in BLE scan results.
///
/// The spec lists WEP, WPA, WPA2, WPA2 EAP, WPA3, WAPI and NO, joined with
/// `/` for mixed modes. Methods outside that list map to the closest entry;
/// [`WifiAuth::Unknown`] is reported as `UNKNOWN`, which clients treat as
/// "not open" and so still ask for a passphrase.
pub fn ble_auth_name(auth: WifiAuth) -> &'static str {
    match auth {
        WifiAuth::Open | WifiAuth::Owe => "NO",
        WifiAuth::Wep => "WEP",
        WifiAuth::Wpa | WifiAuth::WpaEnterprise => "WPA",
        WifiAuth::Wpa2Personal => "WPA2",
        WifiAuth::WpaWpa2Personal => "WPA/WPA2",
        WifiAuth::Wpa2Enterprise => "WPA2 EAP",
        WifiAuth::Wpa3Personal | WifiAuth::Wpa3Enterprise => "WPA3",
        WifiAuth::Wpa2Wpa3Personal => "WPA2/WPA3",
        WifiAuth::Wpa2Wpa3Enterprise => "WPA2 EAP/WPA3",
        WifiAuth::WapiPersonal => "WAPI",
        _ => "UNKNOWN",
    }
}

async fn publish<L: Link>(link: &mut L, machine: &mut Machine) -> Result<(), L::Error> {
    if let Some(e) = machine.take_error() {
        link.send_error(e).await?;
    }
    if let Some(s) = machine.take_state() {
        link.send_state(s).await?;
    }
    Ok(())
}

async fn send_frame<L: Link>(
    link: &mut L,
    machine: &mut Machine,
    frame: Result<&[u8], EncodeError>,
) -> Result<(), L::Error> {
    match frame {
        Ok(f) => link.send_result(f).await,
        Err(_) => {
            machine.on_rpc_error(ErrorCode::Unknown);
            Ok(())
        }
    }
}

async fn send_strings<L: Link>(
    link: &mut L,
    machine: &mut Machine,
    tx: &mut [u8],
    id: u8,
    strings: &[&str],
) -> Result<(), L::Error> {
    let frame = build_strings(tx, L::TRANSPORT, id, strings);
    send_frame(link, machine, frame).await
}

/// Scan results differ by transport. Serial sends one result per network
/// (SSID, RSSI, `YES`/`NO`) followed by an empty result. BLE sends a single
/// result of SSID/RSSI/auth-type triples, as many as fit.
async fn send_scan<L: Link>(
    link: &mut L,
    machine: &mut Machine,
    tx: &mut [u8],
    id: u8,
    networks: &[NetworkInfo],
) -> Result<(), L::Error> {
    match L::TRANSPORT {
        Transport::Serial => {
            for n in networks {
                let frame = build_serial_network(tx, id, n);
                send_frame(link, machine, frame).await?;
            }
            send_strings(link, machine, tx, id, &[]).await
        }
        Transport::Ble => {
            let frame = build_ble_scan(tx, id, networks);
            send_frame(link, machine, frame).await
        }
    }
}

fn build_strings<'b>(
    tx: &'b mut [u8],
    t: Transport,
    id: u8,
    strings: &[&str],
) -> Result<&'b [u8], EncodeError> {
    let mut w = ResultWriter::new(tx, t, id)?;
    for s in strings {
        w.push(s)?;
    }
    w.finish()
}

fn build_network_state<'b>(
    tx: &'b mut [u8],
    t: Transport,
    id: u8,
    flags: u8,
    url: Option<&str>,
) -> Result<&'b [u8], EncodeError> {
    let mut w = ResultWriter::new(tx, t, id)?;
    w.push_display(flags)?;
    if let Some(u) = url {
        w.push(u)?;
    }
    w.finish()
}

fn build_serial_network<'b>(
    tx: &'b mut [u8],
    id: u8,
    n: &NetworkInfo,
) -> Result<&'b [u8], EncodeError> {
    let mut w = ResultWriter::new(tx, Transport::Serial, id)?;
    w.push(&n.ssid)?
        .push_display(n.rssi_dbm)?
        .push(if n.auth.requires_passphrase() {
            "YES"
        } else {
            "NO"
        })?;
    w.finish()
}

fn build_ble_scan<'b>(
    tx: &'b mut [u8],
    id: u8,
    networks: &[NetworkInfo],
) -> Result<&'b [u8], EncodeError> {
    let mut w = ResultWriter::new(tx, Transport::Ble, id)?;
    for n in networks {
        let mark = w.mark();
        let pushed = w
            .push(&n.ssid)
            .and_then(|w| w.push_display(n.rssi_dbm))
            .and_then(|w| w.push(ble_auth_name(n.auth)))
            .is_ok();
        if !pushed {
            // Keep only whole triples.
            w.rewind(mark);
            break;
        }
    }
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_names() {
        assert_eq!(ble_auth_name(WifiAuth::Owe), "NO");
        assert_eq!(ble_auth_name(WifiAuth::Wpa2Wpa3Personal), "WPA2/WPA3");
        assert_eq!(ble_auth_name(WifiAuth::Unknown), "UNKNOWN");
    }

    #[test]
    fn hostname_errors_map_to_protocol() {
        assert_eq!(
            hostname_error_code(HostnameError::InvalidName),
            ErrorCode::BadHostname
        );
        assert_eq!(
            hostname_error_code(HostnameError::Unavailable),
            ErrorCode::UnknownCommand
        );
        assert_eq!(
            hostname_error_code(HostnameError::Failed),
            ErrorCode::Unknown
        );
    }
}
