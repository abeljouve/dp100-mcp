//! Sans-I/O codec for the ALIENTEK DP100 USB HID protocol.
//!
//! This crate performs no I/O: it builds the 64-byte output reports sent to the
//! device and parses the 64-byte input reports it answers with. See
//! `docs/PROTOCOL.md` at the workspace root for the reverse-engineered
//! specification and the confidence level of every field.
//!
//! Firmware-upgrade function codes (0x11–0x15) are deliberately not exposed.

mod crc;
mod error;
mod frame;
mod message;
mod units;

pub use crc::crc16_modbus;
pub use error::ProtoError;
pub use frame::{DEVICE_ADDR, Frame, Func, HOST_ADDR, MAX_PAYLOAD, REPORT_LEN};
pub use message::{
    BasicInfo, DeviceInfo, OutMode, OutputSet, PresetIndex, Request, Response, SetpointRecord, Setpoints,
    SystemSettings, WorkState,
};
pub use units::{DeciCelsius, DeciWatts, Milliamps, Millivolts, UnitError, milliwatts};

/// USB vendor ID of the DP100.
pub const VID: u16 = 0x2E3C;
/// USB product ID of the DP100.
pub const PID: u16 = 0xAF01;

/// Absolute device limits, from the factory OVP/OCP maxima (30.5 V / 5.05 A)
/// and the 30 V / 5 A rating.
pub mod device_limits {
    use crate::{Milliamps, Millivolts};
    /// Rated maximum output voltage.
    pub const MAX_VOUT: Millivolts = Millivolts(30_000);
    /// Rated maximum output current.
    pub const MAX_IOUT: Milliamps = Milliamps(5_000);
    /// Maximum over-voltage protection threshold accepted by the firmware.
    pub const MAX_OVP: Millivolts = Millivolts(30_500);
    /// Maximum over-current protection threshold accepted by the firmware.
    pub const MAX_OCP: Milliamps = Milliamps(5_050);
    /// Over-temperature protection range, °C.
    pub const OTP_RANGE_C: core::ops::RangeInclusive<u16> = 50..=80;
    /// Maximum over-power protection, in 0.1 W.
    pub const MAX_OPP_DW: u16 = 1_050;
    /// Maximum backlight / volume level.
    pub const MAX_LEVEL: u8 = 4;
}
