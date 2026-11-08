//! `WifiShare`: handles to one backend, support forwarded, joins announced.

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use impromptu_wifi_core::traits::{
    unsupported, ConnectError, DeviceEvent, EventSource, NetworkInfo, NetworkStatus, ScanError,
    Url, WifiAuth, WifiConnect, WifiScanner,
};
use impromptu_wifi_embassy::{SharedWifi, WifiShare};

type Share = WifiShare<NoopRawMutex, Radio, 2>;
type Handle<'a> = SharedWifi<'a, NoopRawMutex, Radio, 2>;

#[derive(Default)]
struct Radio {
    joins: u32,
}

impl WifiConnect for Radio {
    async fn connect(&mut self, ssid: &str, _: &str) -> Result<Option<Url>, ConnectError> {
        self.joins += 1;
        match ssid {
            "home" => Ok(Some(Url::try_from("http://10.0.0.2").unwrap())),
            _ => Err(ConnectError::NetworkNotFound),
        }
    }
}

impl WifiScanner for Radio {
    async fn scan<const N: usize>(
        &mut self,
        out: &mut heapless::Vec<NetworkInfo, N>,
    ) -> Result<(), ScanError> {
        let _ = out.push(NetworkInfo::new("home", -50, WifiAuth::Wpa2Personal).unwrap());
        Ok(())
    }
}

unsupported!(Radio: NetworkStatus);

#[test]
fn handles_share_one_backend_and_forward_support() {
    let share = Share::new(Radio::default());
    let mut serial = share.handle();
    let mut ble = serial;

    let url = block_on(serial.connect("home", "pw")).unwrap();
    assert_eq!(url.as_deref(), Some("http://10.0.0.2"));
    assert_eq!(
        block_on(ble.connect("cafe", "pw")),
        Err(ConnectError::NetworkNotFound)
    );
    assert_eq!(
        block_on(share.wifi().lock()).joins,
        2,
        "both went to the one radio"
    );

    let mut found = heapless::Vec::<NetworkInfo, 4>::new();
    block_on(ble.scan(&mut found)).unwrap();
    assert_eq!(found[0].ssid.as_str(), "home");

    const { assert!(<Handle<'_> as WifiScanner>::SUPPORTED) };
    const { assert!(!<Handle<'_> as NetworkStatus>::SUPPORTED) };
}

#[test]
fn joins_are_announced_to_every_driver() {
    let share = Share::new(Radio::default());
    let mut serial_events = share.events().unwrap();
    let mut ble_events = share.events().unwrap();
    assert!(share.events().is_none(), "only RECEIVERS of them");

    // A failed join announces nothing; a successful one reaches both.
    let mut ble = share.handle();
    assert!(block_on(ble.connect("cafe", "pw")).is_err());
    block_on(ble.connect("home", "pw")).unwrap();
    let expected = DeviceEvent::Provisioned(Some(Url::try_from("http://10.0.0.2").unwrap()));
    assert_eq!(block_on(serial_events.next()), expected);
    assert_eq!(block_on(ble_events.next()), expected);
}
