//! Display-only conversion of ZFS creation times (Unix seconds in UTC).
//! Snapshot selectors and on-disk timestamps are never changed.
use chrono::{DateTime, Local, Utc};
use chrono_tz::Tz;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SnapshotTimeZone {
    #[default]
    Local,
    Named(Tz),
}

impl FromStr for SnapshotTimeZone {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "local" {
            Ok(Self::Local)
        } else {
            value.parse::<Tz>().map(Self::Named).map_err(|_| {
                format!("Unknown time zone {value:?}. Use local, UTC, or an IANA name such as America/New_York.")
            })
        }
    }
}

impl SnapshotTimeZone {
    pub fn format(self, timestamp: u64) -> Option<String> {
        // Zero is used by streams without a recorded creation time.
        if timestamp == 0 {
            return None;
        }
        let utc = DateTime::<Utc>::from_timestamp(i64::try_from(timestamp).ok()?, 0)?;
        Some(match self {
            Self::Local => utc
                .with_timezone(&Local)
                .format("%b %-d, %Y · %-I:%M:%S %p (UTC%:z)")
                .to_string(),
            Self::Named(zone) => utc
                .with_timezone(&zone)
                .format("%b %-d, %Y · %-I:%M:%S %p %Z (UTC%:z)")
                .to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seconds(iso: &str) -> u64 {
        DateTime::parse_from_rfc3339(iso).unwrap().timestamp() as u64
    }

    #[test]
    fn fall_back_hours_are_distinct_and_keep_the_same_instant() {
        let zone: SnapshotTimeZone = "America/New_York".parse().unwrap();
        assert_eq!(
            zone.format(seconds("2026-11-01T05:30:00Z")).unwrap(),
            "Nov 1, 2026 · 1:30:00 AM EDT (UTC-04:00)"
        );
        assert_eq!(
            zone.format(seconds("2026-11-01T06:30:00Z")).unwrap(),
            "Nov 1, 2026 · 1:30:00 AM EST (UTC-05:00)"
        );
    }

    #[test]
    fn conversions_handle_date_boundaries_and_fractional_offsets() {
        let utc = seconds("2026-09-22T00:15:00Z");
        assert!(
            "America/Los_Angeles"
                .parse::<SnapshotTimeZone>()
                .unwrap()
                .format(utc)
                .unwrap()
                .starts_with("Sep 21, 2026 · 5:15:00 PM")
        );
        assert_eq!(
            "Asia/Kathmandu"
                .parse::<SnapshotTimeZone>()
                .unwrap()
                .format(utc)
                .unwrap(),
            "Sep 22, 2026 · 6:00:00 AM +0545 (UTC+05:45)"
        );
        assert!(
            "UTC"
                .parse::<SnapshotTimeZone>()
                .unwrap()
                .format(utc)
                .unwrap()
                .starts_with("Sep 22, 2026 · 12:15:00 AM UTC")
        );
    }

    #[test]
    fn unknown_times_do_not_become_1970_and_invalid_zones_are_rejected() {
        assert_eq!(SnapshotTimeZone::default(), SnapshotTimeZone::Local);
        assert_eq!(SnapshotTimeZone::Local.format(0), None);
        assert_eq!(SnapshotTimeZone::Local.format(u64::MAX), None);
        assert!("Mars/Olympus".parse::<SnapshotTimeZone>().is_err());
        assert_eq!("local".parse(), Ok(SnapshotTimeZone::Local));
    }
}
