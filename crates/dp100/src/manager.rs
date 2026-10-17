//! Keeps one [`Dp100`] actor per physical unit, keyed by USB serial.

use std::{collections::BTreeMap, sync::Mutex};

use crate::{
    ActorOptions, Dp100, Error, Result,
    sim::SimDevice,
    transport::{DeviceDescriptor, enumerate},
};

#[derive(Debug)]
enum Backend {
    Hid,
    Sim(Vec<SimDevice>),
}

/// Resolves device selectors to actors, opening them lazily.
#[derive(Debug)]
pub struct DeviceManager {
    backend: Backend,
    default_selector: Option<String>,
    opts: ActorOptions,
    handles: Mutex<BTreeMap<String, Dp100>>,
}

impl DeviceManager {
    /// Real hardware. `default_selector` (USB serial or hidraw path) is used
    /// when a call does not name a device.
    pub fn hid(default_selector: Option<String>, opts: ActorOptions) -> Self {
        Self { backend: Backend::Hid, default_selector, opts, handles: Mutex::default() }
    }

    /// Simulated devices (tests, `--simulate`).
    pub fn sim(devices: Vec<SimDevice>, opts: ActorOptions) -> Self {
        Self { backend: Backend::Sim(devices), default_selector: None, opts, handles: Mutex::default() }
    }

    /// Units currently present.
    pub fn list(&self) -> Result<Vec<DeviceDescriptor>> {
        match &self.backend {
            Backend::Hid => enumerate(),
            Backend::Sim(devs) => Ok(devs
                .iter()
                .filter(|d| d.with(|s| s.plugged))
                .map(|d| DeviceDescriptor {
                    usb_serial: Some(d.usb_serial.clone()),
                    path: format!("sim:{}", d.usb_serial),
                    product: Some("ATK-MDP100 (simulated)".into()),
                })
                .collect()),
        }
    }

    /// Actor for `selector` (USB serial / path), or for the only unit present.
    pub fn get(&self, selector: Option<&str>) -> Result<(String, Dp100)> {
        let key = match selector.map(str::to_owned).or_else(|| self.default_selector.clone()) {
            Some(k) => k,
            None => {
                let present = self.list()?;
                match present.as_slice() {
                    [] => return Err(Error::NotFound(None)),
                    [one] => one.key(),
                    many => return Err(Error::Ambiguous(many.iter().map(DeviceDescriptor::key).collect())),
                }
            }
        };
        let mut handles = self.handles.lock().expect("device map poisoned");
        if let Some(h) = handles.get(&key) {
            return Ok((key, h.clone()));
        }
        let h = match &self.backend {
            Backend::Hid => Dp100::open_hid(Some(key.clone()), self.opts),
            Backend::Sim(devs) => {
                let d = devs.iter().find(|d| d.usb_serial == key).ok_or_else(|| Error::NotFound(Some(key.clone())))?;
                Dp100::spawn(d.clone(), self.opts)
            }
        };
        handles.insert(key.clone(), h.clone());
        Ok((key, h))
    }

    /// Every actor opened so far (for shutdown handling).
    pub fn opened(&self) -> Vec<(String, Dp100)> {
        self.handles.lock().expect("device map poisoned").iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }
}
