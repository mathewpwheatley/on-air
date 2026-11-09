//! Local time for the clock, using the IANA timezone database bundled with
//! `jiff`. Zones are named like `America/Los_Angeles`, which is also what
//! the Open-Meteo city lookup returns.

pub use jiff::civil::DateTime;
pub use jiff::tz::TimeZone;

/// Look up a zone by IANA name. `None` for an unknown name.
pub fn find(name: &str) -> Option<TimeZone> {
    jiff::tz::db().get(name).ok()
}

/// Local wall-clock time at a Unix time in seconds. `None` only for a time
/// outside jiff's supported range.
pub fn local_time(zone: &TimeZone, unix_s: i64) -> Option<DateTime> {
    let timestamp = jiff::Timestamp::from_second(unix_s).ok()?;
    Some(zone.to_datetime(timestamp))
}

/// UTC offset in seconds east of UTC at a Unix time.
pub fn offset_s(zone: &TimeZone, unix_s: i64) -> Option<i32> {
    let timestamp = jiff::Timestamp::from_second(unix_s).ok()?;
    Some(zone.to_offset(timestamp).seconds())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zone(name: &str) -> TimeZone {
        find(name).unwrap_or_else(|| panic!("zone {name}"))
    }

    /// Transition instants checked against Python's zoneinfo.
    #[test]
    fn daylight_saving_transitions_2026() {
        let cases = [
            ("America/Los_Angeles", 1_772_963_999, -28_800),
            ("America/Los_Angeles", 1_772_964_000, -25_200),
            ("America/Los_Angeles", 1_793_523_599, -25_200),
            ("America/Los_Angeles", 1_793_523_600, -28_800),
            ("Europe/Berlin", 1_774_745_999, 3_600),
            ("Europe/Berlin", 1_774_746_000, 7_200),
            ("Australia/Sydney", 1_775_318_399, 39_600),
            ("Australia/Sydney", 1_775_318_400, 36_000),
            ("Asia/Kolkata", 1_775_318_400, 19_800),
            ("Asia/Kathmandu", 1_775_318_400, 20_700),
        ];
        for (name, unix_s, expected) in cases {
            assert_eq!(
                offset_s(&zone(name), unix_s),
                Some(expected),
                "{name} at {unix_s}"
            );
        }
    }

    #[test]
    fn local_wall_clock_time() {
        // 2026-11-08 21:30 UTC is 13:30 in Los Angeles, a Sunday.
        let t = local_time(&zone("America/Los_Angeles"), 1_794_173_400).unwrap();
        assert_eq!(
            (t.year(), t.month(), t.day(), t.hour(), t.minute()),
            (2026, 11, 8, 13, 30)
        );
        assert_eq!(t.weekday(), jiff::civil::Weekday::Sunday);
    }

    #[test]
    fn unknown_zone_is_none() {
        assert!(find("Mars/Olympus_Mons").is_none());
        assert!(find("").is_none());
    }
}
