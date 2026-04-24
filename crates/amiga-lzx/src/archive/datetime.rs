//! LZX-specific date/time packing — 4 bytes, big-endian bit-packed.
//!
//! ```text
//! bits 31..27  day      (5 bits, 1..=31)
//! bits 26..23  month-1  (4 bits, 0..=11, i.e. 0 = January)
//! bits 22..17  year_fld (6 bits, see table)
//! bits 16..12  hour     (5 bits, 0..=23)
//! bits 11..6   minute   (6 bits, 0..=59)
//! bits  5..0   second   (6 bits, 0..=59)
//! ```
//!
//! The 6-bit year field is a **piecewise** mapping, not a single offset
//! — this is the real on-disk layout used by the original Amiga LZX,
//! verified against `Test_LZX.lzx` from the dr.Titus Y2K-fix package:
//!
//! | field  | year range   | offset |
//! |--------|--------------|--------|
//! |  8..29 | 1978..=1999  | +1970  |
//! | 58..63 | 2000..=2005  | +1942  |
//! | 30..57 | 2006..=2033  | +1976  |
//! |  0..7  | 2034..=2041  | +2034  |
//!
//! Old unlzx versions (and the un-fixed LZX 1.21R1) just apply
//! `field + 1970` across the board, which silently mis-decodes anything
//! outside the 1978..=1999 segment. The Y2K "fix" is a decoder change
//! — the encoding was always this shape.
//!
//! Stored in 4 bytes **big-endian**, unlike the rest of the entry header.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: u16,  // absolute year, 1978..=2041
    pub month: u8,  // 1..=12 (1-based in the struct; stored 0-based on disk)
    pub day: u8,    // 1..=31
    pub hour: u8,   // 0..=23
    pub minute: u8, // 0..=59
    pub second: u8, // 0..=59
}

impl DateTime {
    pub const MIN_YEAR: u16 = 1978;
    pub const MAX_YEAR: u16 = 2041;

    /// Lowest representable instant: 1978-01-01 00:00:00. Used as the
    /// default when no mtime is available and as the pre-range clamp
    /// target.
    pub const ZERO: DateTime = DateTime {
        year: 1978,
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
        if self.year < Self::MIN_YEAR || self.year > Self::MAX_YEAR {
            return Err(Error::DateOutOfRange("year must be 1978..=2041"));
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
        let year_fld: u32 = match self.year {
            1978..=1999 => (self.year - 1970) as u32,
            2000..=2005 => (self.year - 1942) as u32,
            2006..=2033 => (self.year - 1976) as u32,
            2034..=2041 => (self.year - 2034) as u32,
            _ => 0, // validated range; unreachable in practice
        };
        let month_on_disk = (self.month as u32).saturating_sub(1) & 0x0F;
        let temp = ((self.day as u32) << 27)
            | (month_on_disk << 23)
            | (year_fld << 17)
            | ((self.hour as u32) << 12)
            | ((self.minute as u32) << 6)
            | (self.second as u32);
        temp.to_be_bytes()
    }

    pub fn unpack(bytes: [u8; 4]) -> Self {
        let temp = u32::from_be_bytes(bytes);
        let year_fld = ((temp >> 17) & 63) as u16;
        let year = match year_fld {
            0..=7 => year_fld + 2034,
            8..=29 => year_fld + 1970,
            30..=57 => year_fld + 1976,
            _ => year_fld + 1942, // 58..=63
        };
        DateTime {
            day: ((temp >> 27) & 31) as u8,
            month: (((temp >> 23) & 15) + 1) as u8,
            year,
            hour: ((temp >> 12) & 31) as u8,
            minute: ((temp >> 6) & 63) as u8,
            second: (temp & 63) as u8,
        }
    }

    /// Convert a `SystemTime` (interpreted as seconds since the Unix
    /// epoch) into an LZX `DateTime`, clamping out-of-range values to
    /// the supported 1978..=2041 window. Returns the resulting
    /// `DateTime` and a boolean indicating whether clamping occurred.
    pub fn from_system_time_clamped(t: SystemTime) -> (Self, bool) {
        // 1978-01-01 00:00:00 UTC and 2042-01-01 00:00:00 UTC in
        // seconds since the Unix epoch. Using `civil_to_days` here
        // would work too, but these are constants so we inline them.
        const MIN_SECS: i64 = 252_460_800; // 1978-01-01
        const MAX_SECS: i64 = 2_272_147_200 - 1; // 2041-12-31 23:59:59

        let secs_since_epoch = match t.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs() as i64,
            Err(_) => return (Self::ZERO, true),
        };
        if secs_since_epoch < MIN_SECS {
            return (Self::ZERO, true);
        }
        let (clamped, secs) = if secs_since_epoch > MAX_SECS {
            (true, MAX_SECS)
        } else {
            (false, secs_since_epoch)
        };

        let days = secs / 86_400;
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
            DateTime::try_new(2041, 12, 31, 23, 59, 59).unwrap(),
            // One from each of the four year-encoding segments.
            DateTime::try_new(1978, 1, 1, 0, 0, 0).unwrap(),
            DateTime::try_new(2003, 7, 4, 12, 34, 56).unwrap(),
            DateTime::try_new(2020, 4, 19, 11, 33, 0).unwrap(),
            DateTime::try_new(2040, 6, 15, 18, 0, 0).unwrap(),
        ];
        for c in cases {
            let bytes = c.pack();
            let back = DateTime::unpack(bytes);
            assert_eq!(back, c, "round-trip failed for {c:?} -> {bytes:02x?}");
        }
    }

    #[test]
    fn rejects_out_of_range() {
        assert!(DateTime::try_new(1977, 1, 1, 0, 0, 0).is_err());
        assert!(DateTime::try_new(2042, 1, 1, 0, 0, 0).is_err());
        assert!(DateTime::try_new(2026, 13, 1, 0, 0, 0).is_err());
        assert!(DateTime::try_new(2026, 1, 0, 0, 0, 0).is_err());
        assert!(DateTime::try_new(2026, 1, 1, 24, 0, 0).is_err());
    }

    #[test]
    fn y2k_fix_sample_decodes_to_2026() {
        // date.lzx from issue #5 — real archive made on 2026-04-19.
        let bytes = [0x99, 0xe4, 0xb8, 0x40];
        let dt = DateTime::unpack(bytes);
        assert_eq!(dt.year, 2026);
        assert_eq!(dt.month, 4);
        assert_eq!(dt.day, 19);
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
        let st2 = dt.to_system_time();
        assert_eq!(st2, st);
    }

    #[test]
    fn pre_1978_clamps_to_zero() {
        // Unix epoch itself is before the LZX minimum (1978) — clamp.
        let (dt, clamped) = DateTime::from_system_time_clamped(UNIX_EPOCH);
        assert!(clamped);
        assert_eq!(dt, DateTime::ZERO);

        // And anything before UNIX_EPOCH clamps the same way.
        let pre = UNIX_EPOCH - Duration::from_secs(60);
        let (dt, clamped) = DateTime::from_system_time_clamped(pre);
        assert!(clamped);
        assert_eq!(dt, DateTime::ZERO);
    }

    #[test]
    fn post_2041_clamps_to_max() {
        // 2050-01-01 → must clamp.
        let secs_2050 = 2_524_608_000u64;
        let st = UNIX_EPOCH + Duration::from_secs(secs_2050);
        let (dt, clamped) = DateTime::from_system_time_clamped(st);
        assert!(clamped);
        assert_eq!(dt.year, 2041);
        assert_eq!(dt.month, 12);
        assert_eq!(dt.day, 31);
        assert_eq!(dt.hour, 23);
        assert_eq!(dt.minute, 59);
        assert_eq!(dt.second, 59);
    }

    #[test]
    fn leap_year_day() {
        let dt = DateTime::try_new(2024, 2, 29, 12, 0, 0).unwrap();
        let st = dt.to_system_time();
        let (back, _) = DateTime::from_system_time_clamped(st);
        assert_eq!(back, dt);
    }

    #[test]
    fn last_valid_lzx_instant_round_trips() {
        let dt = DateTime::try_new(2041, 12, 31, 23, 59, 59).unwrap();
        let st = dt.to_system_time();
        let (back, clamped) = DateTime::from_system_time_clamped(st);
        assert!(!clamped);
        assert_eq!(back, dt);
    }
}
