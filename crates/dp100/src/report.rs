//! JSON-friendly views (engineering units) of the protocol types.

use dp100_proto::{BasicInfo, DeviceInfo, SetpointRecord, SystemSettings};
use schemars::JsonSchema;
use serde::Serialize;

use crate::limits::Limits;

fn r4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

/// Identity of one DP100.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DeviceInfoReport {
    /// USB iSerial (use it to select a device).
    pub usb_serial: Option<String>,
    pub model: String,
    pub hardware_version: String,
    pub firmware_version: String,
    pub bootloader_version: String,
    /// "APP" or "BOOTLOADER".
    pub run_mode: String,
    /// 12-byte device serial from DEVICE_INFO, hex.
    pub device_serial: String,
    /// Short form shown by the vendor software.
    pub device_serial_short: String,
    pub firmware_date: String,
}

impl DeviceInfoReport {
    pub fn new(usb_serial: Option<String>, d: &DeviceInfo) -> Self {
        Self {
            usb_serial,
            model: d.model.clone(),
            hardware_version: d.hw_version_str(),
            firmware_version: d.fw_version_str(),
            bootloader_version: d.boot_version_str(),
            run_mode: if d.in_application() { "APP" } else { "BOOTLOADER" }.into(),
            device_serial: d.serial_hex(),
            device_serial_short: d.serial_short(),
            firmware_date: d.date_str(),
        }
    }
}

/// Setpoints of the active set. `output_on` is the firmware's state byte,
/// which can disagree with the real output (see `StatusReport::output_on`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, JsonSchema)]
pub struct SetpointReport {
    pub preset: u8,
    pub output_on: bool,
    pub voltage_v: f64,
    pub current_limit_a: f64,
    pub ovp_v: f64,
    pub ocp_a: f64,
}

impl From<&SetpointRecord> for SetpointReport {
    fn from(s: &SetpointRecord) -> Self {
        Self {
            preset: s.index,
            output_on: s.output_on,
            voltage_v: s.setpoints.voltage.volts(),
            current_limit_a: s.setpoints.current.amps(),
            ovp_v: s.setpoints.ovp.volts(),
            ocp_a: s.setpoints.ocp.amps(),
        }
    }
}

/// Live measurements.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TelemetryReport {
    pub vin_v: f64,
    pub vout_v: f64,
    pub iout_a: f64,
    pub power_w: f64,
    /// Firmware-reported maximum Vout at the current Vin.
    pub vo_max_v: f64,
    /// Ceiling enforced by this server: min(vo_max, Vin − dropout).
    pub vout_ceiling_v: f64,
    pub temperature1_c: f64,
    pub temperature2_c: f64,
    pub rail_5v_v: f64,
    /// "CV", "CC" (unverified code), "OFF" or "UNKNOWN".
    pub mode: String,
    pub mode_raw: u8,
    /// "NORMAL" or the tripped protection (codes other than 0 unverified).
    pub protection: String,
    pub protection_raw: u8,
    pub protection_tripped: bool,
}

impl TelemetryReport {
    pub fn new(t: &BasicInfo, limits: &Limits) -> Self {
        Self {
            vin_v: t.vin.volts(),
            vout_v: t.vout.volts(),
            iout_a: t.iout.amps(),
            power_w: r4(t.power_mw() / 1000.0),
            vo_max_v: t.vo_max.volts(),
            vout_ceiling_v: limits.vout_ceiling(t).volts(),
            temperature1_c: t.temp1.celsius(),
            temperature2_c: t.temp2.celsius(),
            rail_5v_v: t.dc_5v.volts(),
            mode: t.out_mode().as_str().into(),
            mode_raw: t.out_mode_raw,
            protection: t.work_state().as_str().into(),
            protection_raw: t.work_state_raw,
            protection_tripped: t.work_state().is_tripped(),
        }
    }
}

/// Telemetry plus active setpoints.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StatusReport {
    /// Real output state, from telemetry (out_mode ≠ OFF).
    pub output_on: bool,
    /// True when the active set's state byte disagrees with the real output
    /// state (the firmware can store state=0 while the output stays on).
    pub output_state_flag_mismatch: bool,
    pub telemetry: TelemetryReport,
    pub setpoint: SetpointReport,
}

impl StatusReport {
    pub fn new(t: &BasicInfo, s: &SetpointRecord, limits: &Limits) -> Self {
        let on = crate::output_active(t);
        Self {
            output_on: on,
            output_state_flag_mismatch: on != s.output_on,
            telemetry: TelemetryReport::new(t, limits),
            setpoint: s.into(),
        }
    }
}

/// A stored preset slot.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, JsonSchema)]
pub struct PresetReport {
    pub preset: u8,
    /// This slot is the active set's preset number.
    pub active: bool,
    pub voltage_v: f64,
    pub current_limit_a: f64,
    pub ovp_v: f64,
    pub ocp_a: f64,
}

impl PresetReport {
    pub fn new(p: &SetpointRecord, active_index: u8) -> Self {
        Self {
            preset: p.index,
            active: p.index == active_index,
            voltage_v: p.setpoints.voltage.volts(),
            current_limit_a: p.setpoints.current.amps(),
            ovp_v: p.setpoints.ovp.volts(),
            ocp_a: p.setpoints.ocp.amps(),
        }
    }
}

/// System settings in engineering units.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, JsonSchema)]
pub struct SystemSettingsReport {
    pub otp_c: u16,
    pub opp_w: f64,
    pub backlight: u8,
    pub volume: u8,
    pub reverse_protection: bool,
    pub output_on_at_power_up: bool,
}

impl From<&SystemSettings> for SystemSettingsReport {
    fn from(s: &SystemSettings) -> Self {
        Self {
            otp_c: s.otp_c,
            opp_w: s.opp.watts(),
            backlight: s.backlight,
            volume: s.volume,
            reverse_protection: s.reverse_protection,
            output_on_at_power_up: s.auto_output,
        }
    }
}

/// Configured safety limits.
#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
pub struct LimitsReport {
    pub max_voltage_v: f64,
    pub max_current_a: f64,
    pub vin_dropout_v: f64,
}

impl From<&Limits> for LimitsReport {
    fn from(l: &Limits) -> Self {
        Self {
            max_voltage_v: l.max_voltage.volts(),
            max_current_a: l.max_current.amps(),
            vin_dropout_v: l.vin_dropout.volts(),
        }
    }
}
