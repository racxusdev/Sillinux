//! libusb-backed transport (feature `usb`).
//!
//! On Linux the `usblp` kernel driver binds interface 0 of the Cameo. This
//! transport detaches it, claims the interface, and re-attaches it on drop —
//! the same sequence that was verified with PyUSB.
//!
//! Writing while the device is executing a long move can block; the default
//! write timeout is therefore 30 s. An aborted transfer can leave the endpoint
//! wedged until [`UsbTransport::reset`].

use std::time::Duration;

use rusb::{Context, DeviceHandle, UsbContext};

use crate::{Error, Transport, CAMEO5_PID, CAMEO5_VID, EP_IN, EP_OUT};

/// Map a `rusb` error, translating "Access denied" into the fix the user
/// actually needs: an AppImage cannot grant itself device permissions — the
/// udev rule from `packaging/udev/` must be installed on the host.
fn open_err(context: &str, e: rusb::Error) -> Error {
    match e {
        rusb::Error::Access => Error::Usb(format!(
            "{context}: acesso negado — instale a regra udev do projeto \
             (packaging/udev/99-silhouette.rules) e religue o cabo USB"
        )),
        other => Error::Usb(format!("{context}: {other}")),
    }
}

pub struct UsbTransport {
    handle: DeviceHandle<Context>,
    interface: u8,
    detached: bool,
    write_timeout_ms: u64,
}

impl UsbTransport {
    /// Open the first device with the given VID/PID and claim interface 0.
    pub fn open(vendor: u16, product: u16) -> Result<Self, Error> {
        let context = Context::new().map_err(|e| Error::Usb(e.to_string()))?;
        let devices = context.devices().map_err(|e| Error::Usb(e.to_string()))?;

        let mut found = None;
        for device in devices.iter() {
            let desc = device
                .device_descriptor()
                .map_err(|e| Error::Usb(e.to_string()))?;
            if desc.vendor_id() == vendor && desc.product_id() == product {
                found = Some(device);
                break;
            }
        }

        let device = found.ok_or(Error::DeviceNotFound { vendor, product })?;
        Self::open_device(device)
    }

    fn open_device(device: rusb::Device<Context>) -> Result<Self, Error> {
        let handle = device.open().map_err(|e| open_err("abrir dispositivo", e))?;

        let detached = handle.kernel_driver_active(0).unwrap_or(false);
        if detached {
            handle
                .detach_kernel_driver(0)
                .map_err(|e| Error::Usb(format!("detach kernel driver: {e}")))?;
        }
        handle
            .claim_interface(0)
            .map_err(|e| open_err("claim interface 0", e))?;

        Ok(Self {
            handle,
            interface: 0,
            detached,
            write_timeout_ms: 30_000,
        })
    }

    /// Open a Cameo 5 (`0b4d:1140`) by USB bus and address.
    pub fn open_at(bus: u8, address: u8) -> Result<Self, Error> {
        let context = Context::new().map_err(|e| Error::Usb(e.to_string()))?;
        let devices = context.devices().map_err(|e| Error::Usb(e.to_string()))?;
        let mut found = None;
        for device in devices.iter() {
            if device.bus_number() == bus && device.address() == address {
                found = Some(device);
                break;
            }
        }
        let device = found.ok_or(Error::Usb(format!(
            "dispositivo USB {bus}:{address} não encontrado"
        )))?;
        Self::open_device(device)
    }

    /// Open a Cameo 5 (`0b4d:1140`).
    pub fn open_cameo5() -> Result<Self, Error> {
        Self::open(CAMEO5_VID, CAMEO5_PID)
    }

    /// Override the bulk-write timeout.
    #[must_use]
    pub fn with_write_timeout_ms(mut self, ms: u64) -> Self {
        self.write_timeout_ms = ms;
        self
    }

    /// USB reset. Recovers a device whose OUT endpoint stopped accepting data.
    pub fn reset(&self) -> Result<(), Error> {
        self.handle.reset().map_err(|e| Error::Usb(e.to_string()))
    }
}

impl Transport for UsbTransport {
    fn write(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let timeout = Duration::from_millis(self.write_timeout_ms);
        match self.handle.write_bulk(EP_OUT, bytes, timeout) {
            Ok(n) if n == bytes.len() => Ok(()),
            Ok(n) => Err(Error::Usb(format!(
                "short write: {n} of {} bytes",
                bytes.len()
            ))),
            Err(rusb::Error::Timeout) => Err(Error::Timeout),
            Err(e) => Err(Error::Usb(e.to_string())),
        }
    }

    fn read(&mut self, buf: &mut [u8], timeout_ms: u32) -> Result<usize, Error> {
        match self
            .handle
            .read_bulk(EP_IN, buf, Duration::from_millis(u64::from(timeout_ms)))
        {
            Ok(n) => Ok(n),
            Err(rusb::Error::Timeout) => Err(Error::Timeout),
            Err(e) => Err(Error::Usb(e.to_string())),
        }
    }
}

impl Drop for UsbTransport {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(self.interface);
        if self.detached {
            let _ = self.handle.attach_kernel_driver(self.interface);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbInfo {
    pub bus: u8,
    pub address: u8,
    pub product: String,
}

pub fn list_cameo5() -> Result<Vec<UsbInfo>, Error> {
    let context = Context::new().map_err(|e| Error::Usb(e.to_string()))?;
    let devices = context.devices().map_err(|e| Error::Usb(e.to_string()))?;
    let mut out = Vec::new();
    for device in devices.iter() {
        let desc = device
            .device_descriptor()
            .map_err(|e| Error::Usb(e.to_string()))?;
        if desc.vendor_id() != CAMEO5_VID || desc.product_id() != CAMEO5_PID {
            continue;
        }
        out.push(UsbInfo {
            bus: device.bus_number(),
            address: device.address(),
            product: "Silhouette Cameo".to_string(),
        });
    }
    out.sort_by_key(|d| (d.bus, d.address));
    Ok(out)
}
