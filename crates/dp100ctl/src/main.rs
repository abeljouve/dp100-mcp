//! `dp100ctl`: manual debugging CLI for the ALIENTEK DP100.

use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use dp100::{
    ActorOptions, DeviceManager, Dp100, Limits, LimitsConfig, OutputChange, PROTO_TARGET,
    config::FileConfig,
    measure::{Sample, write_csv},
    proto::{Milliamps, Millivolts},
    report::{DeviceInfoReport, PresetReport, SetpointReport, StatusReport, SystemSettingsReport},
    sim::SimDevice,
};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(version, about = "Debug CLI for the ALIENTEK DP100 power supply")]
struct Cli {
    /// USB serial (or hidraw path) of the unit to drive.
    #[arg(long, global = true, env = "DP100_SERIAL")]
    serial: Option<String>,
    /// TOML configuration file ([device], [limits]).
    #[arg(long, global = true, env = "DP100_CONFIG")]
    config: Option<PathBuf>,
    /// Refuse setpoints above this voltage (V).
    #[arg(long, global = true)]
    max_voltage: Option<f64>,
    /// Refuse current limits above this current (A).
    #[arg(long, global = true)]
    max_current: Option<f64>,
    /// Required margin between Vin and Vout (mV, default 500).
    #[arg(long, global = true)]
    vin_dropout_mv: Option<u16>,
    /// Hex-dump every TX/RX frame on stderr.
    #[arg(long, global = true)]
    trace_proto: bool,
    /// Talk to an in-memory simulator instead of hardware.
    #[arg(long, global = true, hide = true)]
    simulate: bool,
    /// JSON output.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// List connected DP100 units.
    List,
    /// Show measurements and setpoints.
    Status,
    /// Change setpoints; the output state only changes with --on / --off.
    Set {
        /// Output voltage (V).
        #[arg(short, long)]
        voltage: Option<f64>,
        /// Current limit (A).
        #[arg(short, long)]
        current: Option<f64>,
        /// Also switch the output on.
        #[arg(long, conflicts_with = "off")]
        on: bool,
        /// Also switch the output off.
        #[arg(long)]
        off: bool,
    },
    /// Switch the output on with the current setpoints.
    On,
    /// Switch the output off.
    Off,
    /// Switch off, wait, switch back on.
    Cycle {
        #[arg(long, default_value_t = 1000)]
        off_ms: u64,
        #[arg(long, default_value_t = 200)]
        settle_ms: u64,
        /// Allow cycling when the output is currently off (it ends up on).
        #[arg(long)]
        from_off: bool,
    },
    /// Print telemetry continuously (Ctrl-C to stop).
    Watch {
        #[arg(long, default_value_t = 5.0)]
        rate_hz: f64,
        /// Stop after this many seconds.
        #[arg(long)]
        duration_s: Option<f64>,
        /// Also write samples to this CSV file.
        #[arg(long)]
        csv: Option<PathBuf>,
    },
    /// Show the 10 presets.
    Presets,
    /// Show system settings.
    Settings,
}

fn init_logging(trace_proto: bool) {
    let mut filter = EnvFilter::try_from_env("DP100_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    if trace_proto {
        filter = filter.add_directive(format!("{PROTO_TARGET}=trace").parse().expect("valid directive"));
    }
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();
}

fn print_json<T: serde::Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn print_status(r: &StatusReport) {
    let t = &r.telemetry;
    let s = &r.setpoint;
    println!("output   {}   mode {}   protection {}", if r.output_on { "ON " } else { "OFF" }, t.mode, t.protection);
    if r.output_state_flag_mismatch {
        println!(
            "warning  the device's state byte says {} but telemetry shows the output {}",
            if s.output_on { "ON" } else { "OFF" },
            if r.output_on { "ON" } else { "OFF" }
        );
    }
    println!("measured {:7.3} V  {:6.3} A  {:7.3} W", t.vout_v, t.iout_a, t.power_w);
    println!(
        "setpoint {:7.3} V  {:6.3} A   (OVP {:.3} V, OCP {:.3} A, preset {})",
        s.voltage_v, s.current_limit_a, s.ovp_v, s.ocp_a, s.preset
    );
    println!(
        "input    {:7.3} V   vo_max {:.3} V   ceiling {:.3} V   5V rail {:.3} V",
        t.vin_v, t.vo_max_v, t.vout_ceiling_v, t.rail_5v_v
    );
    println!("temp     {:.1} °C / {:.1} °C", t.temperature1_c, t.temperature2_c);
}

fn print_write(w: &dp100::WriteOutcome, json: bool) -> Result<()> {
    let after = SetpointReport::from(&w.after);
    let real_on = dp100::output_active(&w.telemetry);
    if json {
        return print_json(&serde_json::json!({ "output_on": real_on, "applied": after, "warnings": w.warnings }));
    }
    println!(
        "applied: output {}  {:.3} V  {:.3} A  (OVP {:.3} V, OCP {:.3} A)",
        if real_on { "ON" } else { "OFF" },
        after.voltage_v,
        after.current_limit_a,
        after.ovp_v,
        after.ocp_a
    );
    for m in &w.warnings {
        println!("warning: {m}");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(cli.trace_proto);

    let file = match &cli.config {
        Some(p) => FileConfig::load(p).map_err(anyhow::Error::msg)?,
        None => FileConfig::default(),
    };
    let flags = LimitsConfig {
        max_voltage_v: cli.max_voltage,
        max_current_a: cli.max_current,
        vin_dropout_mv: cli.vin_dropout_mv,
    };
    let limits: Limits = file.limits.clone().merged(flags).resolve().map_err(anyhow::Error::msg)?;
    let selector = cli.serial.clone().or(file.device.serial.clone());
    let manager = if cli.simulate {
        DeviceManager::sim(vec![SimDevice::new("SIM0001")], ActorOptions::default())
    } else {
        DeviceManager::hid(selector.clone(), ActorOptions::default())
    };

    if let Cmd::List = cli.cmd {
        let mut rows = Vec::new();
        for desc in manager.list()? {
            let (_, d) = manager.get(Some(&desc.key()))?;
            let info = d.device_info().await.map(|i| DeviceInfoReport::new(desc.usb_serial.clone(), &i));
            rows.push(serde_json::json!({
                "path": desc.path,
                "usb_serial": desc.usb_serial,
                "info": info.as_ref().ok(),
                "error": info.as_ref().err().map(ToString::to_string),
            }));
        }
        if cli.json {
            return print_json(&rows);
        }
        if rows.is_empty() {
            println!("no DP100 found");
        }
        for r in rows {
            println!(
                "{}  serial {}  fw {}  hw {}  {}",
                r["path"].as_str().unwrap_or("?"),
                r["usb_serial"].as_str().unwrap_or("-"),
                r["info"]["firmware_version"].as_str().unwrap_or("?"),
                r["info"]["hardware_version"].as_str().unwrap_or("?"),
                r["error"].as_str().unwrap_or("")
            );
        }
        return Ok(());
    }

    let (_, d) = manager.get(selector.as_deref())?;
    d.ensure_application().await.context("talking to the DP100")?;
    run(&cli, &d, &limits).await
}

async fn run(cli: &Cli, d: &Dp100, limits: &Limits) -> Result<()> {
    match &cli.cmd {
        Cmd::List => unreachable!(),
        Cmd::Status => {
            let (t, s) = d.status().await?;
            let r = StatusReport::new(&t, &s, limits);
            if cli.json { print_json(&r)? } else { print_status(&r) }
        }
        Cmd::Set { voltage, current, on, off } => {
            if voltage.is_none() && current.is_none() && !on && !off {
                bail!("nothing to do: pass --voltage, --current, --on or --off");
            }
            let change = OutputChange {
                voltage: voltage.map(Millivolts::from_volts).transpose()?,
                current: current.map(Milliamps::from_amps).transpose()?,
                enable: if *on {
                    Some(true)
                } else if *off {
                    Some(false)
                } else {
                    None
                },
            };
            print_write(&d.set_output(change, limits).await?, cli.json)?;
        }
        Cmd::On => print_write(
            &d.set_output(OutputChange { enable: Some(true), ..Default::default() }, limits).await?,
            cli.json,
        )?,
        Cmd::Off => print_write(
            &d.set_output(OutputChange { enable: Some(false), ..Default::default() }, limits).await?,
            cli.json,
        )?,
        Cmd::Cycle { off_ms, settle_ms, from_off } => {
            let r = d
                .power_cycle(Duration::from_millis(*off_ms), Duration::from_millis(*settle_ms), *from_off, limits)
                .await?;
            let before = StatusReport::new(&r.before.0, &r.before.1, limits);
            let after = StatusReport::new(&r.after.0, &r.after.1, limits);
            if cli.json {
                print_json(&serde_json::json!({ "before": before, "after": after, "warnings": r.warnings }))?;
            } else {
                println!("-- before");
                print_status(&before);
                println!("-- after");
                print_status(&after);
                for m in &r.warnings {
                    println!("warning: {m}");
                }
            }
        }
        Cmd::Watch { rate_hz, duration_s, csv } => watch(cli, d, *rate_hz, *duration_s, csv.as_deref()).await?,
        Cmd::Presets => {
            let active = d.active_set().await?.index;
            let p: Vec<PresetReport> = d.presets().await?.iter().map(|p| PresetReport::new(p, active)).collect();
            if cli.json {
                print_json(&p)?;
            } else {
                for s in p {
                    println!(
                        "P{}  {:7.3} V  {:6.3} A   OVP {:7.3} V  OCP {:6.3} A{}",
                        s.preset,
                        s.voltage_v,
                        s.current_limit_a,
                        s.ovp_v,
                        s.ocp_a,
                        if s.active { "   (active preset)" } else { "" }
                    );
                }
            }
        }
        Cmd::Settings => {
            let s = SystemSettingsReport::from(&d.system_settings().await?);
            print_json(&s)?;
        }
    }
    Ok(())
}

async fn watch(
    cli: &Cli,
    d: &Dp100,
    rate_hz: f64,
    duration_s: Option<f64>,
    csv: Option<&std::path::Path>,
) -> Result<()> {
    if !(rate_hz > 0.0 && rate_hz <= dp100::measure::MAX_RATE_HZ) {
        bail!("--rate-hz must be in (0, {}]", dp100::measure::MAX_RATE_HZ);
    }
    let mut ticker = tokio::time::interval(Duration::from_secs_f64(1.0 / rate_hz));
    let start = tokio::time::Instant::now();
    let mut samples: Vec<Sample> = Vec::new();
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    if !cli.json {
        println!("{:>9}  {:>8}  {:>7}  {:>8}  {:>8}  mode  prot", "t_s", "Vout_V", "Iout_A", "P_W", "Vin_V");
    }
    loop {
        tokio::select! {
            _ = &mut stop => break,
            _ = ticker.tick() => {}
        }
        let t = start.elapsed().as_secs_f64();
        if duration_s.is_some_and(|d| t > d) {
            break;
        }
        let b = d.basic_info().await?;
        if cli.json {
            println!(
                "{}",
                serde_json::json!({"t_s": t, "vout_v": b.vout.volts(), "iout_a": b.iout.amps(), "power_w": b.power_mw() / 1000.0,
                                   "vin_v": b.vin.volts(), "mode": b.out_mode().as_str(), "protection": b.work_state().as_str()})
            );
        } else {
            println!(
                "{t:9.3}  {:8.3}  {:7.3}  {:8.3}  {:8.3}  {:4}  {}",
                b.vout.volts(),
                b.iout.amps(),
                b.power_mw() / 1000.0,
                b.vin.volts(),
                b.out_mode().as_str(),
                b.work_state().as_str()
            );
        }
        if csv.is_some() {
            samples.push(Sample {
                t_ms: t * 1000.0,
                vin_mv: b.vin.0,
                vout_mv: b.vout.0,
                iout_ma: b.iout.0,
                power_mw: b.power_mw(),
                mode_raw: b.out_mode_raw,
                protection_raw: b.work_state_raw,
            });
        }
    }
    if let Some(p) = csv {
        write_csv(p, &samples).with_context(|| format!("writing {}", p.display()))?;
        eprintln!("wrote {} samples to {}", samples.len(), p.display());
    }
    Ok(())
}
