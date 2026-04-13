//! LZX-specific date/time packing (1970 epoch, big-endian bit-packed).
//!
//! See ALGORITHM.md §11 "Packed date/time byte order". Layout:
//!
//! ```text
//! bits 31..27  day    (5 bits)
//! bits 26..23  month  (4 bits)
//! bits 22..17  year   (6 bits, + 1970)
//! bits 16..12  hour   (5 bits)
//! bits 11..6   minute (6 bits)
//! bits  5..0   second (6 bits)
//! ```
//!
//! Stored in 4 bytes **big-endian**, unlike the rest of the entry header.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: u16,  // absolute year, 1970..=2033
    pub month: u8,  // 1..=12
    pub day: u8,    // 1..=31
    pub hour: u8,   // 0..=23
    pub minute: u8, // 0..=59
    pub second: u8, // 0..=59
}

impl DateTime {
    pub const EPOCH: u16 = 1970;

    /// Sentinel "epoch zero" for tests and stub fields.
    pub const ZERO: DateTime = DateTime {
        year: 1970,
        month: 1,
        day: 1,
        hour: 0,
        minute: 0,
        second: 0,
    };

    pub fn try_new(
        year: u16,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
    ) -> Result<Self> {
        let dt = DateTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
        };
        dt.validate()?;
        Ok(dt)
    }

    fn validate(&self) -> Result<()> {
        if self.year < Self::EPOCH || self.year > Self::EPOCH + 63 {
            return Err(Error::DateOutOfRange("year must be 1970..=2033"));
        }
        if !(1..=12).contains(&self.month) {
            return Err(Error::DateOutOfRange("month must be 1..=12"));
        }
        if !(1..=31).contains(&self.day) {
            return Err(Error::DateOutOfRange("day must be 1..=31"));
        }
        if self.hour > 23 {
            return Err(Error::DateOutOfRange("hour must be 0..=23"));
        }
        if self.minute > 59 {
            return Err(Error::DateOutOfRange("minute must be 0..=59"));
        }
        if self.second > 59 {
            return Err(Error::DateOutOfRange("second must be 0..=59"));
        }
        Ok(())
    }

    pub fn pack(self) -> [u8; 4] {
        let temp = ((self.day as u32) << 27)
            | ((self.month as u32) << 23)
            | (((self.year - Self::EPOCH) as u32) << 17)
            | ((self.hour as u32) << 12)
            | ((self.minute as u32) << 6)
            | (self.second as u32);
        temp.to_be_bytes()
    }

    pub fn unpack(bytes: [u8; 4]) -> Self {
        let temp = u32::from_be_bytes(bytes);
        DateTime {
            day: ((temp >> 27) & 31) as u8,
            month: ((temp >> 23) & 15) as u8,
            year: (((temp >> 17) & 63) as u16) + Self::EPOCH,
            hour: ((temp >> 12) & 31) as u8,
            minute: ((temp >> 6) & 63) as u8,
            second: (temp & 63) as u8,
        }
    }

    /// Convert a `SystemTime` (interpreted as seconds since the Unix
    /// epoch) into an LZX `DateTime`, clamping out-of-range values to
    /// the supported window. Returns the resulting `DateTime` and a
    /// boolean indicating whether clamping occurred.
    pub fn from_system_time_clamped(t: SystemTime) -> (Self, bool) {
        // Pre-1970 → clamp to ZERO.
        let secs_since_epoch = match t.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs() as i64,
            Err(_) => return (Self::ZERO, true),
        };
        // 2034-01-01 00:00:00 UTC = 2 019 686 400 seconds since 1970.
        // Anything ≥ that clamps to 2033-12-31 23:59:59.
        const MAX_SECS: i64 = 2_019_686_400 - 1;
        let (clamped, secs) = if secs_since_epoch > MAX_SECS {
            (true, MAX_SECS)
        } else {
            (false, secs_since_epoch)
        };

        let days = (secs / 86_400) as i64;
        let day_secs = (secs % 86_400) as u32;
        let hour = (day_secs / 3600) as u8;
        let minute = ((day_secs % 3600) / 60) as u8;
        let second = (day_secs % 60) as u8;

        let (year, month, day) = days_to_civil(days);
        let dt = DateTime {
            year: year as u16,
            month,
            day,
            hour,
            minute,
            second,
        };
        (dt, clamped)
    }

    /// Convert this `DateTime` to a `SystemTime`. Always succeeds because
    /// the supported range fits comfortably inside `SystemTime`'s
    /// representable interval.
    pub fn to_system_time(self) -> SystemTime {
        let days = civil_to_days(self.year as i32, self.month, self.day);
        let secs = (days as i64) * 86_400
            + (self.hour as i64) * 3600
            + (self.minute as i64) * 60
            + (self.second as i64);
        UNIX_EPOCH + Duration::from_secs(secs as u64)
    }
}

/// Convert a civil date (year, month, day) to days since 1970-01-01.
/// Hinnant's algorithm — handles all proleptic Gregorian dates without
/// leap-year edge cases.
fn civil_to_days(y: i32, m: u8, d: u8) -> i32 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let m = m as u32;
    let d = d as u32;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe as i32 - 719468
}

/// Inverse of [`civil_to_days`]: convert days since 1970-01-01 back to
/// (year, month, day).
fn days_to_civil(z: i64) -> (i32, u8, u8) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i32 + era as i32 * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

impl Default for DateTime {
    fn default() -> Self {
        Self::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_round_trip() {
        let cases = [
            DateTime::ZERO,
            DateTime::try_new(2026, 4, 13, 3, 47, 22).unwrap(),
            DateTime::try_new(1999, 12, 31, 23, 59, 59).unwrap(),
            DateTime::try_new(2033, 12, 31, 23, 59, 59).unwrap(),
        ];
        for c in cases {
            let bytes = c.pack();
            let back = DateTime::unpack(bytes);
            assert_eq!(back, c);
        }
    }

    #[test]
    fn rejects_out_of_range() {
        assert!(DateTime::try_new(1969, 1, 1, 0, 0, 0).is_err());
        assert!(DateTime::try_new(2034, 1, 1, 0, 0, 0).is_err());
        assert!(DateTime::try_new(2026, 13, 1, 0, 0, 0).is_err());
        assert!(DateTime::try_new(2026, 1, 0, 0, 0, 0).is_err());
        assert!(DateTime::try_new(2026, 1, 1, 24, 0, 0).is_err());
    }

    #[test]
    fn epoch_round_trip() {
        let dt = DateTime::ZERO;
        let st = dt.to_system_time();
        assert_eq!(st, UNIX_EPOCH);
        let (back, clamped) = DateTime::from_system_time_clamped(st);
        assert!(!clamped);
        assert_eq!(back, dt);
    }

    #[test]
    fn known_instant_round_trip() {
        // 2024-06-15 10:30:45 UTC = 1718447445 seconds since epoch.
        let st = UNIX_EPOCH + Duration::from_secs(1_718_447_445);
        let (dt, clamped) = DateTime::from_system_time_clamped(st);
        assert!(!clamped);
        assert_eq!(dt.year, 2024);
        assert_eq!(dt.month, 6);
        assert_eq!(dt.day, 15);
        assert_eq!(dt.hour, 10);
        assert_eq!(dt.minute, 30);
        assert_eq!(dt.second, 45);
        // Round-trip back.
        let st2 = dt.to_system_time();
        assert_eq!(st2, st);
    }

    #[test]
    fn pre_1970_clamps_to_zero() {
        // SystemTime before UNIX_EPOCH is represented by a Result::Err
        // from duration_since.
        let st = UNIX_EPOCH - Duration::from_secs(60);
        let (dt, clamped) = DateTime::from_system_time_clamped(st);
        assert!(clamped);
        assert_eq!(dt, DateTime::ZERO);
    }

    #[test]
    fn post_2033_clamps_to_max() {
        // 2050-01-01 → must clamp.
        let secs_2050 = 2_524_608_000u64;
        let st = UNIX_EPOCH + Duration::from_secs(secs_2050);
        let (dt, clamped) = DateTime::from_system_time_clamped(st);
        assert!(clamped);
        assert_eq!(dt.year, 2033);
        assert_eq!(dt.month, 12);
        assert_eq!(dt.day, 31);
        assert_eq!(dt.hour, 23);
        assert_eq!(dt.minute, 59);
        assert_eq!(dt.second, 59);
    }

    #[test]
    fn leap_year_day() {
        // 2024-02-29 12:00:00 UTC.
        let dt = DateTime::try_new(2024, 2, 29, 12, 0, 0).unwrap();
        let st = dt.to_system_time();
        let (back, _) = DateTime::from_system_time_clamped(st);
        assert_eq!(back, dt);
    }

    #[test]
    fn last_valid_lzx_instant_round_trips() {
        let dt = DateTime::try_new(2033, 12, 31, 23, 59, 59).unwrap();
        let st = dt.to_system_time();
        let (back, clamped) = DateTime::from_system_time_clamped(st);
        assert!(!clamped);
        assert_eq!(back, dt);
    }
}
