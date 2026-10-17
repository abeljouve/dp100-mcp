//! `dp100-mcp`: MCP server for the ALIENTEK DP100 (stdio by default,
//! Streamable HTTP with `--http`).

mod server;

use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use dp100::{
    ActorOptions, DeviceManager, Limits, LimitsConfig, OutputChange, PROTO_TARGET, config::FileConfig,
    report::LimitsReport, sim::SimDevice,
};
use rmcp::{
    ServiceExt,
    transport::{
        stdio,
        streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
        },
    },
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::server::{CsvPolicy, Dp100Server};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OnExit {
    /// Leave the output as it is.
    Keep,
    /// Switch every DP100 used by this server OFF.
    Off,
}

#[derive(Parser, Debug)]
#[command(version, about = "MCP server for the ALIENTEK DP100 lab power supply")]
struct Args {
    /// Serve Streamable HTTP on this address (e.g. 127.0.0.1:8765) instead of stdio.
    #[arg(long, value_name = "ADDR")]
    http: Option<SocketAddr>,
    /// Extra Host header values accepted in HTTP mode (loopback names are always accepted).
    #[arg(long = "allowed-host", value_name = "HOST")]
    allowed_hosts: Vec<String>,
    /// TOML configuration file.
    #[arg(long, env = "DP100_CONFIG")]
    config: Option<PathBuf>,
    /// Default DP100 (USB serial or hidraw path).
    #[arg(long, env = "DP100_SERIAL")]
    serial: Option<String>,
    /// Refuse setpoints above this voltage (V).
    #[arg(long)]
    max_voltage: Option<f64>,
    /// Refuse current limits above this current (A).
    #[arg(long)]
    max_current: Option<f64>,
    /// Required margin between Vin and Vout in mV (default 500).
    #[arg(long)]
    vin_dropout_mv: Option<u16>,
    /// What to do with the outputs when the server stops (SIGINT/SIGTERM or client disconnect).
    #[arg(long, value_enum)]
    on_exit: Option<OnExit>,
    /// Directory where `measure` may write CSV files. Without it, stdio mode
    /// accepts any absolute path and HTTP mode disables CSV output.
    #[arg(long)]
    csv_dir: Option<PathBuf>,
    /// Hex-dump every TX/RX frame (stderr).
    #[arg(long)]
    trace_proto: bool,
    /// Use an in-memory simulated DP100 instead of hardware.
    #[arg(long)]
    simulate: bool,
}

fn init_logging(trace_proto: bool) {
    let mut filter = EnvFilter::try_from_env("DP100_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    if trace_proto {
        filter = filter.add_directive(format!("{PROTO_TARGET}=trace").parse().expect("valid directive"));
    }
    // stderr only: stdout carries the MCP stream in stdio mode.
    tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).with_ansi(false).init();
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => info!("SIGINT received"),
            _ = term.recv() => info!("SIGTERM received"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
        info!("Ctrl-C received");
    }
}

async fn apply_on_exit(devices: &DeviceManager, on_exit: OnExit) {
    if on_exit == OnExit::Keep {
        info!("on-exit=keep: outputs left unchanged");
        return;
    }
    // Switching off is never refused by the limits; defaults are enough.
    let limits = Limits::default();
    for (serial, d) in devices.opened() {
        let off = d.set_output(OutputChange { enable: Some(false), ..Default::default() }, &limits);
        match tokio::time::timeout(Duration::from_secs(3), off).await {
            Ok(Ok(w)) => {
                info!("on-exit=off: {serial} output is {}", if w.after.output_on { "still ON!" } else { "OFF" })
            }
            Ok(Err(e)) => error!("on-exit=off: failed to switch {serial} off: {e}"),
            Err(_) => error!("on-exit=off: timeout switching {serial} off"),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    init_logging(args.trace_proto);

    let file = match &args.config {
        Some(p) => FileConfig::load(p).map_err(anyhow::Error::msg)?,
        None => FileConfig::default(),
    };
    let flags = LimitsConfig {
        max_voltage_v: args.max_voltage,
        max_current_a: args.max_current,
        vin_dropout_mv: args.vin_dropout_mv,
    };
    let limits = file.limits.clone().merged(flags).resolve().map_err(anyhow::Error::msg).context("invalid limits")?;
    let on_exit = match (args.on_exit, file.server.on_exit.as_deref()) {
        (Some(o), _) => o,
        (None, None) => OnExit::Keep,
        (None, Some(s)) => {
            OnExit::from_str(s, true).map_err(|_| anyhow::anyhow!("server.on_exit must be keep or off, got {s:?}"))?
        }
    };
    let http = match (&args.http, &file.server.http) {
        (Some(a), _) => Some(*a),
        (None, Some(s)) => Some(s.parse().with_context(|| format!("server.http {s:?}"))?),
        (None, None) => None,
    };
    let csv = match (&args.csv_dir, http.is_some()) {
        (Some(dir), _) => {
            if !dir.is_dir() {
                bail!("--csv-dir {} is not a directory", dir.display());
            }
            CsvPolicy::Under(dir.canonicalize()?)
        }
        (None, false) => CsvPolicy::Anywhere,
        (None, true) => CsvPolicy::Disabled,
    };

    let selector = args.serial.clone().or(file.device.serial.clone());
    let devices = Arc::new(if args.simulate {
        warn!("SIMULATION MODE: no hardware is driven");
        DeviceManager::sim(vec![SimDevice::new("SIM0001")], ActorOptions::default())
    } else {
        DeviceManager::hid(selector, ActorOptions::default())
    });
    let l = LimitsReport::from(&limits);
    info!(
        "limits: max {} V / {} A, Vin dropout {} V; on-exit {:?}",
        l.max_voltage_v, l.max_current_a, l.vin_dropout_v, on_exit
    );
    let server = Dp100Server::new(devices.clone(), limits, csv);

    match http {
        None => {
            info!("serving MCP over stdio");
            let service = server.serve(stdio()).await.context("starting the stdio MCP service")?;
            let ct = service.cancellation_token();
            let waiting = service.waiting();
            tokio::pin!(waiting);
            tokio::select! {
                r = &mut waiting => info!("client disconnected ({r:?})"),
                _ = shutdown_signal() => {
                    ct.cancel();
                    let _ = waiting.await;
                }
            }
        }
        Some(addr) => {
            let ct = CancellationToken::new();
            let mut hosts: Vec<String> = vec!["localhost".into(), "127.0.0.1".into(), "::1".into()];
            hosts.extend(file.server.allowed_hosts.iter().cloned());
            hosts.extend(args.allowed_hosts.iter().cloned());
            let config = StreamableHttpServerConfig::default()
                .with_allowed_hosts(hosts)
                .with_cancellation_token(ct.child_token());
            let factory_server = server.clone();
            let service: StreamableHttpService<Dp100Server, LocalSessionManager> =
                StreamableHttpService::new(move || Ok(factory_server.clone()), Default::default(), config);
            let app = axum::Router::new().nest_service("/mcp", service);
            let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("binding {addr}"))?;
            if !addr.ip().is_loopback() {
                warn!("listening on a non-loopback address {addr}: anyone who can reach it can drive the power supply");
            }
            info!("serving MCP over Streamable HTTP at http://{addr}/mcp");
            let shutdown = ct.clone();
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    shutdown_signal().await;
                    shutdown.cancel();
                })
                .await?;
        }
    }

    apply_on_exit(&devices, on_exit).await;
    Ok(())
}
