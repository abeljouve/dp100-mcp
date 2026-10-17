//! Multi-request operations: every write is preceded by safety checks and
//! followed by a read-back of what the device actually applied.

use std::time::Duration;

use dp100_proto::{
    BasicInfo, DeviceInfo, Milliamps, Millivolts, OutMode, OutputSet, PresetIndex, Request, Response, SetpointRecord,
    Setpoints, SystemSettings, device_limits,
};
use tracing::{info, warn};

use crate::{Dp100, Error, Result, limits::Limits};

/// Requested change of the live output. `None` fields are left untouched;
/// in particular `enable: None` never changes the ON/OFF state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutputChange {
    pub voltage: Option<Millivolts>,
    pub current: Option<Milliamps>,
    pub enable: Option<bool>,
}

/// Result of a write: state before, state read back after, telemetry once
/// the output stage settled, and any difference between request and device.
#[derive(Debug, Clone, PartialEq)]
pub struct WriteOutcome {
    pub before: SetpointRecord,
    pub after: SetpointRecord,
    pub telemetry: BasicInfo,
    pub warnings: Vec<String>,
}

/// Max time for the output stage to reach the requested ON/OFF state.
/// Observed on hardware: ON and OFF within 100–200 ms (fw 1.5).
const OUTPUT_SETTLE_TIMEOUT: Duration = Duration::from_millis(800);
/// Max time for Vout to reach the setpoint once the output is on
/// (observed: 5 → 12 V with a fan load in < 0.5 s).
const VOUT_SETTLE_TIMEOUT: Duration = Duration::from_millis(1500);
const OUTPUT_POLL_PERIOD: Duration = Duration::from_millis(20);

/// Real output state, from telemetry. The `state` byte of the active set is
/// what was last written, not what the output does: fw 1.5 can acknowledge
/// and store a write without applying it (docs/PROTOCOL.md §5.3.1).
pub fn output_active(t: &BasicInfo) -> bool {
    t.out_mode() != OutMode::Off
}

/// Accepted |Vout − Vset| in CV mode.
fn vout_tolerance(v: Millivolts) -> u16 {
    150u16.max(v.0 / 50)
}

/// Outcome of checking telemetry against a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Ok,
    StillOn,
    StillOff,
    VoutHigh,
    VoutLow,
}

/// Result of [`Dp100::power_cycle`].
#[derive(Debug, Clone)]
pub struct PowerCycleOutcome {
    pub before: (BasicInfo, SetpointRecord),
    pub after: (BasicInfo, SetpointRecord),
    pub warnings: Vec<String>,
}

fn unexpected(r: Response) -> Error {
    Error::UnexpectedResponse(format!("{r:?}"))
}

fn compare(requested: &OutputSet, got: &SetpointRecord) -> Vec<String> {
    let mut w = Vec::new();
    let r = &requested.setpoints;
    let g = &got.setpoints;
    if got.output_on != requested.output_on {
        w.push(format!(
            "output state read back as {} instead of {}",
            on_off(got.output_on),
            on_off(requested.output_on)
        ));
    }
    if g.voltage != r.voltage {
        w.push(format!("voltage applied {} V instead of {} V", g.voltage.volts(), r.voltage.volts()));
    }
    if g.current != r.current {
        w.push(format!("current limit applied {} A instead of {} A", g.current.amps(), r.current.amps()));
    }
    if g.ovp != r.ovp || g.ocp != r.ocp {
        w.push(format!(
            "protections read back as OVP {} V / OCP {} A instead of {} V / {} A",
            g.ovp.volts(),
            g.ocp.amps(),
            r.ovp.volts(),
            r.ocp.amps()
        ));
    }
    w
}

fn on_off(b: bool) -> &'static str {
    if b { "ON" } else { "OFF" }
}

impl Dp100 {
    pub async fn device_info(&self) -> Result<DeviceInfo> {
        match self.request(Request::DeviceInfo).await? {
            Response::DeviceInfo(d) => Ok(d),
            r => Err(unexpected(r)),
        }
    }

    /// Read device info and refuse to continue if the unit sits in its bootloader.
    pub async fn ensure_application(&self) -> Result<DeviceInfo> {
        let d = self.device_info().await?;
        if !d.in_application() {
            return Err(Error::Bootloader(d.run_area));
        }
        Ok(d)
    }

    pub async fn basic_info(&self) -> Result<BasicInfo> {
        match self.request(Request::BasicInfo).await? {
            Response::BasicInfo(b) => Ok(b),
            r => Err(unexpected(r)),
        }
    }

    pub async fn active_set(&self) -> Result<SetpointRecord> {
        match self.request(Request::ActiveSet).await? {
            Response::Setpoints(s) => Ok(s),
            r => Err(unexpected(r)),
        }
    }

    pub async fn preset(&self, index: PresetIndex) -> Result<SetpointRecord> {
        match self.request(Request::Preset(index)).await? {
            Response::Setpoints(s) => Ok(s),
            r => Err(unexpected(r)),
        }
    }

    pub async fn presets(&self) -> Result<Vec<SetpointRecord>> {
        let mut out = Vec::with_capacity(10);
        for i in 0..=9 {
            out.push(self.preset(PresetIndex::new(i)?).await?);
        }
        Ok(out)
    }

    pub async fn system_settings(&self) -> Result<SystemSettings> {
        match self.request(Request::SystemSettings).await? {
            Response::SystemSettings(s) => Ok(s),
            r => Err(unexpected(r)),
        }
    }

    /// Telemetry and active setpoints.
    pub async fn status(&self) -> Result<(BasicInfo, SetpointRecord)> {
        let t = self.basic_info().await?;
        let s = self.active_set().await?;
        Ok((t, s))
    }

    async fn write(&self, req: Request) -> Result<()> {
        match self.request(req).await? {
            Response::Ack => Ok(()),
            r => Err(unexpected(r)),
        }
    }

    /// Poll telemetry until the output reaches `on`, or the timeout expires.
    async fn wait_output(&self, on: bool) -> Result<(bool, BasicInfo)> {
        let deadline = tokio::time::Instant::now() + OUTPUT_SETTLE_TIMEOUT;
        loop {
            let t = self.basic_info().await?;
            if output_active(&t) == on || tokio::time::Instant::now() >= deadline {
                return Ok((output_active(&t) == on, t));
            }
            tokio::time::sleep(OUTPUT_POLL_PERIOD).await;
        }
    }

    /// Check that the real output matches `set`: ON/OFF state, and when ON,
    /// Vout within tolerance of the setpoint (or current-limited).
    async fn verify(&self, set: &OutputSet) -> Result<(Verdict, BasicInfo)> {
        let (reached, t) = self.wait_output(set.output_on).await?;
        if !reached {
            return Ok((if set.output_on { Verdict::StillOff } else { Verdict::StillOn }, t));
        }
        if !set.output_on {
            return Ok((Verdict::Ok, t));
        }
        let v = set.setpoints.voltage;
        let tol = vout_tolerance(v);
        let deadline = tokio::time::Instant::now() + VOUT_SETTLE_TIMEOUT;
        loop {
            let t = self.basic_info().await?;
            let cc = t.out_mode() == OutMode::Cc;
            if cc || t.vout.0.abs_diff(v.0) <= tol {
                return Ok((Verdict::Ok, t));
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok((if t.vout.0 > v.0 { Verdict::VoutHigh } else { Verdict::VoutLow }, t));
            }
            tokio::time::sleep(OUTPUT_POLL_PERIOD).await;
        }
    }

    /// Write `set`, read it back, and check the real output against it.
    ///
    /// fw 1.5 can acknowledge a write and store it without applying it (seen
    /// when a 0x35 read followed within ~5 ms; the actor's write guard now
    /// prevents that). As a second line of defence, if telemetry does not
    /// match, the same frame is re-sent once; a write is idempotent.
    async fn apply_verified(&self, before: SetpointRecord, set: OutputSet) -> Result<WriteOutcome> {
        info!(
            target: "dp100",
            "apply preset={} output={} V={}mV I={}mA OVP={}mV OCP={}mA",
            set.preset.get(), on_off(set.output_on), set.setpoints.voltage.0, set.setpoints.current.0,
            set.setpoints.ovp.0, set.setpoints.ocp.0
        );
        self.write(Request::ApplyOutput(set)).await?;
        let after = self.active_set().await?;
        let mut warnings = compare(&set, &after);
        let (mut verdict, mut telemetry) = self.verify(&set).await?;
        if verdict != Verdict::Ok {
            warn!(target: "dp100", "output does not match the write ({verdict:?}); re-sending it once");
            self.write(Request::ApplyOutput(set)).await?;
            (verdict, telemetry) = self.verify(&set).await?;
            if verdict == Verdict::Ok {
                warnings.push("the device applied the write only after it was re-sent".into());
            }
        }
        let v = set.setpoints.voltage;
        match verdict {
            Verdict::Ok => {}
            Verdict::StillOn => {
                return Err(Error::OutputStillOn { vout: telemetry.vout, mode: telemetry.out_mode().as_str() });
            }
            Verdict::VoutHigh => return Err(Error::VoutAboveSetpoint { vout: telemetry.vout, setpoint: v }),
            Verdict::StillOff => warnings.push(format!(
                "device acknowledged ON but telemetry still reports the output OFF after {} ms",
                OUTPUT_SETTLE_TIMEOUT.as_millis()
            )),
            Verdict::VoutLow => warnings.push(format!(
                "Vout {} V is below the setpoint {} V in {} mode after {} ms",
                telemetry.vout.volts(),
                v.volts(),
                telemetry.out_mode().as_str(),
                VOUT_SETTLE_TIMEOUT.as_millis()
            )),
        }
        Ok(WriteOutcome { before, after, telemetry, warnings })
    }

    /// Change setpoints and/or the output state, after checking `limits`.
    ///
    /// Switching the output OFF without other changes is never refused.
    pub async fn set_output(&self, change: OutputChange, limits: &Limits) -> Result<WriteOutcome> {
        let _g = self.op_lock.lock().await;
        self.set_output_locked(change, limits).await
    }

    pub(crate) async fn set_output_locked(&self, change: OutputChange, limits: &Limits) -> Result<WriteOutcome> {
        let before = self.active_set().await?;
        let t = self.basic_info().await?;
        let active = output_active(&t);
        let sp = before.setpoints;
        let target = Setpoints {
            voltage: change.voltage.unwrap_or(sp.voltage),
            current: change.current.unwrap_or(sp.current),
            ..sp
        };
        // "Unchanged" means the real state, not the (unreliable) state byte.
        let on = change.enable.unwrap_or(active);
        let setpoints_changed = target != sp;
        if on || setpoints_changed {
            limits.check_static(target.voltage, target.current)?;
            Limits::check_protections(target.voltage, target.current, target.ovp, target.ocp)?;
            limits.check_input(target.voltage, &t)?;
        }
        let set = OutputSet { preset: PresetIndex::new(before.index)?, output_on: on, setpoints: target };
        self.apply_verified(before, set).await
    }

    /// Change the protections (OVP/OCP) of the active set.
    pub async fn set_protections(
        &self,
        ovp: Option<Millivolts>,
        ocp: Option<Milliamps>,
        limits: &Limits,
    ) -> Result<WriteOutcome> {
        let _g = self.op_lock.lock().await;
        let before = self.active_set().await?;
        let target = Setpoints {
            ovp: ovp.unwrap_or(before.setpoints.ovp),
            ocp: ocp.unwrap_or(before.setpoints.ocp),
            ..before.setpoints
        };
        Limits::check_protections(target.voltage, target.current, target.ovp, target.ocp)?;
        let t = self.basic_info().await?;
        let active = output_active(&t);
        if active {
            limits.check_output(target.voltage, target.current, target.ovp, target.ocp, &t)?;
        }
        let set = OutputSet { preset: PresetIndex::new(before.index)?, output_on: active, setpoints: target };
        self.apply_verified(before, set).await
    }

    /// Switch the output off, wait `off`, switch it back on with the current
    /// setpoints, wait `settle`, measure again.
    pub async fn power_cycle(
        &self,
        off: Duration,
        settle: Duration,
        allow_from_off: bool,
        limits: &Limits,
    ) -> Result<PowerCycleOutcome> {
        let _g = self.op_lock.lock().await;
        let before = self.status().await?;
        let (t0, s0) = before;
        if !output_active(&t0) && !allow_from_off {
            return Err(Error::InvalidArgument(
                "the output is currently OFF; power_cycle would switch it ON (pass allow_from_off=true to accept)"
                    .into(),
            ));
        }
        let sp = s0.setpoints;
        // Refuse up front rather than switching off and failing to switch back on.
        limits.check_output(sp.voltage, sp.current, sp.ovp, sp.ocp, &t0)?;
        let mut warnings = Vec::new();
        let off_res =
            self.set_output_locked(OutputChange { enable: Some(false), ..Default::default() }, limits).await?;
        warnings.extend(off_res.warnings);
        tokio::time::sleep(off).await;
        let on_res = self
            .set_output_locked(OutputChange { enable: Some(true), ..Default::default() }, limits)
            .await
            .map_err(|e| Error::InvalidArgument(format!("output left OFF: re-enable refused: {e}")))?;
        warnings.extend(on_res.warnings);
        tokio::time::sleep(settle).await;
        let after = self.status().await?;
        Ok(PowerCycleOutcome { before, after, warnings })
    }

    /// Store a preset (1–9) in flash. Does not touch the output.
    pub async fn save_preset(&self, index: PresetIndex, sp: Setpoints, limits: &Limits) -> Result<SetpointRecord> {
        let index = index.writable()?;
        limits.check_static(sp.voltage, sp.current)?;
        Limits::check_protections(sp.voltage, sp.current, sp.ovp, sp.ocp)?;
        let _g = self.op_lock.lock().await;
        self.write(Request::SavePreset { index, setpoints: sp }).await?;
        let got = self.preset(index).await?;
        if got.setpoints != sp {
            return Err(Error::InvalidArgument(format!(
                "preset {} read back as {:?} instead of {:?}",
                index.get(),
                got.setpoints,
                sp
            )));
        }
        Ok(got)
    }

    /// Recall preset 1–9 as the active set. If the device switched the output
    /// on as a side effect, it is switched back off and an error is returned.
    pub async fn recall_preset(&self, index: PresetIndex, limits: &Limits) -> Result<WriteOutcome> {
        let index = index.writable()?;
        let _g = self.op_lock.lock().await;
        let before = self.active_set().await?;
        let t0 = self.basic_info().await?;
        let was_active = output_active(&t0);
        let p = self.preset(index).await?;
        let sp = p.setpoints;
        limits.check_static(sp.voltage, sp.current)?;
        Limits::check_protections(sp.voltage, sp.current, sp.ovp, sp.ocp)?;
        if was_active {
            limits.check_input(sp.voltage, &t0)?;
        }
        self.write(Request::RecallPreset { index, setpoints: sp }).await?;
        let after = self.active_set().await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let telemetry = self.basic_info().await?;
        let mut warnings = Vec::new();
        if output_active(&telemetry) && !was_active {
            let off =
                OutputSet { preset: PresetIndex::new(after.index)?, output_on: false, setpoints: after.setpoints };
            self.apply_verified(after, off).await?;
            return Err(Error::InvalidArgument(format!(
                "recalling preset {} switched the output ON; it was switched back OFF",
                index.get()
            )));
        }
        if after.index != index.get() {
            warnings.push(format!("active preset reads back as {} instead of {}", after.index, index.get()));
        }
        if after.setpoints != sp {
            warnings.push(format!("active setpoints {:?} differ from preset {:?}", after.setpoints, sp));
        }
        Ok(WriteOutcome { before, after, telemetry, warnings })
    }

    /// Write system settings and return what the device reads back.
    pub async fn write_system_settings(&self, s: SystemSettings) -> Result<(SystemSettings, Vec<String>)> {
        if !device_limits::OTP_RANGE_C.contains(&s.otp_c) {
            return Err(Error::InvalidArgument(format!("OTP {} °C outside 50–80 °C", s.otp_c)));
        }
        if s.opp.0 > device_limits::MAX_OPP_DW {
            return Err(Error::InvalidArgument(format!("OPP {} W above 105 W", s.opp.watts())));
        }
        if s.backlight > device_limits::MAX_LEVEL || s.volume > device_limits::MAX_LEVEL {
            return Err(Error::InvalidArgument("backlight and volume range from 0 to 4".into()));
        }
        let _g = self.op_lock.lock().await;
        self.write(Request::WriteSystemSettings(s)).await?;
        let got = self.system_settings().await?;
        let warnings = if got != s { vec![format!("settings read back as {got:?} instead of {s:?}")] } else { vec![] };
        Ok((got, warnings))
    }
}
