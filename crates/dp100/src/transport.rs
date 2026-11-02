//! Byte-level transports. [`HidConnector`] talks to a real DP100 through
//! hidapi (Linux hidraw backend); [`crate::sim`] provides an in-memory one.

use std::time::Duration;

use dp100_proto::{PID, REPORT_LEN, VID};
use hidapi::{HidApi, HidDevice};
use serde::Serialize;

use crate::{Error, Result};

/// A connected link that moves 64-byte reports.
pub trait Transport: Send {
    /// Send one 64-byte output report (without report ID).
    fn write_report(&mut self, report: &[u8; REPORT_LEN]) -> Result<()>;
    /// Wait up to `timeout` for one input report. `Ok(None)` on timeout.
    fn read_report(&mut self, timeout: Duration) -> Result<Option<[u8; REPORT_LEN]>>;
    /// Drop stale input reports.
    fn drain(&mut self) -> Result<()> {
        while self.read_report(Duration::ZERO)?.is_some() {}
        Ok(())
    }
}

/// Opens (and re-opens after unplug) a transport.
pub trait Connector: Send + 'static {
    fn connect(&mut self) -> Result<Box<dyn Transport>>;
    /// Human-readable target, for logs.
    fn describe(&self) -> String;
}

/// A DP100 seen during USB enumeration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct DeviceDescriptor {
    /// USB iSerial string (stable per unit, readable without opening).
    pub usb_serial: Option<String>,
    /// hidraw path.
    pub path: String,
    pub product: Option<String>,
}

impl DeviceDescriptor {
    /// Key used to address the device: USB serial, else hidraw path.
    pub fn key(&self) -> String {
        self.usb_serial.clone().unwrap_or_else(|| self.path.clone())
    }
}

/// List the DP100 units currently plugged in.
pub fn enumerate() -> Result<Vec<DeviceDescriptor>> {
    let api = HidApi::new()?;
    let mut out: Vec<DeviceDescriptor> = api
        .device_list()
        .filter(|d| d.vendor_id() == VID && d.product_id() == PID)
        .map(|d| DeviceDescriptor {
            usb_serial: d.serial_number().filter(|s| !s.is_empty()).map(str::to_owned),
            path: d.path().to_string_lossy().into_owned(),
            product: d.product_string().map(str::to_owned),
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.dedup_by(|a, b| a.path == b.path);
    Ok(out)
}

/// Connects to a DP100 selected by USB serial (or hidraw path), or to the only one present.
#[derive(Debug, Clone)]
pub struct HidConnector {
    selector: Option<String>,
}

impl HidConnector {
    /// `selector`: USB serial or hidraw path; `None` = the single connected unit.
    pub fn new(selector: Option<String>) -> Self {
        Self { selector }
    }
}

impl Connector for HidConnector {
    fn connect(&mut self) -> Result<Box<dyn Transport>> {
        let api = HidApi::new()?;
        let candidates: Vec<_> = api
            .device_list()
            .filter(|d| d.vendor_id() == VID && d.product_id() == PID)
            .filter(|d| match &self.selector {
                None => true,
                Some(sel) => d.serial_number() == Some(sel.as_str()) || d.path().to_string_lossy() == sel.as_str(),
            })
            .collect();
        let info = match candidates.as_slice() {
            [] => return Err(Error::NotFound(self.selector.clone())),
            [one] => *one,
            many => {
                return Err(Error::Ambiguous(
                    many.iter()
                        .map(|d| {
                            d.serial_number().map_or_else(|| d.path().to_string_lossy().into_owned(), str::to_owned)
                        })
                        .collect(),
                ));
            }
        };
        // Exclusive advisory lock on the hidraw node: two processes driving the
        // same unit can cancel each other's writes (docs/PROTOCOL.md §5.3.1).
        let path = info.path().to_string_lossy().into_owned();
        let lock = std::fs::File::open(&path)
            .map_err(|e| Error::Hid(format!("cannot open {path} ({e}); check the udev rule / plugdev membership")))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(Error::Busy(path)),
            Err(std::fs::TryLockError::Error(e)) => return Err(Error::Hid(format!("cannot lock {path}: {e}"))),
        }
        let dev = info.open_device(&api).map_err(|e| {
            Error::Hid(format!(
                "cannot open {} ({e}); check the udev rule / plugdev membership",
                info.path().to_string_lossy()
            ))
        })?;
        Ok(Box::new(HidTransport { dev, _lock: lock }))
    }

    fn describe(&self) -> String {
        self.selector.clone().unwrap_or_else(|| "<single DP100>".into())
    }
}

struct HidTransport {
    dev: HidDevice,
    /// Holds the flock for as long as the link is open.
    _lock: std::fs::File,
}

impl Transport for HidTransport {
    fn write_report(&mut self, report: &[u8; REPORT_LEN]) -> Result<()> {
        // Report ID 0 ("no report ID") first: the kernel strips it.
        let mut buf = [0u8; REPORT_LEN + 1];
        buf[1..].copy_from_slice(report);
        let n = self.dev.write(&buf).map_err(|e| Error::Disconnected(e.to_string()))?;
        if n != buf.len() {
            return Err(Error::Hid(format!("short write: {n}/{} bytes", buf.len())));
        }
        Ok(())
    }

    fn read_report(&mut self, timeout: Duration) -> Result<Option<[u8; REPORT_LEN]>> {
        let mut buf = [0u8; REPORT_LEN];
        let ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        let n = self.dev.read_timeout(&mut buf, ms).map_err(|e| Error::Disconnected(e.to_string()))?;
        Ok((n > 0).then_some(buf))
    }
}
