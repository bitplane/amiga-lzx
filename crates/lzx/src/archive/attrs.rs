//! Amiga file attribute byte (HSPARWED).

use bitflags::bitflags;

bitflags! {
    /// Amiga file protection bits, packed into one byte.
    /// Layout: H S P A R W E D (high to low).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct EntryAttrs: u8 {
        const HOLD    = 0b1000_0000;
        const SCRIPT  = 0b0100_0000;
        const PURE    = 0b0010_0000;
        const ARCHIVE = 0b0001_0000;
        const READ    = 0b0000_1000;
        const WRITE   = 0b0000_0100;
        const EXECUTE = 0b0000_0010;
        const DELETE  = 0b0000_0001;
    }
}

impl Default for EntryAttrs {
    /// Matches `entry_attr_default = 0x07` from the Amiga compressor —
    /// READ | WRITE | EXECUTE.
    fn default() -> Self {
        EntryAttrs::READ | EntryAttrs::WRITE | EntryAttrs::EXECUTE
    }
}
