//! GPGL command encoding for Silhouette cutters.
//!
//! Every command is ASCII text terminated by `0x03` (ETX), except the two
//! control-byte queries (`ESC+EOT`, `ESC+ENQ`, `ESC+NAK`).
//!
//! # Transport rule (verified on hardware)
//!
//! Send **one command per USB transfer**. Sending five `M` commands as a single
//! 49-byte bulk write moved the carriage only once; the same five as individual
//! writes produced a complete 30 mm square. See `re_session.md`, Round 5.
//!
//! # Units
//!
//! Coordinates are in Silhouette Units (SU). `1 SU = 0.05 mm`, so 20 mm = 400 SU.
//! Coordinates are ordered `(y, x)`, matching the verified job that cut a closed
//! square on the Cameo 5.

// `deny` (not `forbid`): the Linux-only `bt` module needs a small, audited
// `allow` for its plain libc socket sequence — everything else stays unsafe-free.
#![deny(unsafe_code)]

mod command;
mod error;
mod job;
mod serial;
mod transport;

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod bt;

pub use command::*;
pub use error::Error;
pub use job::{
    golden_square_cut, order_for_cut, polyline_area, polyline_epilogue, polyline_job,
    polyline_paths, polyline_prelude, CutMapping, CutSettings, Job,
};
pub use serial::SerialTransport;
pub use transport::{MockTransport, RegMarkScan, Session, Status, Transport};

#[cfg(target_os = "linux")]
pub use bt::BtRfcommTransport;

#[cfg(feature = "usb")]
mod usb;
#[cfg(feature = "usb")]
pub use usb::{list_cameo5, UsbInfo, UsbTransport};

/// Cameo 5 USB identifiers (verified with `lsusb`).
pub const CAMEO5_VID: u16 = 0x0b4d;
pub const CAMEO5_PID: u16 = 0x1140;

/// Bulk endpoints of interface 0 (verified from the descriptors).
pub const EP_OUT: u8 = 0x01;
pub const EP_IN: u8 = 0x82;
