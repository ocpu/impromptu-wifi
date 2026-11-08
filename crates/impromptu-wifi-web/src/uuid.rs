//! UUIDs in the form the browser APIs take.

/// Format a 128-bit UUID as lowercase hyphenated text.
pub fn uuid_string(u: u128) -> String {
    let h = format!("{u:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use impromptu_wifi_client::proto::ble::SERVICE_UUID;

    #[test]
    fn service_uuid_string() {
        assert_eq!(
            uuid_string(SERVICE_UUID),
            "00467768-6228-2272-4663-277478268000"
        );
    }
}
