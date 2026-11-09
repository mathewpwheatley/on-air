//! The setup hotspot.

use alloc::format;
use alloc::string::String;

/// Name of the open WiFi network the device creates for setup: `OnAir-`
/// plus the last 4 hex digits of its WiFi MAC address, so each device has a
/// fixed name and two in one house don't clash. Short enough to fit across
/// the panel without scrolling.
pub fn network_name(mac: [u8; 6]) -> String {
    format!("OnAir-{:02X}{:02X}", mac[4], mac[5])
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::last_two_bytes_in_uppercase_hex([0x34, 0x85, 0x18, 0x00, 0x1a, 0x2b], "OnAir-1A2B")]
    #[case::zeros([0; 6], "OnAir-0000")]
    #[case::all_ones([0xff; 6], "OnAir-FFFF")]
    fn network_names(#[case] mac: [u8; 6], #[case] expected: &str) {
        assert_eq!(network_name(mac), expected);
    }
}
