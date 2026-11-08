//! Bluetooth SPP transport over a direct RFCOMM socket (Linux only).
//!
//! A plain `AF_BLUETOOTH`/`BTPROTO_RFCOMM` socket needs no privileges, so this
//! replaces the old `pkexec rfcomm bind` + `/dev/rfcommN` flow: no root, no
//! `rfcomm` binary and no polkit agent — which is what an AppImage needs, since
//! it runs as the unprivileged desktop user.
//!
//! The machine exposes the serial profile on channel 1 (the same channel the
//! old `rfcomm bind … 1` used). The device must already be paired/trusted; the
//! GUI does that with `bluetoothctl` before opening the transport.

use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd};

use crate::error::Error;
use crate::transport::Transport;

/// RFCOMM channel of the Silhouette serial profile.
const M2_CHANNEL: u8 = 1;

// libc 0.2 has no `BTPROTO_RFCOMM`/`sockaddr_rc`; the ABI is fixed by
// <linux/rfcomm.h>, so it is defined here instead of pinning a libc fork.
const BTPROTO_RFCOMM: libc::c_int = 3;

/// `struct sockaddr_rc` from `<linux/rfcomm.h>`.
#[repr(C)]
struct SockaddrRc {
    rc_family: libc::sa_family_t,
    rc_bdaddr: [u8; 6],
    rc_channel: u8,
}

pub struct BtRfcommTransport {
    file: File,
    peer: String,
}

impl BtRfcommTransport {
    /// Connect to a Silhouette over Bluetooth SPP (channel 1).
    pub fn connect_m2(mac: &str) -> Result<Self, Error> {
        Self::connect(mac, M2_CHANNEL)
    }

    /// Connect to `mac` on RFCOMM `channel`.
    pub fn connect(mac: &str, channel: u8) -> Result<Self, Error> {
        let bdaddr = parse_bdaddr(mac)?;

        // SAFETY: plain libc socket setup. On any failure the fd is closed
        // before returning; on success the fd immediately becomes a `File`,
        // which owns it from then on.
        let fd = unsafe { libc::socket(libc::AF_BLUETOOTH, libc::SOCK_STREAM, BTPROTO_RFCOMM) };
        if fd < 0 {
            return Err(Error::Usb(format!(
                "bluetooth {mac}: socket RFCOMM: {}",
                std::io::Error::last_os_error()
            )));
        }

        let addr = SockaddrRc {
            rc_family: libc::AF_BLUETOOTH as libc::sa_family_t,
            rc_bdaddr: bdaddr,
            rc_channel: channel,
        };
        let rc = unsafe {
            libc::connect(
                fd,
                std::ptr::from_ref(&addr).cast::<libc::sockaddr>(),
                std::mem::size_of::<SockaddrRc>() as libc::socklen_t,
            )
        };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            // SAFETY: `fd` is ours and nothing else holds it.
            unsafe { libc::close(fd) };
            return Err(Error::Usb(format!("bluetooth {mac}: conectar: {err}")));
        }

        // SAFETY: `fd` is an open, connected socket; `File` takes ownership
        // and closes it on drop.
        let file = unsafe { File::from_raw_fd(fd) };
        Ok(Self {
            file,
            peer: format!("{mac}/{channel}"),
        })
    }

    /// `MAC/channel` of the connected device (used in error messages).
    pub fn peer(&self) -> &str {
        &self.peer
    }
}

/// `"AA:BB:CC:DD:EE:FF"` → bdaddr bytes. Bluetooth addresses are
/// little-endian, so the octets are reversed.
fn parse_bdaddr(mac: &str) -> Result<[u8; 6], Error> {
    let invalid = || Error::Usb(format!("endereço Bluetooth inválido: {mac}"));
    let parts: Vec<&str> = mac.split(':').collect();
    if parts.len() != 6 {
        return Err(invalid());
    }
    let mut octets = [0u8; 6];
    for (i, part) in parts.iter().enumerate() {
        if part.len() != 2 || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid());
        }
        octets[i] = u8::from_str_radix(part, 16).map_err(|_| invalid())?;
    }
    octets.reverse();
    Ok(octets)
}

impl Transport for BtRfcommTransport {
    fn write(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.file
            .write_all(bytes)
            .map_err(|e| Error::Usb(format!("bluetooth {}: {e}", self.peer)))
    }

    fn read(&mut self, buf: &mut [u8], timeout_ms: u32) -> Result<usize, Error> {
        // `SO_RCVTIMEO` applies to the next read on this socket. A 0 ms budget
        // would mean "return immediately"; polling loops expect a small window
        // instead, so treat 0 like the serial transport's 100 ms tick.
        let timeout_ms = if timeout_ms == 0 { 100 } else { timeout_ms };
        let tv = libc::timeval {
            tv_sec: libc::time_t::from(timeout_ms / 1000),
            tv_usec: libc::suseconds_t::from((timeout_ms % 1000) * 1000),
        };
        let fd = self.file.as_raw_fd();
        // SAFETY: `file` owns a valid socket fd; the option only affects the
        // receive timeout of the read below.
        let rc = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                std::ptr::from_ref(&tv).cast::<libc::c_void>(),
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            )
        };
        if rc != 0 {
            return Err(Error::Usb(format!(
                "bluetooth {}: timeout: {}",
                self.peer,
                std::io::Error::last_os_error()
            )));
        }
        match self.file.read(buf) {
            Ok(n) => Ok(n),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                Err(Error::Timeout)
            }
            Err(e) => Err(Error::Usb(format!("bluetooth {}: {e}", self.peer))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bdaddr_octets_are_reversed() {
        assert_eq!(
            parse_bdaddr("AA:BB:CC:DD:EE:FF").unwrap(),
            [0xFF, 0xEE, 0xDD, 0xCC, 0xBB, 0xAA]
        );
    }

    #[test]
    fn bdaddr_rejects_bad_input() {
        assert!(parse_bdaddr("AABBCCDDEEFF").is_err());
        assert!(parse_bdaddr("AA:BB:CC:DD:EE").is_err());
        assert!(parse_bdaddr("AA:BB:CC:DD:EE:ZZ").is_err());
    }
}
