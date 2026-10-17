//! The HID handle lives on a dedicated OS thread; async callers talk to it
//! through a channel. The thread reconnects on demand after a USB unplug.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use dp100_proto::{Frame, Func, Request, Response};
use tokio::sync::oneshot;
use tracing::{debug, trace, warn};

use crate::{
    Error, Result,
    transport::{Connector, Transport},
};

/// Tracing target for hex dumps of every frame (`--trace-proto`).
pub const PROTO_TARGET: &str = "dp100::proto";

/// Timing knobs of the worker.
#[derive(Debug, Clone, Copy)]
pub struct ActorOptions {
    /// Wait for one reply before giving up (or retrying a read).
    pub reply_timeout: Duration,
    /// Extra attempts for read-only requests (writes are never retried).
    pub read_retries: u8,
    /// Upper bound for a whole call as seen by async callers.
    pub call_timeout: Duration,
    /// Quiet time after a write before any request other than telemetry
    /// (0x30). fw 1.5 silently drops a just-acknowledged write if a 0x35
    /// read arrives within ~5–20 ms (docs/PROTOCOL.md §5.3.1).
    pub write_guard: Duration,
}

impl Default for ActorOptions {
    fn default() -> Self {
        Self {
            // Replies arrive in ~2 ms; the vendor DLL waits 50 ms for reads, 200 ms for writes.
            reply_timeout: Duration::from_millis(250),
            read_retries: 2,
            call_timeout: Duration::from_secs(3),
            write_guard: Duration::from_millis(100),
        }
    }
}

struct Job {
    req: Request,
    reply: oneshot::Sender<Result<Response>>,
}

/// Cheap-to-clone async handle to one DP100.
#[derive(Clone)]
pub struct Dp100 {
    tx: mpsc::Sender<Job>,
    connected: Arc<AtomicBool>,
    target: Arc<str>,
    opts: ActorOptions,
    /// Serialises multi-request operations (read-modify-write).
    pub(crate) op_lock: Arc<tokio::sync::Mutex<()>>,
}

impl std::fmt::Debug for Dp100 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dp100").field("target", &self.target).field("connected", &self.is_connected()).finish()
    }
}

impl Dp100 {
    /// Spawn the worker thread. No I/O happens until the first request.
    pub fn spawn(connector: impl Connector, opts: ActorOptions) -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        let connected = Arc::new(AtomicBool::new(false));
        let target: Arc<str> = connector.describe().into();
        let worker = Worker {
            connector: Box::new(connector),
            link: None,
            seq: 0,
            opts,
            connected: connected.clone(),
            last_write: None,
        };
        thread::Builder::new()
            .name(format!("dp100-{target}"))
            .spawn(move || worker.run(rx))
            .expect("spawning the DP100 worker thread");
        Self { tx, connected, target, opts, op_lock: Arc::new(tokio::sync::Mutex::new(())) }
    }

    /// Connect to a real DP100 (`selector`: USB serial or hidraw path).
    pub fn open_hid(selector: Option<String>, opts: ActorOptions) -> Self {
        Self::spawn(crate::transport::HidConnector::new(selector), opts)
    }

    /// Whether the last exchange succeeded.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    /// Send one request and wait for its decoded reply.
    pub async fn request(&self, req: Request) -> Result<Response> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(Job { req, reply }).map_err(|_| Error::WorkerGone)?;
        match tokio::time::timeout(self.opts.call_timeout, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(Error::WorkerGone),
            Err(_) => Err(Error::Timeout { func: req.func(), write: req.is_write() }),
        }
    }
}

struct Worker {
    connector: Box<dyn Connector>,
    link: Option<Box<dyn Transport>>,
    seq: u8,
    opts: ActorOptions,
    connected: Arc<AtomicBool>,
    last_write: Option<Instant>,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(" ")
}

impl Worker {
    fn run(mut self, rx: mpsc::Receiver<Job>) {
        // Ends when every handle has been dropped.
        while let Ok(job) = rx.recv() {
            let res = self.handle(job.req);
            let _ = job.reply.send(res);
        }
        debug!(target: "dp100", "worker for {} exiting", self.connector.describe());
    }

    fn link(&mut self) -> Result<&mut Box<dyn Transport>> {
        if self.link.is_none() {
            let link = self.connector.connect().inspect_err(|_| self.connected.store(false, Ordering::Relaxed))?;
            debug!(target: "dp100", "connected to {}", self.connector.describe());
            self.link = Some(link);
        }
        Ok(self.link.as_mut().expect("link set above"))
    }

    fn drop_link(&mut self, why: &Error) {
        if self.link.take().is_some() {
            warn!(target: "dp100", "lost {}: {why}; will reconnect on next request", self.connector.describe());
        }
        self.connected.store(false, Ordering::Relaxed);
    }

    fn handle(&mut self, req: Request) -> Result<Response> {
        let attempts = if req.is_write() { 1 } else { 1 + u32::from(self.opts.read_retries) };
        let mut last = Error::Timeout { func: req.func(), write: req.is_write() };
        for attempt in 0..attempts {
            match self.transact(req) {
                Ok(r) => {
                    self.connected.store(true, Ordering::Relaxed);
                    return Ok(r);
                }
                Err(e @ (Error::Disconnected(_) | Error::Hid(_))) => {
                    self.drop_link(&e);
                    // A write may or may not have reached the device: never replay it.
                    if req.is_write() {
                        return Err(e);
                    }
                    last = e;
                }
                Err(e @ Error::Timeout { .. }) => {
                    debug!(target: "dp100", "attempt {} for {:?} timed out", attempt + 1, req.func());
                    last = e;
                }
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }

    fn transact(&mut self, req: Request) -> Result<Response> {
        if req.func() != Func::BasicInfo
            && let Some(t) = self.last_write
        {
            let wait = self.opts.write_guard.saturating_sub(t.elapsed());
            if !wait.is_zero() {
                trace!(target: "dp100", "write guard: waiting {} ms before {:?}", wait.as_millis(), req.func());
                thread::sleep(wait);
            }
        }
        self.seq = self.seq.wrapping_add(1);
        let seq = self.seq;
        let frame = req.to_frame(seq)?;
        let report = frame.encode();
        let timeout = self.opts.reply_timeout;
        if req.is_write() {
            // Set before sending (the link borrows self below); the µs difference is irrelevant.
            self.last_write = Some(Instant::now());
        }
        let link = self.link()?;
        link.drain()?;
        trace!(target: PROTO_TARGET, "TX {}", hex(&report[..frame.wire_len()]));
        link.write_report(&report)?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Error::Timeout { func: req.func(), write: req.is_write() });
            }
            let Some(rx) = link.read_report(left)? else { continue };
            let n = usize::from(rx[3]).min(58);
            trace!(target: PROTO_TARGET, "RX {}", hex(&rx[..6 + n]));
            match Frame::decode(&rx) {
                Ok(f) if f.func == req.func() && f.seq == seq => return Ok(req.parse_response(&f)?),
                Ok(f) => debug!(target: "dp100", "ignoring stale frame func={:?} seq={}", f.func, f.seq),
                Err(e) => debug!(target: "dp100", "ignoring undecodable report: {e}"),
            }
        }
    }
}
