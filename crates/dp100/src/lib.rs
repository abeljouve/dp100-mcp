//! Async device layer for the ALIENTEK DP100.
//!
//! Each unit is owned by a dedicated worker thread ([`Dp100`]); requests go
//! through a channel with timeouts, and the worker reconnects after a USB
//! unplug. [`DeviceManager`] maps USB serial numbers to workers so several
//! DP100 can be driven at once. Safety checks live in [`limits`].

mod actor;
pub mod config;
mod error;
pub mod limits;
mod manager;
pub mod measure;
mod ops;
pub mod report;
pub mod sim;
pub mod transport;

pub use actor::{ActorOptions, Dp100, PROTO_TARGET};
pub use dp100_proto as proto;
pub use error::{Error, Result};
pub use limits::{LimitError, Limits, LimitsConfig};
pub use manager::DeviceManager;
pub use ops::{OutputChange, PowerCycleOutcome, WriteOutcome, output_active};
pub use transport::{DeviceDescriptor, HidConnector, enumerate};
