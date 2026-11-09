//! The setup hotspot.

use alloc::format;
use alloc::string::String;

/// Name of the open WiFi network the device creates for setup: `OnAir-Setup-`
/// plus the last 4 hex digits of its WiFi MAC address, so each device has a
/// fixed name and two in one house don't clash.
pub fn network_name(mac: [u8; 6]) -> String {
    format!("OnAir-Setup-{:02X}{:02X}", mac[4], mac[5])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_last_two_bytes_in_uppercase_hex() {
        assert_eq!(
            network_name([0x34, 0x85, 0x18, 0x00, 0x1a, 0x2b]),
            "OnAir-Setup-1A2B"
        );
        assert_eq!(network_name([0; 6]), "OnAir-Setup-0000");
        assert_eq!(network_name([0xff; 6]), "OnAir-Setup-FFFF");
    }
}
