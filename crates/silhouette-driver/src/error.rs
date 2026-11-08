use std::fmt;

/// Errors from the driver layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The transport reported an I/O failure.
    Io(String),
    /// The device could not be found.
    DeviceNotFound { vendor: u16, product: u16 },
    /// The device did not answer within the transfer timeout.
    Timeout,
    /// The response did not follow the ETX framing.
    Protocol(String),
    /// A USB operation failed.
    Usb(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(m) => write!(f, "transport I/O error: {m}"),
            Error::DeviceNotFound { vendor, product } => {
                write!(f, "device {vendor:04x}:{product:04x} not found")
            }
            Error::Timeout => write!(f, "device did not respond before the timeout"),
            Error::Protocol(m) => write!(f, "protocol error: {m}"),
            Error::Usb(m) => write!(f, "USB error: {m}"),
        }
    }
}

impl std::error::Error for Error {}
