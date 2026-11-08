//! `SharedWifi`: two handles to one backend, with support forwarded.

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use impromptu_wifi_core::traits::{
    unsupported, ConnectError, NetworkInfo, NetworkStatus, ScanError, Url, WifiAuth, WifiConnect,
    WifiScanner,
};
use impromptu_wifi_embassy::SharedWifi;

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
    let radio = Mutex::<NoopRawMutex, _>::new(Radio::default());
    let mut serial = SharedWifi::new(&radio);
    let mut ble = serial;

    let url = block_on(serial.connect("home", "pw")).unwrap();
    assert_eq!(url.as_deref(), Some("http://10.0.0.2"));
    assert_eq!(
        block_on(ble.connect("cafe", "pw")),
        Err(ConnectError::NetworkNotFound)
    );
    assert_eq!(
        block_on(radio.lock()).joins,
        2,
        "both went to the one radio"
    );

    let mut found = heapless::Vec::<NetworkInfo, 4>::new();
    block_on(ble.scan(&mut found)).unwrap();
    assert_eq!(found[0].ssid.as_str(), "home");

    const { assert!(<SharedWifi<'_, NoopRawMutex, Radio> as WifiScanner>::SUPPORTED) };
    const { assert!(!<SharedWifi<'_, NoopRawMutex, Radio> as NetworkStatus>::SUPPORTED) };
}
