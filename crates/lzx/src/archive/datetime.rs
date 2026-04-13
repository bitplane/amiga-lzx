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

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: u16,   // absolute year, 1970..=2033
    pub month: u8,   // 1..=12
    pub day: u8,     // 1..=31
    pub hour: u8,    // 0..=23
    pub minute: u8,  // 0..=59
    pub second: u8,  // 0..=59
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
        let dt = DateTime { year, month, day, hour, minute, second };
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
}
