//! Host-timed sampling and voltage ramps.

use std::{io::Write, path::Path, time::Duration};

use dp100_proto::{BasicInfo, Millivolts};
use schemars::JsonSchema;
use serde::Serialize;
use tokio::time::{Instant, MissedTickBehavior};

use crate::{Dp100, Error, Result, limits::Limits, ops::OutputChange, report::SetpointReport};

/// Hard caps protecting the host and the USB link.
pub const MAX_RATE_HZ: f64 = 200.0;
pub const MAX_DURATION: Duration = Duration::from_secs(3600);
pub const MAX_RAMP_STEPS: usize = 1000;

/// One telemetry sample.
#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
pub struct Sample {
    /// Milliseconds since the start of the measurement.
    pub t_ms: f64,
    pub vin_mv: u16,
    pub vout_mv: u16,
    pub iout_ma: u16,
    pub power_mw: f64,
    pub mode_raw: u8,
    pub protection_raw: u8,
}

impl Sample {
    fn new(t_ms: f64, b: &BasicInfo) -> Self {
        Self {
            t_ms,
            vin_mv: b.vin.0,
            vout_mv: b.vout.0,
            iout_ma: b.iout.0,
            power_mw: b.power_mw(),
            mode_raw: b.out_mode_raw,
            protection_raw: b.work_state_raw,
        }
    }
}

fn r6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// Min / max / mean of a series.
#[derive(Debug, Clone, Copy, Default, Serialize, JsonSchema)]
pub struct Stat {
    pub min: f64,
    pub max: f64,
    pub mean: f64,
}

impl Stat {
    fn of(values: impl Iterator<Item = f64>) -> Self {
        let (mut min, mut max, mut sum, mut n) = (f64::INFINITY, f64::NEG_INFINITY, 0.0, 0usize);
        for v in values {
            min = min.min(v);
            max = max.max(v);
            sum += v;
            n += 1;
        }
        if n == 0 { Stat::default() } else { Stat { min: r6(min), max: r6(max), mean: r6(sum / n as f64) } }
    }
}

/// Aggregates of a measurement window.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct MeasureSummary {
    pub samples: usize,
    pub missed_samples: usize,
    pub duration_s: f64,
    pub requested_rate_hz: f64,
    pub achieved_rate_hz: f64,
    pub vout_v: Stat,
    pub iout_a: Stat,
    pub power_w: Stat,
    pub vin_v: Stat,
    /// Trapezoidal integral of P over time.
    pub energy_mwh: f64,
    /// Trapezoidal integral of I over time.
    pub charge_mah: f64,
    /// True if any sample reported a tripped protection.
    pub protection_tripped: bool,
}

pub fn summarize(samples: &[Sample], requested_rate_hz: f64, missed: usize) -> MeasureSummary {
    let duration_s = match (samples.first(), samples.last()) {
        (Some(a), Some(b)) => (b.t_ms - a.t_ms) / 1000.0,
        _ => 0.0,
    };
    let (mut energy_mws, mut charge_mas) = (0.0, 0.0);
    for w in samples.windows(2) {
        let dt = (w[1].t_ms - w[0].t_ms) / 1000.0;
        energy_mws += (w[0].power_mw + w[1].power_mw) / 2.0 * dt;
        charge_mas += (f64::from(w[0].iout_ma) + f64::from(w[1].iout_ma)) / 2.0 * dt;
    }
    MeasureSummary {
        samples: samples.len(),
        missed_samples: missed,
        duration_s: r6(duration_s),
        requested_rate_hz,
        achieved_rate_hz: if duration_s > 0.0 { r6((samples.len() - 1) as f64 / duration_s) } else { 0.0 },
        vout_v: Stat::of(samples.iter().map(|s| f64::from(s.vout_mv) / 1000.0)),
        iout_a: Stat::of(samples.iter().map(|s| f64::from(s.iout_ma) / 1000.0)),
        power_w: Stat::of(samples.iter().map(|s| s.power_mw / 1000.0)),
        vin_v: Stat::of(samples.iter().map(|s| f64::from(s.vin_mv) / 1000.0)),
        energy_mwh: r6(energy_mws / 3600.0),
        charge_mah: r6(charge_mas / 3600.0),
        protection_tripped: samples.iter().any(|s| s.protection_raw != 0),
    }
}

/// Write samples as CSV (`t_ms,vin_mv,vout_mv,iout_ma,power_mw,mode_raw,protection_raw`).
pub fn write_csv(path: &Path, samples: &[Sample]) -> std::io::Result<()> {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    writeln!(f, "t_ms,vin_mv,vout_mv,iout_ma,power_mw,mode_raw,protection_raw")?;
    for s in samples {
        writeln!(
            f,
            "{:.3},{},{},{},{:.3},{},{}",
            s.t_ms, s.vin_mv, s.vout_mv, s.iout_ma, s.power_mw, s.mode_raw, s.protection_raw
        )?;
    }
    f.flush()
}

impl Dp100 {
    /// Sample telemetry at `rate_hz` for `duration`. Read-only.
    pub async fn measure(&self, duration: Duration, rate_hz: f64) -> Result<(Vec<Sample>, MeasureSummary)> {
        if !(rate_hz > 0.0 && rate_hz <= MAX_RATE_HZ) {
            return Err(Error::InvalidArgument(format!("rate_hz must be in (0, {MAX_RATE_HZ}]")));
        }
        if duration.is_zero() || duration > MAX_DURATION {
            return Err(Error::InvalidArgument(format!("duration must be in (0, {} s]", MAX_DURATION.as_secs())));
        }
        let period = Duration::from_secs_f64(1.0 / rate_hz);
        let mut ticker = tokio::time::interval(period);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let start = Instant::now();
        let expected = (duration.as_secs_f64() * rate_hz).floor() as usize + 1;
        let mut samples = Vec::with_capacity(expected);
        loop {
            let tick = ticker.tick().await;
            if tick.duration_since(start) > duration {
                break;
            }
            let b = self.basic_info().await?;
            samples.push(Sample::new(start.elapsed().as_secs_f64() * 1000.0, &b));
        }
        let missed = expected.saturating_sub(samples.len());
        let summary = summarize(&samples, rate_hz, missed);
        Ok((samples, summary))
    }
}

/// One plateau of a ramp.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RampStep {
    pub requested_v: f64,
    /// Setpoint read back from the device.
    pub applied: SetpointReport,
    /// Measured at the end of the dwell time.
    pub vout_v: f64,
    pub iout_a: f64,
    pub power_w: f64,
    pub mode: String,
    pub protection: String,
}

/// Outcome of a ramp.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RampReport {
    pub steps: Vec<RampStep>,
    pub completed: bool,
    pub stopped_reason: Option<String>,
    pub warnings: Vec<String>,
}

/// Voltages visited from `start` to `end` (inclusive) by `step`.
pub fn ramp_points(start: Millivolts, end: Millivolts, step: Millivolts) -> Result<Vec<Millivolts>> {
    if step.0 == 0 {
        return Err(Error::InvalidArgument("step_v must be > 0".into()));
    }
    let (s, e, st) = (i32::from(start.0), i32::from(end.0), i32::from(step.0));
    let dir = if e >= s { 1 } else { -1 };
    let n = ((e - s).abs() / st) as usize;
    if n + 2 > MAX_RAMP_STEPS {
        return Err(Error::InvalidArgument(format!("ramp would need more than {MAX_RAMP_STEPS} steps")));
    }
    let mut pts: Vec<Millivolts> = (0..=n).map(|k| Millivolts((s + dir * st * k as i32) as u16)).collect();
    if pts.last() != Some(&end) {
        pts.push(end);
    }
    Ok(pts)
}

impl Dp100 {
    /// Step the voltage through `points`, waiting `dwell` and measuring at
    /// each plateau. Every point is validated before the first write.
    /// `enable`: same semantics as `set_output` (`None` keeps ON/OFF state),
    /// applied with the first point. Stops on any tripped protection.
    pub async fn ramp(
        &self,
        points: &[Millivolts],
        dwell: Duration,
        enable: Option<bool>,
        limits: &Limits,
    ) -> Result<RampReport> {
        let _g = self.op_lock.lock().await;
        let (t, s) = self.status().await?;
        let sp = s.setpoints;
        for &v in points {
            limits.check_static(v, sp.current)?;
            Limits::check_protections(v, sp.current, sp.ovp, sp.ocp)?;
            limits.check_input(v, &t)?;
        }
        let mut report = RampReport { steps: Vec::new(), completed: false, stopped_reason: None, warnings: Vec::new() };
        let will_be_on = enable.unwrap_or(s.output_on);
        if !will_be_on {
            report.warnings.push("output is OFF: setpoints are stepped but nothing is delivered".into());
        }
        for (k, &v) in points.iter().enumerate() {
            let change = OutputChange { voltage: Some(v), current: None, enable: if k == 0 { enable } else { None } };
            let w = match self.set_output_locked(change, limits).await {
                Ok(w) => w,
                Err(e) => {
                    report.stopped_reason = Some(format!("step {k} ({} V): {e}", v.volts()));
                    return Ok(report);
                }
            };
            report.warnings.extend(w.warnings.iter().map(|m| format!("step {k}: {m}")));
            tokio::time::sleep(dwell).await;
            let b = self.basic_info().await?;
            report.steps.push(RampStep {
                requested_v: v.volts(),
                applied: (&w.after).into(),
                vout_v: b.vout.volts(),
                iout_a: b.iout.amps(),
                power_w: b.power_mw() / 1000.0,
                mode: b.out_mode().as_str().into(),
                protection: b.work_state().as_str().into(),
            });
            if b.work_state().is_tripped() {
                report.stopped_reason = Some(format!("protection {} tripped at step {k}", b.work_state().as_str()));
                return Ok(report);
            }
        }
        report.completed = true;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramp_points_cover_endpoints() {
        let mv = Millivolts;
        assert_eq!(ramp_points(mv(1000), mv(2000), mv(500)).unwrap(), vec![mv(1000), mv(1500), mv(2000)]);
        assert_eq!(ramp_points(mv(1000), mv(2200), mv(500)).unwrap(), vec![mv(1000), mv(1500), mv(2000), mv(2200)]);
        assert_eq!(ramp_points(mv(3000), mv(2000), mv(400)).unwrap(), vec![mv(3000), mv(2600), mv(2200), mv(2000)]);
        assert_eq!(ramp_points(mv(5000), mv(5000), mv(100)).unwrap(), vec![mv(5000)]);
        assert!(ramp_points(mv(0), mv(5000), mv(0)).is_err());
        assert!(ramp_points(mv(0), mv(30000), mv(1)).is_err());
    }

    #[test]
    fn energy_integration() {
        // Constant 5 V × 200 mA = 1 W for 3.6 s → 1 mWh, 0.2 mAh.
        let samples: Vec<Sample> = (0..=36)
            .map(|k| Sample {
                t_ms: f64::from(k) * 100.0,
                vin_mv: 15000,
                vout_mv: 5000,
                iout_ma: 200,
                power_mw: 1000.0,
                mode_raw: 1,
                protection_raw: 0,
            })
            .collect();
        let s = summarize(&samples, 10.0, 0);
        assert!((s.energy_mwh - 1.0).abs() < 1e-9);
        assert!((s.charge_mah - 0.2).abs() < 1e-9);
        assert!((s.achieved_rate_hz - 10.0).abs() < 1e-9);
        assert!((s.power_w.mean - 1.0).abs() < 1e-12);
        assert!(!s.protection_tripped);
    }
}
