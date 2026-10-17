//! Hardware-in-the-loop tests. Need a real DP100.
//!
//! Read-only tests:   `cargo test -p dp100 --features hil --test hil`
//! Output tests (switch the output, use a dummy load or nothing attached!):
//!   `DP100_HIL_ALLOW_OUTPUT=1 cargo test -p dp100 --features hil --test hil -- --ignored --test-threads=1`
//! Select a unit with `DP100_SERIAL=<usb serial>`.
#![cfg(feature = "hil")]

use std::time::Duration;

use dp100::{ActorOptions, Dp100, Limits, LimitsConfig, OutputChange, proto::Millivolts};

fn open() -> Dp100 {
    Dp100::open_hid(std::env::var("DP100_SERIAL").ok(), ActorOptions::default())
}

fn output_tests_allowed() -> bool {
    std::env::var("DP100_HIL_ALLOW_OUTPUT").as_deref() == Ok("1")
}

#[tokio::test]
async fn hil_device_info() {
    let d = open();
    let info = d.ensure_application().await.expect("device info");
    assert_eq!(info.model, "ATK-DP100");
    eprintln!("{info:?}");
}

#[tokio::test]
async fn hil_status_is_plausible() {
    let d = open();
    let (t, s) = d.status().await.expect("status");
    eprintln!("{t:?}\n{s:?}");
    assert!(t.vin.0 > 4_000 && t.vin.0 < 31_000, "Vin {}", t.vin);
    assert!(t.vo_max <= t.vin);
    assert!(t.temp1.celsius() > 0.0 && t.temp1.celsius() < 90.0);
    assert!(s.setpoints.ovp.0 <= 30_500 && s.setpoints.ocp.0 <= 5_050);
}

#[tokio::test]
async fn hil_presets_and_settings() {
    let d = open();
    assert_eq!(d.presets().await.expect("presets").len(), 10);
    let s = d.system_settings().await.expect("settings");
    assert!((50..=80).contains(&s.otp_c));
}

#[tokio::test]
async fn hil_polling_rate() {
    let d = open();
    let (samples, sum) = d.measure(Duration::from_secs(2), 100.0).await.expect("measure");
    eprintln!("{} samples, achieved {:.1} Hz, missed {}", samples.len(), sum.achieved_rate_hz, sum.missed_samples);
    assert!(sum.achieved_rate_hz > 50.0);
}

/// Changes the setpoint while keeping the output OFF, then restores it.
#[tokio::test]
#[ignore = "writes to the device; set DP100_HIL_ALLOW_OUTPUT=1"]
async fn hil_setpoint_write_with_output_off() {
    assert!(output_tests_allowed(), "set DP100_HIL_ALLOW_OUTPUT=1");
    let d = open();
    let l =
        LimitsConfig { max_voltage_v: Some(5.0), max_current_a: Some(0.5), vin_dropout_mv: None }.resolve().unwrap();
    let start = d.active_set().await.unwrap();
    if start.output_on {
        d.set_output(OutputChange { enable: Some(false), ..Default::default() }, &Limits::default()).await.unwrap();
    }
    let w = d.set_output(OutputChange { voltage: Some(Millivolts(3300)), ..Default::default() }, &l).await.unwrap();
    assert!(!w.after.output_on);
    assert_eq!(w.after.setpoints.voltage, Millivolts(3300));
    assert!(w.warnings.is_empty(), "{:?}", w.warnings);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(d.basic_info().await.unwrap().vout.0 < 100, "output must stay OFF");
    // Restore the original setpoints (output stays OFF).
    let restore =
        OutputChange { voltage: Some(start.setpoints.voltage), current: Some(start.setpoints.current), enable: None };
    d.set_output(restore, &Limits::default()).await.unwrap();
}
