use dp100_proto::{Func, ProtoError, UnitError};

use crate::limits::LimitError;

/// Device-layer error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no DP100 found{}", .0.as_deref().map(|s| format!(" with serial {s}")).unwrap_or_default())]
    NotFound(Option<String>),
    #[error("several DP100 are connected ({}); pass a serial number", .0.join(", "))]
    Ambiguous(Vec<String>),
    #[error("HID error: {0}")]
    Hid(String),
    #[error("device disconnected: {0}")]
    Disconnected(String),
    #[error(
        "timeout waiting for the reply to {func:?}{}",
        if *.write { " (write: the device state is unknown, re-read it)" } else { "" }
    )]
    Timeout { func: Func, write: bool },
    #[error("protocol error: {0}")]
    Proto(ProtoError),
    #[error("refused by safety limits: {0}")]
    Limit(LimitError),
    #[error("invalid value: {0}")]
    Unit(UnitError),
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    #[error("device is in bootloader mode (run_area 0x{0:02X}); refusing to drive it")]
    Bootloader(u16),
    #[error(
        "SAFETY: the device acknowledged OFF but the output is still active (Vout {} V, mode {mode}); \
         switch it off at the front panel",
        .vout.volts()
    )]
    OutputStillOn { vout: dp100_proto::Millivolts, mode: &'static str },
    #[error(
        "SAFETY: Vout {} V stays above the setpoint {} V even after re-sending the write; check the device",
        .vout.volts(), .setpoint.volts()
    )]
    VoutAboveSetpoint { vout: dp100_proto::Millivolts, setpoint: dp100_proto::Millivolts },
    #[error("unexpected response from the device: {0}")]
    UnexpectedResponse(String),
    #[error("device worker thread is gone")]
    WorkerGone,
    #[error("I/O error: {0}")]
    Io(std::io::Error),
}

// Manual `From` impls: with `#[from]` the inner error would also be reported
// as a `source()`, and anyhow would print the message twice.
macro_rules! from {
    ($($t:ty => $v:ident),*) => {$(
        impl From<$t> for Error {
            fn from(e: $t) -> Self {
                Error::$v(e)
            }
        }
    )*};
}
from!(ProtoError => Proto, LimitError => Limit, UnitError => Unit, std::io::Error => Io);

impl From<hidapi::HidError> for Error {
    fn from(e: hidapi::HidError) -> Self {
        Error::Hid(e.to_string())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
