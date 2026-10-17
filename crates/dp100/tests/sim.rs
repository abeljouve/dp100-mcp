//! Device-layer behaviour against the protocol simulator (no hardware).

use std::time::Duration;

use dp100::{
    ActorOptions, DeviceManager, Dp100, Error, LimitError, Limits, LimitsConfig, OutputChange,
    proto::{Milliamps, Millivolts, PresetIndex, Setpoints},
    sim::SimDevice,
};

fn opts() -> ActorOptions {
    ActorOptions {
        reply_timeout: Duration::from_millis(30),
        read_retries: 2,
        call_timeout: Duration::from_secs(3),
        write_guard: Duration::from_millis(30),
    }
}

fn dev() -> (SimDevice, Dp100) {
    let sim = SimDevice::new("SIM0001");
    let h = Dp100::spawn(sim.clone(), opts());
    (sim, h)
}

fn v(mv: u16) -> Option<Millivolts> {
    Some(Millivolts(mv))
}

#[tokio::test]
async fn reads_status() {
    let (_sim, d) = dev();
    let info = d.ensure_application().await.unwrap();
    assert_eq!(info.fw_version_str(), "1.5");
    let (t, s) = d.status().await.unwrap();
    assert_eq!(t.vin, Millivolts(15223));
    assert!(!s.output_on);
    assert!(d.is_connected());
}

#[tokio::test]
async fn set_output_without_enable_keeps_state() {
    let (sim, d) = dev();
    let l = Limits::default();
    let w = d.set_output(OutputChange { voltage: v(3300), ..Default::default() }, &l).await.unwrap();
    assert!(!w.after.output_on, "output must stay OFF without enable");
    assert_eq!(w.after.setpoints.voltage, Millivolts(3300));
    assert!(w.warnings.is_empty());
    // Protections are preserved from the read-back, never zeroed.
    let last = sim.with(|s| s.writes.last().cloned().unwrap());
    assert_eq!(&last[..2], &[0x20, 0x00]);
    assert_eq!(&last[6..10], &[0x24, 0x77, 0xBA, 0x13]);

    sim.with(|s| s.set_on(true));
    let w = d.set_output(OutputChange { current: Some(Milliamps(200)), ..Default::default() }, &l).await.unwrap();
    assert!(w.after.output_on, "output must stay ON without enable");
}

#[tokio::test]
async fn enable_and_disable() {
    let (sim, d) = dev();
    let l = Limits::default();
    let w = d.set_output(OutputChange { enable: Some(true), ..Default::default() }, &l).await.unwrap();
    assert!(w.after.output_on && !w.before.output_on);
    assert!(sim.with(|s| s.output_active));
    let w = d.set_output(OutputChange { enable: Some(false), ..Default::default() }, &l).await.unwrap();
    assert!(!w.after.output_on);
}

#[tokio::test]
async fn limits_refuse_before_sending() {
    let (sim, d) = dev();
    let l =
        LimitsConfig { max_voltage_v: Some(12.0), max_current_a: Some(1.0), vin_dropout_mv: None }.resolve().unwrap();
    let e = d.set_output(OutputChange { voltage: v(12500), ..Default::default() }, &l).await.unwrap_err();
    assert!(matches!(e, Error::Limit(LimitError::VoltageAboveMax { .. })), "{e}");
    let e = d.set_output(OutputChange { current: Some(Milliamps(1500)), ..Default::default() }, &l).await.unwrap_err();
    assert!(matches!(e, Error::Limit(LimitError::CurrentAboveMax { .. })), "{e}");
    // Vin 15.223 V → ceiling 14.723 V even with generous configured max.
    let e =
        d.set_output(OutputChange { voltage: v(14800), ..Default::default() }, &Limits::default()).await.unwrap_err();
    assert!(matches!(e, Error::Limit(LimitError::VoltageAboveInput { .. })), "{e}");
    assert!(sim.with(|s| s.writes.is_empty()), "nothing may be written when refused");
    assert!(d.set_output(OutputChange { voltage: v(14723), ..Default::default() }, &Limits::default()).await.is_ok());
}

#[tokio::test]
async fn switching_off_is_never_refused() {
    let (sim, d) = dev();
    // Device left above the configured limit by someone at the front panel.
    sim.with(|s| {
        s.live[0] = 24000;
        s.set_on(true);
        s.vin_mv = 20000;
    });
    let strict = LimitsConfig { max_voltage_v: Some(5.0), ..Default::default() }.resolve().unwrap();
    let w = d.set_output(OutputChange { enable: Some(false), ..Default::default() }, &strict).await.unwrap();
    assert!(!w.after.output_on);
}

#[tokio::test]
async fn power_cycle_reports_before_and_after() {
    let (sim, d) = dev();
    sim.with(|s| {
        s.set_on(true);
        s.load_ohms = Some(100.0);
    });
    let out =
        d.power_cycle(Duration::from_millis(20), Duration::from_millis(5), false, &Limits::default()).await.unwrap();
    assert!(out.before.1.output_on && out.after.1.output_on);
    assert!(dp100::output_active(&out.after.0));
    assert_eq!(out.after.0.iout, Milliamps(50));
    let writes = sim.with(|s| s.writes.clone());
    assert_eq!(writes.len(), 2, "one OFF write, one ON write");
    assert_eq!(&writes[0][..6], &[0x20, 0, 0x88, 0x13, 0xF4, 0x01], "OFF keeps the setpoints");
    assert_eq!(writes[1][1], 1, "then ON");

    sim.with(|s| s.set_on(false));
    let e = d.power_cycle(Duration::ZERO, Duration::ZERO, false, &Limits::default()).await.unwrap_err();
    assert!(matches!(e, Error::InvalidArgument(_)));
}

#[tokio::test]
async fn reconnects_after_unplug() {
    let (sim, d) = dev();
    d.basic_info().await.unwrap();
    sim.with(|s| s.plugged = false);
    assert!(d.basic_info().await.is_err());
    assert!(!d.is_connected());
    sim.with(|s| s.plugged = true);
    d.basic_info().await.unwrap();
    assert!(d.is_connected());
}

#[tokio::test]
async fn lost_read_reply_is_retried_but_write_is_not() {
    let (sim, d) = dev();
    sim.with(|s| s.drop_replies = 1);
    d.basic_info().await.expect("read retried");
    sim.with(|s| s.drop_next_write_ack = true);
    let e =
        d.set_output(OutputChange { voltage: v(3000), ..Default::default() }, &Limits::default()).await.unwrap_err();
    assert!(matches!(e, Error::Timeout { write: true, .. }), "{e}");
    assert_eq!(sim.with(|s| s.writes.len()), 1, "write sent exactly once");
}

#[tokio::test]
async fn nak_surfaces_as_error() {
    let (sim, d) = dev();
    sim.with(|s| s.nak_next_write = true);
    let e =
        d.set_output(OutputChange { voltage: v(3000), ..Default::default() }, &Limits::default()).await.unwrap_err();
    assert!(matches!(e, Error::Proto(_)), "{e}");
}

#[tokio::test]
async fn presets() {
    let (sim, d) = dev();
    let l = Limits::default();
    let all = d.presets().await.unwrap();
    assert_eq!(all.len(), 10);
    let sp =
        Setpoints { voltage: Millivolts(3300), current: Milliamps(250), ovp: Millivolts(4000), ocp: Milliamps(500) };
    let p3 = PresetIndex::new(3).unwrap();
    assert_eq!(d.save_preset(p3, sp, &l).await.unwrap().setpoints, sp);
    let w = d.recall_preset(p3, &l).await.unwrap();
    assert_eq!(w.after.index, 3);
    assert_eq!(w.after.setpoints, sp);
    assert!(!w.after.output_on);
    let p0 = PresetIndex::new(0).unwrap();
    assert!(d.save_preset(p0, sp, &l).await.is_err());
    assert!(d.recall_preset(p0, &l).await.is_err());
    let bad = Setpoints { voltage: Millivolts(5000), ..sp };
    assert!(matches!(d.save_preset(p3, bad, &l).await, Err(Error::Limit(LimitError::VoltageAboveOvp { .. }))));
    assert_eq!(sim.with(|s| s.active_index), 3);
}

#[tokio::test]
async fn system_settings_roundtrip() {
    let (_sim, d) = dev();
    let mut s = d.system_settings().await.unwrap();
    s.backlight = 4;
    let (got, warnings) = d.write_system_settings(s).await.unwrap();
    assert_eq!(got.backlight, 4);
    assert!(warnings.is_empty());
    s.otp_c = 90;
    assert!(d.write_system_settings(s).await.is_err());
}

#[tokio::test]
async fn measure_and_ramp() {
    let (sim, d) = dev();
    sim.with(|s| {
        s.set_on(true);
        s.load_ohms = Some(50.0);
    });
    let (samples, sum) = d.measure(Duration::from_millis(300), 50.0).await.unwrap();
    assert!(samples.len() >= 10, "{} samples", samples.len());
    assert!((sum.vout_v.mean - 5.0).abs() < 1e-9);
    assert!((sum.iout_a.mean - 0.1).abs() < 1e-9);
    assert!(sum.energy_mwh > 0.0);

    let pts = dp100::measure::ramp_points(Millivolts(1000), Millivolts(3000), Millivolts(1000)).unwrap();
    let r = d.ramp(&pts, Duration::from_millis(5), None, &Limits::default()).await.unwrap();
    assert!(r.completed, "{:?}", r.stopped_reason);
    assert_eq!(r.steps.len(), 3);
    assert!((r.steps[2].vout_v - 3.0).abs() < 1e-9);
    assert!((r.steps[2].iout_a - 0.06).abs() < 1e-9);

    // A point beyond the Vin ceiling refuses the whole ramp before any write.
    let before = sim.with(|s| s.writes.len());
    let pts = dp100::measure::ramp_points(Millivolts(10000), Millivolts(16000), Millivolts(2000)).unwrap();
    assert!(d.ramp(&pts, Duration::ZERO, None, &Limits::default()).await.is_err());
    assert_eq!(sim.with(|s| s.writes.len()), before);
}

#[tokio::test]
async fn manager_selects_by_serial() {
    let a = SimDevice::new("AAA");
    let b = SimDevice::new("BBB");
    let m = DeviceManager::sim(vec![a.clone(), b.clone()], opts());
    assert!(matches!(m.get(None), Err(Error::Ambiguous(_))));
    let (key, d) = m.get(Some("BBB")).unwrap();
    assert_eq!(key, "BBB");
    b.with(|s| s.vin_mv = 20000);
    assert_eq!(d.basic_info().await.unwrap().vin, Millivolts(20000));
    a.with(|s| s.plugged = false);
    let (key, _) = m.get(None).unwrap();
    assert_eq!(key, "BBB");
    assert!(matches!(m.get(Some("ZZZ")), Err(Error::NotFound(_))));
}

#[tokio::test]
async fn off_is_one_write_and_is_verified() {
    let (sim, d) = dev();
    sim.with(|s| s.set_on(true));
    let w = d.set_output(OutputChange { enable: Some(false), ..Default::default() }, &Limits::default()).await.unwrap();
    assert!(!dp100::output_active(&w.telemetry));
    assert!(!sim.with(|s| s.output_active), "real output must be off");
    assert_eq!(w.after.setpoints.voltage, Millivolts(5000));
    assert_eq!(sim.with(|s| s.writes.len()), 1);
    assert!(w.warnings.is_empty(), "{:?}", w.warnings);
}

#[tokio::test]
async fn off_recovers_from_state_byte_desync() {
    // The state seen on hardware after a single state-0 write: byte says OFF, output is live.
    let (sim, d) = dev();
    sim.with(|s| {
        s.output_on = false;
        s.output_active = true;
    });
    let (t, s) = d.status().await.unwrap();
    let report = dp100::report::StatusReport::new(&t, &s, &Limits::default());
    assert!(report.output_on && report.output_state_flag_mismatch);
    // A setpoint change without `enable` keeps the REAL state (on)...
    let w = d.set_output(OutputChange { voltage: v(4000), ..Default::default() }, &Limits::default()).await.unwrap();
    assert!(dp100::output_active(&w.telemetry));
    assert!(w.after.output_on, "state byte resynchronised to ON");
    // ...and an explicit OFF really switches it off.
    let w = d.set_output(OutputChange { enable: Some(false), ..Default::default() }, &Limits::default()).await.unwrap();
    assert!(!sim.with(|s| s.output_active));
    assert!(!w.after.output_on);
}

#[tokio::test]
async fn off_failure_is_a_hard_error() {
    let (sim, d) = dev();
    sim.with(|s| {
        s.set_on(true);
        s.stuck = true;
    });
    let e =
        d.set_output(OutputChange { enable: Some(false), ..Default::default() }, &Limits::default()).await.unwrap_err();
    assert!(matches!(e, Error::OutputStillOn { .. }), "{e}");
    assert!(e.to_string().starts_with("SAFETY"));
    assert_eq!(sim.with(|s| s.writes.len()), 2, "re-sent exactly once");
}

#[tokio::test]
async fn setpoint_write_keeps_presets() {
    let (sim, d) = dev();
    d.set_output(OutputChange { voltage: v(3300), ..Default::default() }, &Limits::default()).await.unwrap();
    let p0 = d.preset(PresetIndex::new(0).unwrap()).await.unwrap();
    assert_eq!(p0.setpoints.voltage, Millivolts(5000), "0x20 writes do not touch stored presets (seen on HW)");
    assert_eq!(sim.with(|s| s.live[0]), 3300);
}

#[tokio::test]
async fn write_guard_avoids_the_firmware_race() {
    // Without the guard, the immediate read-back cancels the write (fw 1.5
    // behaviour); verification notices and the re-sent write is applied.
    let sim = SimDevice::new("RACE");
    sim.with(|s| {
        s.vin_mv = 20000;
        s.load_ohms = Some(100.0);
        s.live[0] = 12000;
        s.set_on(true);
    });
    let no_guard = Dp100::spawn(sim.clone(), ActorOptions { write_guard: Duration::ZERO, ..opts() });
    let w =
        no_guard.set_output(OutputChange { voltage: v(5000), ..Default::default() }, &Limits::default()).await.unwrap();
    assert!(w.warnings.iter().any(|m| m.contains("re-sent")), "{:?}", w.warnings);
    assert_eq!(sim.with(|s| s.applied[0]), 5000);
    assert_eq!(sim.with(|s| s.writes.len()), 2);

    // With the guard, the first write sticks.
    let guarded = Dp100::spawn(sim.clone(), opts());
    let w =
        guarded.set_output(OutputChange { voltage: v(6000), ..Default::default() }, &Limits::default()).await.unwrap();
    assert!(w.warnings.is_empty(), "{:?}", w.warnings);
    assert_eq!(sim.with(|s| s.applied[0]), 6000);
    assert_eq!(sim.with(|s| s.writes.len()), 3);
    assert_eq!(w.telemetry.vout, Millivolts(6000));
}

#[tokio::test]
async fn vout_stuck_above_setpoint_is_a_hard_error() {
    let (sim, d) = dev();
    sim.with(|s| {
        s.live[0] = 12000;
        s.set_on(true);
        s.stuck = true;
    });
    let e =
        d.set_output(OutputChange { voltage: v(3300), ..Default::default() }, &Limits::default()).await.unwrap_err();
    assert!(matches!(e, Error::VoutAboveSetpoint { .. }), "{e}");
    assert!(e.to_string().starts_with("SAFETY"));
}

#[tokio::test]
async fn vout_below_setpoint_in_cv_is_a_warning() {
    let (sim, d) = dev();
    sim.with(|s| {
        s.live[0] = 3000;
        s.set_on(true);
        s.stuck = true;
    });
    let w = d.set_output(OutputChange { voltage: v(5000), ..Default::default() }, &Limits::default()).await.unwrap();
    assert!(w.warnings.iter().any(|m| m.contains("below the setpoint")), "{:?}", w.warnings);
}
