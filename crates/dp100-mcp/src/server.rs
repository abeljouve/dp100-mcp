//! MCP tool surface.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use dp100::{
    DeviceManager, Dp100, Limits, OutputChange, WriteOutcome,
    measure::{MeasureSummary, RampReport, Sample, ramp_points, write_csv},
    proto::{DeciWatts, Milliamps, Millivolts, PresetIndex, Setpoints},
    report::{
        DeviceInfoReport, LimitsReport, PresetReport, SetpointReport, StatusReport, SystemSettingsReport,
        TelemetryReport,
    },
};
use rmcp::{
    Json, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

type ToolResult<T> = Result<Json<T>, String>;

/// Where `measure` may write CSV files.
#[derive(Debug, Clone)]
pub enum CsvPolicy {
    /// Any absolute path (stdio mode, the client runs as the local user).
    Anywhere,
    /// Only inside this directory.
    Under(PathBuf),
    /// CSV output disabled.
    Disabled,
}

/// Shared server state; cloned per MCP session.
#[derive(Clone)]
pub struct Dp100Server {
    devices: Arc<DeviceManager>,
    limits: Limits,
    csv: CsvPolicy,
    tool_router: ToolRouter<Self>,
}

impl std::fmt::Debug for Dp100Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dp100Server").field("limits", &self.limits).field("csv", &self.csv).finish()
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn mv(v: f64, what: &str) -> Result<Millivolts, String> {
    Millivolts::from_volts(v).map_err(|e| format!("{what}: {e}"))
}

fn ma(a: f64, what: &str) -> Result<Milliamps, String> {
    Milliamps::from_amps(a).map_err(|e| format!("{what}: {e}"))
}

fn preset_index(i: u8) -> Result<PresetIndex, String> {
    PresetIndex::new(i).and_then(PresetIndex::writable).map_err(err)
}

// ---------- parameters ----------

#[derive(Debug, Deserialize, JsonSchema, Default)]
pub struct DeviceParam {
    /// USB serial of the DP100 (from list_devices). Optional when one unit is connected.
    pub serial: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetOutputParams {
    /// USB serial of the DP100. Optional when one unit is connected.
    pub serial: Option<String>,
    /// New output voltage in volts. Omit to keep the current setpoint.
    pub voltage_v: Option<f64>,
    /// New current limit in amps. Omit to keep the current setpoint.
    pub current_limit_a: Option<f64>,
    /// true = switch the output ON, false = OFF. Omit to leave the ON/OFF state untouched.
    pub enable: Option<bool>,
    /// Wait before reading telemetry back (ms, default 250; the output rises in 100–200 ms).
    pub settle_ms: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PowerCycleParams {
    pub serial: Option<String>,
    /// How long the output stays OFF, in ms (1–600000).
    pub off_ms: u64,
    /// Wait after switching back ON before measuring, in ms (default 300).
    pub settle_ms: Option<u64>,
    /// Allow running when the output is currently OFF (it will end up ON). Default false.
    #[serde(default)]
    pub allow_from_off: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MeasureParams {
    pub serial: Option<String>,
    /// Measurement window in ms (1–3600000).
    pub duration_ms: u64,
    /// Sampling rate in Hz (default 10, max 200).
    pub rate_hz: Option<f64>,
    /// Write all samples to this CSV file (absolute path).
    pub csv_path: Option<String>,
    /// Return the raw samples in the result (max 2000 kept). Default false.
    #[serde(default)]
    pub include_samples: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RampParams {
    pub serial: Option<String>,
    pub start_v: f64,
    pub end_v: f64,
    /// Step size in volts (> 0); the sign follows start → end.
    pub step_v: f64,
    /// Time spent on each plateau before measuring, in ms (≤ 600000).
    pub dwell_ms: u64,
    /// Applied with the first step. Omit to leave the ON/OFF state untouched.
    pub enable: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetPresetParams {
    pub serial: Option<String>,
    /// Preset slot 1–9 (slot 0 is read-only).
    pub index: u8,
    pub voltage_v: f64,
    pub current_limit_a: f64,
    /// Over-voltage protection (V). Omit to keep the slot's current value.
    pub ovp_v: Option<f64>,
    /// Over-current protection (A). Omit to keep the slot's current value.
    pub ocp_a: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApplyPresetParams {
    pub serial: Option<String>,
    /// Preset slot 1–9.
    pub index: u8,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetSettingsParams {
    pub serial: Option<String>,
    /// Over-temperature protection, 50–80 °C.
    pub otp_c: Option<u16>,
    /// Over-power protection, up to 105 W.
    pub opp_w: Option<f64>,
    /// Backlight level 0–4.
    pub backlight: Option<u8>,
    /// Beeper volume 0–4.
    pub volume: Option<u8>,
    /// Reverse-polarity protection.
    pub reverse_protection: Option<bool>,
    /// Switch the output ON automatically at power-up.
    pub output_on_at_power_up: Option<bool>,
    /// Over-voltage protection of the active set (V, ≤ 30.5).
    pub ovp_v: Option<f64>,
    /// Over-current protection of the active set (A, ≤ 5.05).
    pub ocp_a: Option<f64>,
}

// ---------- results ----------

#[derive(Debug, Serialize, JsonSchema)]
pub struct DeviceEntry {
    pub usb_serial: Option<String>,
    pub path: String,
    pub info: Option<DeviceInfoReport>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DeviceList {
    pub devices: Vec<DeviceEntry>,
    pub limits: LimitsReport,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct StatusResult {
    pub serial: String,
    #[serde(flatten)]
    pub status: StatusReport,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct WriteResult {
    pub serial: String,
    /// Real output state after the write, from telemetry.
    pub output_on: bool,
    /// Setpoints before the write.
    pub before: SetpointReport,
    /// Setpoints read back from the device after the write.
    pub applied: SetpointReport,
    /// Telemetry read after `settle_ms`.
    pub telemetry: TelemetryReport,
    /// Differences between the request and the read-back, if any.
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PowerCycleResult {
    pub serial: String,
    pub off_ms: u64,
    pub before: StatusReport,
    pub after: StatusReport,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct MeasureResult {
    pub serial: String,
    pub summary: MeasureSummary,
    pub csv_path: Option<String>,
    pub samples: Option<Vec<Sample>>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RampResult {
    pub serial: String,
    #[serde(flatten)]
    pub report: RampReport,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PresetsResult {
    pub serial: String,
    /// Index of the active set.
    pub active_preset: u8,
    pub presets: Vec<PresetReport>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PresetResult {
    pub serial: String,
    /// Slot contents read back after writing.
    pub preset: PresetReport,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ApplyPresetResult {
    pub serial: String,
    /// Real output state after the recall, from telemetry.
    pub output_on: bool,
    pub before: SetpointReport,
    pub applied: SetpointReport,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ProtectionsReport {
    /// OVP of the active set (V).
    pub ovp_v: f64,
    /// OCP of the active set (A).
    pub ocp_a: f64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SettingsResult {
    pub serial: String,
    pub system: SystemSettingsReport,
    pub protections: ProtectionsReport,
    /// Host-side limits enforced by this server.
    pub server_limits: LimitsReport,
    pub warnings: Vec<String>,
}

const INSTRUCTIONS: &str = "\
Controls an ALIENTEK DP100 bench power supply (0–30 V, 0–5 A, 100 W).
Safety rules enforced by the server:
- Requests above the configured limits, above OVP/OCP, or above min(vo_max, Vin − dropout) are REFUSED (never clamped).
- set_output never changes the ON/OFF state unless `enable` is given. output_off is never refused.
- Every write is followed by a read-back; results report what the device actually applied, plus warnings on mismatch.
- `output_on` in results is the REAL state from telemetry; the firmware's own state byte can be wrong.
- After a write, Vout is checked against the setpoint (CV); a SAFETY error means the device did not obey: tell the user.
Before enabling an output, make sure the user knows what is connected to it.
`serial` selects the unit (see list_devices); it can be omitted when only one is connected.";

#[tool_router]
impl Dp100Server {
    pub fn new(devices: Arc<DeviceManager>, limits: Limits, csv: CsvPolicy) -> Self {
        Self { devices, limits, csv, tool_router: Self::tool_router() }
    }

    fn device(&self, serial: Option<&str>) -> Result<(String, Dp100), String> {
        self.devices.get(serial).map_err(err)
    }

    async fn write_result(
        &self,
        serial: String,
        d: &Dp100,
        w: WriteOutcome,
        settle_ms: u64,
    ) -> ToolResult<WriteResult> {
        tokio::time::sleep(Duration::from_millis(settle_ms)).await;
        let t = d.basic_info().await.map_err(err)?;
        Ok(Json(WriteResult {
            serial,
            output_on: dp100::output_active(&t),
            before: (&w.before).into(),
            applied: (&w.after).into(),
            telemetry: TelemetryReport::new(&t, &self.limits),
            warnings: w.warnings,
        }))
    }

    fn csv_target(&self, p: &str) -> Result<PathBuf, String> {
        let path = Path::new(p);
        match &self.csv {
            CsvPolicy::Disabled => Err("CSV output is disabled on this server (start it with --csv-dir)".into()),
            CsvPolicy::Anywhere if path.is_absolute() => Ok(path.to_path_buf()),
            CsvPolicy::Anywhere => Err("csv_path must be an absolute path".into()),
            CsvPolicy::Under(dir) => {
                let rel = path.strip_prefix(dir).unwrap_or(path);
                if rel.is_absolute() || rel.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
                    return Err(format!("csv_path must be a plain file name or a path under {}", dir.display()));
                }
                Ok(dir.join(rel))
            }
        }
    }

    #[tool(
        description = "List connected DP100 units with serial number, firmware and hardware versions, and the server's safety limits. Read-only."
    )]
    async fn list_devices(&self) -> ToolResult<DeviceList> {
        let mut devices = Vec::new();
        for desc in self.devices.list().map_err(err)? {
            let info = match self.devices.get(Some(&desc.key())) {
                Ok((_, d)) => d.device_info().await.map_err(err),
                Err(e) => Err(err(e)),
            };
            devices.push(DeviceEntry {
                usb_serial: desc.usb_serial.clone(),
                path: desc.path,
                error: info.as_ref().err().cloned(),
                info: info.ok().map(|i| DeviceInfoReport::new(desc.usb_serial, &i)),
            });
        }
        Ok(Json(DeviceList { devices, limits: (&self.limits).into() }))
    }

    #[tool(
        description = "Read Vin, Vout, Iout, power, temperatures, output ON/OFF, CV/CC mode, tripped protection, and the active setpoints (V, I limit, OVP, OCP). Read-only."
    )]
    async fn get_status(&self, Parameters(p): Parameters<DeviceParam>) -> ToolResult<StatusResult> {
        let (serial, d) = self.device(p.serial.as_deref())?;
        let (t, s) = d.status().await.map_err(err)?;
        Ok(Json(StatusResult { serial, status: StatusReport::new(&t, &s, &self.limits) }))
    }

    #[tool(
        description = "Change the output voltage and/or current limit. The ON/OFF state changes ONLY if `enable` is given. Refused (not clamped) if above the server limits, the active OVP/OCP, or min(vo_max, Vin − dropout). Returns the setpoints read back from the device and fresh telemetry."
    )]
    async fn set_output(&self, Parameters(p): Parameters<SetOutputParams>) -> ToolResult<WriteResult> {
        if p.voltage_v.is_none() && p.current_limit_a.is_none() && p.enable.is_none() {
            return Err("nothing to change: give voltage_v, current_limit_a and/or enable".into());
        }
        let change = OutputChange {
            voltage: p.voltage_v.map(|v| mv(v, "voltage_v")).transpose()?,
            current: p.current_limit_a.map(|a| ma(a, "current_limit_a")).transpose()?,
            enable: p.enable,
        };
        let (serial, d) = self.device(p.serial.as_deref())?;
        let w = d.set_output(change, &self.limits).await.map_err(err)?;
        self.write_result(serial, &d, w, p.settle_ms.unwrap_or(250).min(10_000)).await
    }

    #[tool(
        description = "Switch the output ON with the current setpoints (checked against the safety limits and Vin first). Returns the read-back state and telemetry."
    )]
    async fn output_on(&self, Parameters(p): Parameters<DeviceParam>) -> ToolResult<WriteResult> {
        let (serial, d) = self.device(p.serial.as_deref())?;
        let w =
            d.set_output(OutputChange { enable: Some(true), ..Default::default() }, &self.limits).await.map_err(err)?;
        self.write_result(serial, &d, w, 250).await
    }

    #[tool(
        description = "Switch the output OFF and verify via telemetry that it really is off (re-sent once if needed); fails loudly with a SAFETY error if it is not. Never refused by the limits."
    )]
    async fn output_off(&self, Parameters(p): Parameters<DeviceParam>) -> ToolResult<WriteResult> {
        let (serial, d) = self.device(p.serial.as_deref())?;
        let w = d
            .set_output(OutputChange { enable: Some(false), ..Default::default() }, &self.limits)
            .await
            .map_err(err)?;
        self.write_result(serial, &d, w, 250).await
    }

    #[tool(
        description = "Power-cycle the DUT: switch the output OFF for `off_ms`, switch it back ON with the current setpoints, wait `settle_ms`, and return measurements before and after. Refuses to start if the output is OFF unless allow_from_off=true. If re-enabling is refused, the output is left OFF."
    )]
    async fn power_cycle(&self, Parameters(p): Parameters<PowerCycleParams>) -> ToolResult<PowerCycleResult> {
        if p.off_ms == 0 || p.off_ms > 600_000 {
            return Err("off_ms must be in 1..=600000".into());
        }
        let (serial, d) = self.device(p.serial.as_deref())?;
        let r = d
            .power_cycle(
                Duration::from_millis(p.off_ms),
                Duration::from_millis(p.settle_ms.unwrap_or(300).min(60_000)),
                p.allow_from_off,
                &self.limits,
            )
            .await
            .map_err(err)?;
        Ok(Json(PowerCycleResult {
            serial,
            off_ms: p.off_ms,
            before: StatusReport::new(&r.before.0, &r.before.1, &self.limits),
            after: StatusReport::new(&r.after.0, &r.after.1, &self.limits),
            warnings: r.warnings,
        }))
    }

    #[tool(
        description = "Sample Vout/Iout/P for `duration_ms` at `rate_hz` (host-timed, max 200 Hz). Returns min/max/mean, energy (mWh) and charge (mAh); optionally writes a CSV. Read-only."
    )]
    async fn measure(&self, Parameters(p): Parameters<MeasureParams>) -> ToolResult<MeasureResult> {
        let csv = p.csv_path.as_deref().map(|c| self.csv_target(c)).transpose()?;
        let (serial, d) = self.device(p.serial.as_deref())?;
        let (samples, summary) =
            d.measure(Duration::from_millis(p.duration_ms), p.rate_hz.unwrap_or(10.0)).await.map_err(err)?;
        if let Some(path) = &csv {
            write_csv(path, &samples).map_err(|e| format!("writing {}: {e}", path.display()))?;
        }
        Ok(Json(MeasureResult {
            serial,
            summary,
            csv_path: csv.map(|p| p.display().to_string()),
            samples: p.include_samples.then(|| samples.into_iter().take(2000).collect()),
        }))
    }

    #[tool(
        description = "Step the output voltage from start_v to end_v by step_v, waiting dwell_ms on each plateau and measuring at its end. All points are validated against the limits before the first write. Stops early if a protection trips. `enable` applies with the first step; omit it to keep the ON/OFF state."
    )]
    async fn ramp_voltage(&self, Parameters(p): Parameters<RampParams>) -> ToolResult<RampResult> {
        if p.dwell_ms > 600_000 {
            return Err("dwell_ms must be ≤ 600000".into());
        }
        let points =
            ramp_points(mv(p.start_v, "start_v")?, mv(p.end_v, "end_v")?, mv(p.step_v, "step_v")?).map_err(err)?;
        let (serial, d) = self.device(p.serial.as_deref())?;
        let report = d.ramp(&points, Duration::from_millis(p.dwell_ms), p.enable, &self.limits).await.map_err(err)?;
        Ok(Json(RampResult { serial, report }))
    }

    #[tool(description = "Read the 10 presets (P0–P9) and the active preset index. Read-only.")]
    async fn get_presets(&self, Parameters(p): Parameters<DeviceParam>) -> ToolResult<PresetsResult> {
        let (serial, d) = self.device(p.serial.as_deref())?;
        let active = d.active_set().await.map_err(err)?;
        let presets = d.presets().await.map_err(err)?.iter().map(|p| PresetReport::new(p, active.index)).collect();
        Ok(Json(PresetsResult { serial, active_preset: active.index, presets }))
    }

    #[tool(
        description = "Store voltage/current (and optionally OVP/OCP) in preset slot 1–9. Does not change the live output. Checked against the server limits. Returns the slot read back."
    )]
    async fn set_preset(&self, Parameters(p): Parameters<SetPresetParams>) -> ToolResult<PresetResult> {
        let index = preset_index(p.index)?;
        let (serial, d) = self.device(p.serial.as_deref())?;
        let current = d.preset(index).await.map_err(err)?;
        let sp = Setpoints {
            voltage: mv(p.voltage_v, "voltage_v")?,
            current: ma(p.current_limit_a, "current_limit_a")?,
            ovp: p.ovp_v.map(|v| mv(v, "ovp_v")).transpose()?.unwrap_or(current.setpoints.ovp),
            ocp: p.ocp_a.map(|a| ma(a, "ocp_a")).transpose()?.unwrap_or(current.setpoints.ocp),
        };
        let got = d.save_preset(index, sp, &self.limits).await.map_err(err)?;
        let active = d.active_set().await.map_err(err)?;
        Ok(Json(PresetResult { serial, preset: PresetReport::new(&got, active.index) }))
    }

    #[tool(
        description = "Make preset 1–9 the active set (its V/I/OVP/OCP become the live setpoints). Checked against the limits and Vin first. Never switches the output ON: if the device did, it is switched back OFF and an error is returned."
    )]
    async fn apply_preset(&self, Parameters(p): Parameters<ApplyPresetParams>) -> ToolResult<ApplyPresetResult> {
        let index = preset_index(p.index)?;
        let (serial, d) = self.device(p.serial.as_deref())?;
        let w = d.recall_preset(index, &self.limits).await.map_err(err)?;
        Ok(Json(ApplyPresetResult {
            serial,
            output_on: dp100::output_active(&w.telemetry),
            before: (&w.before).into(),
            applied: (&w.after).into(),
            warnings: w.warnings,
        }))
    }

    #[tool(
        description = "Read system settings (OTP, OPP, backlight, volume, reverse-polarity protection, output-at-power-up), the OVP/OCP of the active set, and the server's limits. Read-only."
    )]
    async fn get_settings(&self, Parameters(p): Parameters<DeviceParam>) -> ToolResult<SettingsResult> {
        let (serial, d) = self.device(p.serial.as_deref())?;
        let sys = d.system_settings().await.map_err(err)?;
        let active = d.active_set().await.map_err(err)?;
        Ok(Json(SettingsResult {
            serial,
            system: (&sys).into(),
            protections: ProtectionsReport { ovp_v: active.setpoints.ovp.volts(), ocp_a: active.setpoints.ocp.amps() },
            server_limits: (&self.limits).into(),
            warnings: vec![],
        }))
    }

    #[tool(
        description = "Change system settings (otp_c 50–80, opp_w ≤ 105, backlight 0–4, volume 0–4, reverse_protection, output_on_at_power_up) and/or the active set's OVP/OCP. Omitted fields are unchanged. Returns the values read back."
    )]
    async fn set_settings(&self, Parameters(p): Parameters<SetSettingsParams>) -> ToolResult<SettingsResult> {
        let (serial, d) = self.device(p.serial.as_deref())?;
        let mut warnings = Vec::new();
        let sys_change = p.otp_c.is_some()
            || p.opp_w.is_some()
            || p.backlight.is_some()
            || p.volume.is_some()
            || p.reverse_protection.is_some()
            || p.output_on_at_power_up.is_some();
        if !sys_change && p.ovp_v.is_none() && p.ocp_a.is_none() {
            return Err("nothing to change".into());
        }
        if p.ovp_v.is_some() || p.ocp_a.is_some() {
            let ovp = p.ovp_v.map(|v| mv(v, "ovp_v")).transpose()?;
            let ocp = p.ocp_a.map(|a| ma(a, "ocp_a")).transpose()?;
            let w = d.set_protections(ovp, ocp, &self.limits).await.map_err(err)?;
            warnings.extend(w.warnings);
        }
        if sys_change {
            let mut s = d.system_settings().await.map_err(err)?;
            if let Some(v) = p.otp_c {
                s.otp_c = v;
            }
            if let Some(w) = p.opp_w {
                s.opp = DeciWatts::from_watts(w).map_err(|e| format!("opp_w: {e}"))?;
            }
            if let Some(v) = p.backlight {
                s.backlight = v;
            }
            if let Some(v) = p.volume {
                s.volume = v;
            }
            if let Some(v) = p.reverse_protection {
                s.reverse_protection = v;
            }
            if let Some(v) = p.output_on_at_power_up {
                s.auto_output = v;
            }
            let (_, w) = d.write_system_settings(s).await.map_err(err)?;
            warnings.extend(w);
        }
        let sys = d.system_settings().await.map_err(err)?;
        let active = d.active_set().await.map_err(err)?;
        Ok(Json(SettingsResult {
            serial,
            system: (&sys).into(),
            protections: ProtectionsReport { ovp_v: active.setpoints.ovp.volts(), ocp_a: active.setpoints.ocp.amps() },
            server_limits: (&self.limits).into(),
            warnings,
        }))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Dp100Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("dp100-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dp100::{ActorOptions, sim::SimDevice};

    fn server(csv: CsvPolicy) -> Dp100Server {
        let m = DeviceManager::sim(vec![SimDevice::new("SIM0001")], ActorOptions::default());
        Dp100Server::new(Arc::new(m), Limits::default(), csv)
    }

    #[test]
    fn csv_policy() {
        let s = server(CsvPolicy::Anywhere);
        assert!(s.csv_target("/tmp/a.csv").is_ok());
        assert!(s.csv_target("a.csv").is_err());
        let s = server(CsvPolicy::Under(PathBuf::from("/var/lib/dp100-mcp")));
        assert_eq!(s.csv_target("run1.csv").unwrap(), PathBuf::from("/var/lib/dp100-mcp/run1.csv"));
        assert_eq!(s.csv_target("/var/lib/dp100-mcp/x/y.csv").unwrap(), PathBuf::from("/var/lib/dp100-mcp/x/y.csv"));
        assert!(s.csv_target("../etc/passwd").is_err());
        assert!(s.csv_target("/etc/passwd").is_err());
        assert!(server(CsvPolicy::Disabled).csv_target("/tmp/a.csv").is_err());
    }

    #[test]
    fn exposes_all_tools() {
        let mut names: Vec<String> =
            server(CsvPolicy::Disabled).tool_router.list_all().into_iter().map(|t| t.name.to_string()).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "apply_preset",
                "get_presets",
                "get_settings",
                "get_status",
                "list_devices",
                "measure",
                "output_off",
                "output_on",
                "power_cycle",
                "ramp_voltage",
                "set_output",
                "set_preset",
                "set_settings"
            ]
        );
    }

    #[tokio::test]
    async fn set_output_requires_a_change() {
        let s = server(CsvPolicy::Disabled);
        let r = s
            .set_output(Parameters(SetOutputParams {
                serial: None,
                voltage_v: None,
                current_limit_a: None,
                enable: None,
                settle_ms: None,
            }))
            .await;
        assert!(r.is_err());
    }
}
