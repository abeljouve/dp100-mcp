//! Typed requests and responses on top of [`Frame`].

use crate::{DeciCelsius, DeciWatts, Frame, Func, Milliamps, Millivolts, ProtoError, units::milliwatts};

/// Flag bits in the first byte of a `BASIC_SET` (0x35) payload.
mod flag {
    /// Apply V/I/state to the live output (len 10).
    pub const APPLY: u8 = 0x20;
    /// Save to preset storage (len 10).
    pub const SAVE: u8 = 0x40;
    /// Recall a preset (len 10) / read the active set (len 1).
    pub const ACTIVE: u8 = 0x80;
}

/// Preset slot 0–9.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PresetIndex(u8);

impl PresetIndex {
    pub fn new(i: u8) -> Result<Self, ProtoError> {
        if i <= 9 { Ok(Self(i)) } else { Err(ProtoError::BadPreset(i)) }
    }
    pub fn get(self) -> u8 {
        self.0
    }
    /// Slot 0 can be read but not saved or recalled (vendor DLL behaviour).
    pub fn writable(self) -> Result<Self, ProtoError> {
        if self.0 == 0 { Err(ProtoError::PresetZeroReadOnly) } else { Ok(self) }
    }
}

/// Voltage/current setpoints and per-set protections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Setpoints {
    pub voltage: Millivolts,
    pub current: Milliamps,
    /// Over-voltage protection threshold.
    pub ovp: Millivolts,
    /// Over-current protection threshold.
    pub ocp: Milliamps,
}

/// A `BASIC_SET` record as read back from the device (active set or preset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SetpointRecord {
    /// Preset number (low nibble of the index byte).
    pub index: u8,
    pub output_on: bool,
    pub setpoints: Setpoints,
}

/// Live-output write (`0x20 | preset`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputSet {
    /// Active preset number; take it from the active-set read.
    pub preset: PresetIndex,
    pub output_on: bool,
    pub setpoints: Setpoints,
}

fn encode_set(index: u8, state: u8, s: &Setpoints) -> [u8; 10] {
    let mut p = [0u8; 10];
    p[0] = index;
    p[1] = state;
    p[2..4].copy_from_slice(&s.voltage.0.to_le_bytes());
    p[4..6].copy_from_slice(&s.current.0.to_le_bytes());
    p[6..8].copy_from_slice(&s.ovp.0.to_le_bytes());
    p[8..10].copy_from_slice(&s.ocp.0.to_le_bytes());
    p
}

fn u16_at(p: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([p[off], p[off + 1]])
}

impl SetpointRecord {
    fn parse(p: &[u8]) -> Result<Self, ProtoError> {
        if p.len() != 10 {
            return Err(ProtoError::PayloadLen { func: Func::BasicSet, expected: "10", got: p.len() });
        }
        Ok(Self {
            index: p[0] & 0x0F,
            output_on: p[1] != 0,
            setpoints: Setpoints {
                voltage: Millivolts(u16_at(p, 2)),
                current: Milliamps(u16_at(p, 4)),
                ovp: Millivolts(u16_at(p, 6)),
                ocp: Milliamps(u16_at(p, 8)),
            },
        })
    }
}

/// `DEVICE_INFO` (0x10) reply.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DeviceInfo {
    pub model: String,
    /// Version × 10 (14 = 1.4).
    pub hw_version: u16,
    pub fw_version: u16,
    pub boot_version: u16,
    /// `0xAA` when running the application, anything else means bootloader.
    pub run_area: u16,
    /// 12-byte device serial (likely the MCU UID).
    pub serial: [u8; 12],
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

fn version_string(v: u16) -> String {
    format!("{}.{}", v / 10, v % 10)
}

impl DeviceInfo {
    fn parse(p: &[u8]) -> Result<Self, ProtoError> {
        if p.len() != 40 {
            return Err(ProtoError::PayloadLen { func: Func::DeviceInfo, expected: "40", got: p.len() });
        }
        let name = &p[0..16];
        let end = name.iter().position(|&b| b == 0 || b == 0xFF).unwrap_or(16);
        let mut serial = [0u8; 12];
        serial.copy_from_slice(&p[24..36]);
        Ok(Self {
            model: String::from_utf8_lossy(&name[..end]).into_owned(),
            hw_version: u16_at(p, 16),
            fw_version: u16_at(p, 18),
            boot_version: u16_at(p, 20),
            run_area: u16_at(p, 22),
            serial,
            year: u16_at(p, 36),
            month: p[38],
            day: p[39],
        })
    }
    pub fn in_application(&self) -> bool {
        self.run_area == 0xAA
    }
    pub fn hw_version_str(&self) -> String {
        version_string(self.hw_version)
    }
    pub fn fw_version_str(&self) -> String {
        version_string(self.fw_version)
    }
    pub fn boot_version_str(&self) -> String {
        version_string(self.boot_version)
    }
    /// Full 12-byte serial as upper-case hex.
    pub fn serial_hex(&self) -> String {
        self.serial.iter().map(|b| format!("{b:02X}")).collect()
    }
    /// The 8-hex-digit form shown by the vendor software (last 4 bytes).
    pub fn serial_short(&self) -> String {
        self.serial_hex()[16..].to_string()
    }
    pub fn date_str(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// Output regulation mode (`out_mode`), all three values verified on
/// hardware (docs/PROTOCOL.md §5.2). This is the reliable indicator of the
/// real output state; the active set's `state` byte is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum OutMode {
    Cc,
    Cv,
    Off,
    Unknown(u8),
}

impl From<u8> for OutMode {
    fn from(v: u8) -> Self {
        match v {
            0 => OutMode::Cc,
            1 => OutMode::Cv,
            2 => OutMode::Off,
            x => OutMode::Unknown(x),
        }
    }
}

impl OutMode {
    pub fn as_str(self) -> &'static str {
        match self {
            OutMode::Cc => "CC",
            OutMode::Cv => "CV",
            OutMode::Off => "OFF",
            OutMode::Unknown(_) => "UNKNOWN",
        }
    }
}

/// Protection / work state (`work_st`). Only 0 = normal is verified; the
/// other values follow DP100-Lab and are unverified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum WorkState {
    Normal,
    Ovp,
    Ocp,
    Opp,
    Otp,
    ReverseProtection,
    Uvp,
    Unknown(u8),
}

impl From<u8> for WorkState {
    fn from(v: u8) -> Self {
        match v {
            0 => WorkState::Normal,
            1 => WorkState::Ovp,
            2 => WorkState::Ocp,
            3 => WorkState::Opp,
            4 => WorkState::Otp,
            5 => WorkState::ReverseProtection,
            6 => WorkState::Uvp,
            x => WorkState::Unknown(x),
        }
    }
}

impl WorkState {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkState::Normal => "NORMAL",
            WorkState::Ovp => "OVP",
            WorkState::Ocp => "OCP",
            WorkState::Opp => "OPP",
            WorkState::Otp => "OTP",
            WorkState::ReverseProtection => "REVERSE",
            WorkState::Uvp => "UVP",
            WorkState::Unknown(_) => "UNKNOWN",
        }
    }
    pub fn is_tripped(self) -> bool {
        self != WorkState::Normal
    }
}

/// `BASIC_INFO` (0x30) telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BasicInfo {
    pub vin: Millivolts,
    pub vout: Millivolts,
    pub iout: Milliamps,
    /// Highest output voltage the regulator can reach at the current Vin.
    pub vo_max: Millivolts,
    pub temp1: DeciCelsius,
    pub temp2: DeciCelsius,
    pub dc_5v: Millivolts,
    pub out_mode_raw: u8,
    pub work_state_raw: u8,
}

impl BasicInfo {
    fn parse(p: &[u8]) -> Result<Self, ProtoError> {
        if p.len() != 16 {
            return Err(ProtoError::PayloadLen { func: Func::BasicInfo, expected: "16", got: p.len() });
        }
        Ok(Self {
            vin: Millivolts(u16_at(p, 0)),
            vout: Millivolts(u16_at(p, 2)),
            iout: Milliamps(u16_at(p, 4)),
            vo_max: Millivolts(u16_at(p, 6)),
            temp1: DeciCelsius(u16_at(p, 8) as i16),
            temp2: DeciCelsius(u16_at(p, 10) as i16),
            dc_5v: Millivolts(u16_at(p, 12)),
            out_mode_raw: p[14],
            work_state_raw: p[15],
        })
    }
    pub fn out_mode(&self) -> OutMode {
        self.out_mode_raw.into()
    }
    pub fn work_state(&self) -> WorkState {
        self.work_state_raw.into()
    }
    /// Output power in milliwatts.
    pub fn power_mw(&self) -> f64 {
        milliwatts(self.vout, self.iout)
    }
}

/// `SYSTEM_INFO` (0x40) settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SystemSettings {
    /// Over-temperature protection, °C.
    pub otp_c: u16,
    /// Over-power protection.
    pub opp: DeciWatts,
    /// Backlight level 0–4.
    pub backlight: u8,
    /// Beeper volume 0–4.
    pub volume: u8,
    /// Reverse-polarity protection.
    pub reverse_protection: bool,
    /// Enable the output at power-up.
    pub auto_output: bool,
}

impl SystemSettings {
    fn parse(p: &[u8]) -> Result<Self, ProtoError> {
        // Older firmware answers 6 bytes without the two flags (vendor DLL).
        if p.len() != 8 && p.len() != 6 {
            return Err(ProtoError::PayloadLen { func: Func::SystemInfo, expected: "6 or 8", got: p.len() });
        }
        Ok(Self {
            otp_c: u16_at(p, 0),
            opp: DeciWatts(u16_at(p, 2)),
            backlight: p[4],
            volume: p[5],
            reverse_protection: p.get(6).is_some_and(|&b| b != 0),
            auto_output: p.get(7).is_some_and(|&b| b != 0),
        })
    }
    fn encode(&self) -> [u8; 8] {
        let mut p = [0u8; 8];
        p[0..2].copy_from_slice(&self.otp_c.to_le_bytes());
        p[2..4].copy_from_slice(&self.opp.0.to_le_bytes());
        p[4] = self.backlight;
        p[5] = self.volume;
        p[6] = u8::from(self.reverse_protection);
        p[7] = u8::from(self.auto_output);
        p
    }
}

/// Every operation the device layer can perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    DeviceInfo,
    BasicInfo,
    /// Read the active set: live setpoints and output state.
    ActiveSet,
    /// Read preset N.
    Preset(PresetIndex),
    SystemSettings,
    /// Write live setpoints / output state (`0x20 | preset`).
    ApplyOutput(OutputSet),
    /// Store a preset in flash (`0x40 | n`, n ≠ 0).
    SavePreset {
        index: PresetIndex,
        setpoints: Setpoints,
    },
    /// Recall a preset (`0x80 | n`, n ≠ 0). The setpoints should be the
    /// preset's stored values, read beforehand.
    RecallPreset {
        index: PresetIndex,
        setpoints: Setpoints,
    },
    WriteSystemSettings(SystemSettings),
}

/// Decoded reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    DeviceInfo(DeviceInfo),
    BasicInfo(BasicInfo),
    Setpoints(SetpointRecord),
    SystemSettings(SystemSettings),
    /// Write acknowledged (`01`).
    Ack,
}

impl Request {
    pub fn func(&self) -> Func {
        match self {
            Request::DeviceInfo => Func::DeviceInfo,
            Request::BasicInfo => Func::BasicInfo,
            Request::ActiveSet
            | Request::Preset(_)
            | Request::ApplyOutput(_)
            | Request::SavePreset { .. }
            | Request::RecallPreset { .. } => Func::BasicSet,
            Request::SystemSettings | Request::WriteSystemSettings(_) => Func::SystemInfo,
        }
    }

    /// True for requests that change device state.
    pub fn is_write(&self) -> bool {
        matches!(
            self,
            Request::ApplyOutput(_)
                | Request::SavePreset { .. }
                | Request::RecallPreset { .. }
                | Request::WriteSystemSettings(_)
        )
    }

    /// Build the frame for this request.
    pub fn to_frame(&self, seq: u8) -> Result<Frame, ProtoError> {
        let f = self.func();
        match self {
            Request::DeviceInfo | Request::BasicInfo | Request::SystemSettings => Frame::new(f, seq, &[]),
            Request::ActiveSet => Frame::new(f, seq, &[flag::ACTIVE]),
            Request::Preset(i) => Frame::new(f, seq, &[i.get()]),
            Request::ApplyOutput(o) => {
                Frame::new(f, seq, &encode_set(flag::APPLY | o.preset.get(), u8::from(o.output_on), &o.setpoints))
            }
            Request::SavePreset { index, setpoints } => {
                let i = index.writable()?;
                Frame::new(f, seq, &encode_set(flag::SAVE | i.get(), 0, setpoints))
            }
            Request::RecallPreset { index, setpoints } => {
                let i = index.writable()?;
                Frame::new(f, seq, &encode_set(flag::ACTIVE | i.get(), 0, setpoints))
            }
            Request::WriteSystemSettings(s) => Frame::new(f, seq, &s.encode()),
        }
    }

    /// Interpret the device's reply to this request.
    pub fn parse_response(&self, frame: &Frame) -> Result<Response, ProtoError> {
        let expected = self.func();
        if frame.func != expected {
            return Err(ProtoError::FuncMismatch { expected, got: frame.func });
        }
        let p = frame.payload();
        if self.is_write() {
            return match p {
                [1] => Ok(Response::Ack),
                [0] => Err(ProtoError::Nak(expected)),
                _ => Err(ProtoError::PayloadLen { func: expected, expected: "1", got: p.len() }),
            };
        }
        // A one-byte zero payload on a read is the device's NAK (seen on bad CRC).
        if p == [0] {
            return Err(ProtoError::Nak(expected));
        }
        Ok(match self {
            Request::DeviceInfo => Response::DeviceInfo(DeviceInfo::parse(p)?),
            Request::BasicInfo => Response::BasicInfo(BasicInfo::parse(p)?),
            Request::ActiveSet | Request::Preset(_) => Response::Setpoints(SetpointRecord::parse(p)?),
            Request::SystemSettings => Response::SystemSettings(SystemSettings::parse(p)?),
            _ => unreachable!("writes handled above"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(s: &str) -> Frame {
        let mut r: Vec<u8> = s.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect();
        r.resize(64, 0);
        Frame::decode(&r).unwrap()
    }

    fn parse(req: Request, s: &str) -> Response {
        req.parse_response(&report(s)).unwrap()
    }

    // ---- Vectors captured on real hardware, 2026-10-17 (fw 1.5) ----

    #[test]
    fn hw_device_info() {
        let Response::DeviceInfo(d) = parse(
            Request::DeviceInfo,
            "FA 10 00 28 41 54 4B 2D 44 50 31 30 30 00 FF FF FF FF FF FF 0E 00 0F 00 0B 00 AA 00 \
             96 E2 99 0E 00 40 75 30 03 A7 4D 05 EA 07 09 1C 92 67",
        ) else {
            panic!()
        };
        assert_eq!(d.model, "ATK-DP100");
        assert_eq!(
            (d.hw_version_str(), d.fw_version_str(), d.boot_version_str()),
            ("1.4".into(), "1.5".into(), "1.1".into())
        );
        assert!(d.in_application());
        assert_eq!(d.serial_hex(), "96E2990E0040753003A74D05");
        assert_eq!(d.serial_short(), "03A74D05");
        assert_eq!(d.date_str(), "2026-09-28");
    }

    #[test]
    fn hw_basic_info() {
        let Response::BasicInfo(b) =
            parse(Request::BasicInfo, "FA 30 00 10 77 3B 84 13 00 00 C6 39 1C 01 1A 01 DB 13 01 00 36 26")
        else {
            panic!()
        };
        assert_eq!(b.vin, Millivolts(15223));
        assert_eq!(b.vout, Millivolts(4996));
        assert_eq!(b.iout, Milliamps(0));
        assert_eq!(b.vo_max, Millivolts(14790));
        assert_eq!(b.temp1, DeciCelsius(284));
        assert_eq!(b.temp2, DeciCelsius(282));
        assert_eq!(b.dc_5v, Millivolts(5083));
        assert_eq!(b.out_mode(), OutMode::Cv);
        assert_eq!(b.work_state(), WorkState::Normal);
    }

    #[test]
    fn hw_active_set_and_presets() {
        let Response::Setpoints(s) = parse(Request::ActiveSet, "FA 35 00 0A 00 01 88 13 F4 01 24 77 BA 13 BB B2")
        else {
            panic!()
        };
        assert_eq!(s.index, 0);
        assert!(s.output_on);
        assert_eq!(
            s.setpoints,
            Setpoints {
                voltage: Millivolts(5000),
                current: Milliamps(500),
                ovp: Millivolts(30500),
                ocp: Milliamps(5050)
            }
        );
        let p9 = PresetIndex::new(9).unwrap();
        let Response::Setpoints(s) = parse(Request::Preset(p9), "FA 35 00 0A 09 00 10 27 88 13 24 77 BA 13 A8 FB")
        else {
            panic!()
        };
        assert_eq!(
            (s.index, s.output_on, s.setpoints.voltage, s.setpoints.current),
            (9, false, Millivolts(10000), Milliamps(5000))
        );
    }

    #[test]
    fn hw_system_settings() {
        let Response::SystemSettings(s) = parse(Request::SystemSettings, "FA 40 00 08 50 00 1A 04 02 02 01 00 5A 34")
        else {
            panic!()
        };
        assert_eq!(
            s,
            SystemSettings {
                otp_c: 80,
                opp: DeciWatts(1050),
                backlight: 2,
                volume: 2,
                reverse_protection: true,
                auto_output: false
            }
        );
        assert_eq!(s.encode(), [0x50, 0x00, 0x1A, 0x04, 0x02, 0x02, 0x01, 0x00]);
    }

    #[test]
    fn hw_nak_on_read() {
        let r = Request::BasicInfo.parse_response(&report("FA 30 00 01 00 F2 84"));
        assert_eq!(r, Err(ProtoError::Nak(Func::BasicInfo)));
    }

    // ---- Third-party captures ----

    #[test]
    fn weigu_basic_info_output_on_and_off() {
        // Output on, 5 V / 23 mA load.
        let Response::BasicInfo(b) =
            parse(Request::BasicInfo, "FA 30 00 10 BD 4E 8D 13 17 00 5E 4C 43 01 48 01 CD 13 01 00 F9 48")
        else {
            panic!()
        };
        assert_eq!((b.vin, b.vout, b.iout), (Millivolts(20157), Millivolts(5005), Milliamps(23)));
        assert_eq!(b.out_mode(), OutMode::Cv);
        assert!((b.power_mw() - 115.115).abs() < 1e-9);
        // Output off.
        let Response::BasicInfo(b) =
            parse(Request::BasicInfo, "FA 30 00 10 C8 4E 00 00 00 00 5E 4C 2F 01 2D 01 CE 13 02 00 92 29")
        else {
            panic!()
        };
        assert_eq!(b.out_mode(), OutMode::Off);
    }

    #[test]
    fn pydp100_ack_and_legacy_capture() {
        let p = PresetIndex::new(0).unwrap();
        let set = OutputSet {
            preset: p,
            output_on: true,
            setpoints: Setpoints {
                voltage: Millivolts(5000),
                current: Milliamps(2000),
                ovp: Millivolts(30500),
                ocp: Milliamps(5050),
            },
        };
        assert_eq!(Request::ApplyOutput(set).parse_response(&report("FA 35 00 01 01 33 88")), Ok(Response::Ack));
        let nak = Frame::new(Func::BasicSet, 0, &[0]).unwrap();
        assert_eq!(Request::ApplyOutput(set).parse_response(&nak), Err(ProtoError::Nak(Func::BasicSet)));
    }

    #[test]
    fn legacy_six_byte_system_info() {
        let f = Frame::new(Func::SystemInfo, 0, &[0x46, 0, 0x1A, 0x04, 1, 3]).unwrap();
        let Response::SystemSettings(s) = Request::SystemSettings.parse_response(&f).unwrap() else { panic!() };
        assert_eq!((s.otp_c, s.backlight, s.volume, s.reverse_protection, s.auto_output), (70, 1, 3, false, false));
    }

    // ---- Encoding of write requests ----

    #[test]
    fn encode_apply_output() {
        let set = OutputSet {
            preset: PresetIndex::new(0).unwrap(),
            output_on: false,
            setpoints: Setpoints {
                voltage: Millivolts(5000),
                current: Milliamps(500),
                ovp: Millivolts(30500),
                ocp: Milliamps(5050),
            },
        };
        let f = Request::ApplyOutput(set).to_frame(3).unwrap();
        assert_eq!(f.payload(), &[0x20, 0x00, 0x88, 0x13, 0xF4, 0x01, 0x24, 0x77, 0xBA, 0x13]);
        assert_eq!(f.seq, 3);
        let enc = f.encode();
        assert_eq!(&enc[..4], &[0xFB, 0x35, 0x03, 0x0A]);
    }

    #[test]
    fn encode_presets() {
        let sp = Setpoints {
            voltage: Millivolts(3300),
            current: Milliamps(100),
            ovp: Millivolts(4000),
            ocp: Milliamps(200),
        };
        let i3 = PresetIndex::new(3).unwrap();
        let f = Request::SavePreset { index: i3, setpoints: sp }.to_frame(0).unwrap();
        assert_eq!(&f.payload()[..2], &[0x43, 0x00]);
        let f = Request::RecallPreset { index: i3, setpoints: sp }.to_frame(0).unwrap();
        assert_eq!(&f.payload()[..2], &[0x83, 0x00]);
        let i0 = PresetIndex::new(0).unwrap();
        assert_eq!(Request::SavePreset { index: i0, setpoints: sp }.to_frame(0), Err(ProtoError::PresetZeroReadOnly));
        assert_eq!(Request::RecallPreset { index: i0, setpoints: sp }.to_frame(0), Err(ProtoError::PresetZeroReadOnly));
        assert!(PresetIndex::new(10).is_err());
        assert_eq!(Request::Preset(i3).to_frame(0).unwrap().payload(), &[0x03]);
        assert_eq!(Request::ActiveSet.to_frame(0).unwrap().payload(), &[0x80]);
    }

    #[test]
    fn read_flags_never_collide_with_write_flags() {
        for i in 0..=9 {
            let f = Request::Preset(PresetIndex::new(i).unwrap()).to_frame(0).unwrap();
            assert_eq!(f.payload().len(), 1);
            assert_eq!(f.payload()[0] & 0x60, 0);
        }
        assert!(!Request::ActiveSet.is_write());
        assert!(!Request::SystemSettings.is_write());
    }

    #[test]
    fn mismatched_func_rejected() {
        let r = Request::SystemSettings.parse_response(&report("FA 30 00 01 00 F2 84"));
        assert!(matches!(r, Err(ProtoError::FuncMismatch { .. })));
    }

    #[test]
    fn wrong_lengths_rejected() {
        let f = Frame::new(Func::BasicInfo, 0, &[1, 2, 3]).unwrap();
        assert!(matches!(Request::BasicInfo.parse_response(&f), Err(ProtoError::PayloadLen { .. })));
        let f = Frame::new(Func::BasicSet, 0, &[1, 2]).unwrap();
        assert!(matches!(
            Request::ApplyOutput(OutputSet {
                preset: PresetIndex::new(0).unwrap(),
                output_on: false,
                setpoints: Setpoints {
                    voltage: Millivolts(0),
                    current: Milliamps(0),
                    ovp: Millivolts(0),
                    ocp: Milliamps(0)
                },
            })
            .parse_response(&f),
            Err(ProtoError::PayloadLen { .. })
        ));
    }
}
