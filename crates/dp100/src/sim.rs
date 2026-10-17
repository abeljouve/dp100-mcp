//! In-memory DP100 simulator speaking the real wire protocol, used by tests
//! and by `--simulate` in the binaries. Its behaviour follows
//! docs/PROTOCOL.md, including unverified assumptions (flagged there).

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use dp100_proto::{Frame, Func, REPORT_LEN};

use crate::{
    Error, Result,
    transport::{Connector, Transport},
};

/// Mutable simulator state, shared with the test that created it.
#[derive(Debug, Clone)]
pub struct SimState {
    pub plugged: bool,
    pub vin_mv: u16,
    /// Resistive load in ohms (`None` = open circuit).
    pub load_ohms: Option<f64>,
    pub active_index: u8,
    /// The active set's state byte (what the device reports).
    pub output_on: bool,
    /// The real output stage. Diverges from `output_on` like fw 1.5 does.
    pub output_active: bool,
    /// Writes are stored and acknowledged but never applied (safety-check tests).
    pub stuck: bool,
    /// Setpoints the output stage actually uses (may lag `live`).
    pub applied: [u16; 4],
    /// A 0x20 write waiting to be applied by the firmware main loop.
    pub pending: Option<(Instant, bool, [u16; 4])>,
    /// A 0x35 read arriving within this window after a write cancels it,
    /// as observed on fw 1.5.
    pub race_window: Duration,
    /// Live setpoints: voltage, current, OVP, OCP (mV / mA).
    pub live: [u16; 4],
    pub presets: [[u16; 4]; 10],
    /// otp °C, opp dW, backlight, volume, reverse protection, auto output.
    pub system: (u16, u16, u8, u8, bool, bool),
    /// Drop the next N replies (simulates lost packets).
    pub drop_replies: u32,
    /// Apply the next write but drop its acknowledgement.
    pub drop_next_write_ack: bool,
    /// Answer the next write with a NAK.
    pub nak_next_write: bool,
    /// Raw payloads of every write received, in order.
    pub writes: Vec<Vec<u8>>,
}

impl Default for SimState {
    fn default() -> Self {
        // Mirrors the unit probed on 2026-10-17.
        let mut presets = [[0u16; 4]; 10];
        presets[0] = [5000, 500, 30500, 5050];
        for (i, p) in presets.iter_mut().enumerate().skip(1) {
            *p = [1000 + 1000 * i as u16, 500 + 500 * i as u16, 30500, 5050];
        }
        Self {
            plugged: true,
            vin_mv: 15223,
            load_ohms: None,
            active_index: 0,
            output_on: false,
            output_active: false,
            stuck: false,
            applied: presets[0],
            pending: None,
            race_window: Duration::from_millis(10),
            live: presets[0],
            presets,
            system: (80, 1050, 2, 2, true, false),
            drop_replies: 0,
            drop_next_write_ack: false,
            nak_next_write: false,
            writes: Vec::new(),
        }
    }
}

impl SimState {
    /// Set both the state byte and the real output.
    pub fn set_on(&mut self, on: bool) {
        self.output_on = on;
        self.output_active = on;
        self.applied = self.live;
        self.pending = None;
    }

    /// Let the simulated main loop apply a pending write once the race window passed.
    fn tick(&mut self) {
        if let Some((at, on, v)) = self.pending
            && at.elapsed() >= self.race_window
            && !self.stuck
        {
            self.pending = None;
            self.output_active = on;
            self.applied = v;
        }
    }

    fn vo_max(&self) -> u16 {
        (u32::from(self.vin_mv) * 97 / 100) as u16
    }

    /// (vout mV, iout mA, out_mode)
    fn output(&self) -> (u16, u16, u8) {
        if !self.output_active {
            return (0, 0, 2);
        }
        let v = self.applied[0].min(self.vo_max());
        match self.load_ohms {
            None => (v, 0, 1),
            Some(r) => {
                let i = f64::from(v) / r;
                if i > f64::from(self.applied[1]) {
                    let ilim = self.applied[1];
                    ((f64::from(ilim) * r) as u16, ilim, 0)
                } else {
                    (v, i as u16, 1)
                }
            }
        }
    }
}

/// Cloneable simulator handle: a [`Connector`] plus access to the state.
#[derive(Debug, Clone, Default)]
pub struct SimDevice {
    pub state: Arc<Mutex<SimState>>,
    pub usb_serial: String,
}

impl SimDevice {
    pub fn new(usb_serial: &str) -> Self {
        Self { state: Arc::default(), usb_serial: usb_serial.into() }
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut SimState) -> R) -> R {
        f(&mut self.state.lock().expect("sim state poisoned"))
    }
}

impl Connector for SimDevice {
    fn connect(&mut self) -> Result<Box<dyn Transport>> {
        if !self.with(|s| s.plugged) {
            return Err(Error::NotFound(Some(self.usb_serial.clone())));
        }
        Ok(Box::new(SimLink { state: self.state.clone(), pending: VecDeque::new() }))
    }

    fn describe(&self) -> String {
        format!("sim:{}", self.usb_serial)
    }
}

struct SimLink {
    state: Arc<Mutex<SimState>>,
    pending: VecDeque<[u8; REPORT_LEN]>,
}

fn le(v: u16) -> [u8; 2] {
    v.to_le_bytes()
}

fn set_record(index: u8, on: bool, v: [u16; 4]) -> Vec<u8> {
    let mut p = vec![index, u8::from(on)];
    for x in v {
        p.extend_from_slice(&le(x));
    }
    p
}

fn parse_set(p: &[u8]) -> [u16; 4] {
    [2, 4, 6, 8].map(|o| u16::from_le_bytes([p[o], p[o + 1]]))
}

const DEVICE_INFO: [u8; 40] = [
    0x41, 0x54, 0x4B, 0x2D, 0x44, 0x50, 0x31, 0x30, 0x30, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x0E, 0x00, 0x0F,
    0x00, 0x0B, 0x00, 0xAA, 0x00, 0x96, 0xE2, 0x99, 0x0E, 0x00, 0x40, 0x75, 0x30, 0x03, 0xA7, 0x4D, 0x05, 0xEA, 0x07,
    0x09, 0x1C,
];

impl SimLink {
    fn respond(s: &mut SimState, req: &Frame) -> Vec<u8> {
        let p = req.payload();
        s.tick();
        match (req.func, p.len()) {
            (Func::DeviceInfo, 0) => DEVICE_INFO.to_vec(),
            (Func::BasicInfo, 0) => {
                let (v, i, mode) = s.output();
                let mut out = Vec::with_capacity(16);
                for x in [s.vin_mv, v, i, s.vo_max(), 284, 282, 5083] {
                    out.extend_from_slice(&le(x));
                }
                out.extend_from_slice(&[mode, 0]);
                out
            }
            (Func::BasicSet, 1) if p[0] & 0x80 != 0 => {
                // The fw 1.5 race: a read right after a write drops that write.
                if s.pending.is_some_and(|(at, _, _)| at.elapsed() < s.race_window) {
                    s.pending = None;
                }
                set_record(s.active_index, s.output_on, s.live)
            }
            (Func::BasicSet, 1) if p[0] <= 9 => {
                let n = p[0];
                set_record(n, false, s.presets[usize::from(n)])
            }
            (Func::BasicSet, 10) => {
                s.writes.push(p.to_vec());
                if std::mem::take(&mut s.nak_next_write) {
                    return vec![0];
                }
                let (flags, n) = (p[0] & 0xF0, p[0] & 0x0F);
                if n > 9 {
                    return vec![0];
                }
                match flags {
                    0x20 => {
                        // Stored (and read back) at once; applied by the main loop
                        // later, unless a 0x35 read cancels it first. Stored presets
                        // are not touched.
                        let new = parse_set(p);
                        s.active_index = n;
                        s.output_on = p[1] != 0;
                        s.live = new;
                        s.pending = Some((Instant::now(), s.output_on, new));
                    }
                    0x40 => s.presets[usize::from(n)] = parse_set(p),
                    0x80 => {
                        s.active_index = n;
                        s.live = s.presets[usize::from(n)];
                        s.applied = s.live;
                    }
                    _ => return vec![0],
                }
                vec![1]
            }
            (Func::SystemInfo, 0) => {
                let (otp, opp, blk, vol, rep, auto) = s.system;
                let mut out = Vec::new();
                out.extend_from_slice(&le(otp));
                out.extend_from_slice(&le(opp));
                out.extend_from_slice(&[blk, vol, u8::from(rep), u8::from(auto)]);
                out
            }
            (Func::SystemInfo, 8) => {
                s.writes.push(p.to_vec());
                if std::mem::take(&mut s.nak_next_write) {
                    return vec![0];
                }
                s.system = (
                    u16::from_le_bytes([p[0], p[1]]),
                    u16::from_le_bytes([p[2], p[3]]),
                    p[4],
                    p[5],
                    p[6] != 0,
                    p[7] != 0,
                );
                vec![1]
            }
            _ => vec![0],
        }
    }
}

impl Transport for SimLink {
    fn write_report(&mut self, report: &[u8; REPORT_LEN]) -> Result<()> {
        let mut s = self.state.lock().expect("sim state poisoned");
        if !s.plugged {
            return Err(Error::Disconnected("simulated unplug".into()));
        }
        let reply = match Frame::decode_from_host(report) {
            Ok(req) => {
                let is_write = matches!((req.func, req.payload().len()), (Func::BasicSet, 10) | (Func::SystemInfo, 8));
                let payload = Self::respond(&mut s, &req);
                if is_write && std::mem::take(&mut s.drop_next_write_ack) {
                    return Ok(());
                }
                Frame::new(req.func, req.seq, &payload).expect("sim payload fits")
            }
            // Bad CRC: NAK with the same function code, as the real device does.
            Err(_) => match Func::try_from(report[1]) {
                Ok(f) => Frame::new(f, report[2], &[0]).expect("fits"),
                Err(_) => return Ok(()),
            },
        };
        if s.drop_replies > 0 {
            s.drop_replies -= 1;
        } else {
            self.pending.push_back(reply.encode_as_device());
        }
        Ok(())
    }

    fn read_report(&mut self, timeout: Duration) -> Result<Option<[u8; REPORT_LEN]>> {
        if !self.state.lock().expect("sim state poisoned").plugged {
            return Err(Error::Disconnected("simulated unplug".into()));
        }
        match self.pending.pop_front() {
            Some(r) => Ok(Some(r)),
            None => {
                std::thread::sleep(timeout.min(Duration::from_millis(2)));
                Ok(None)
            }
        }
    }
}
