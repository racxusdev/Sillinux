use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Error, Transport};

pub struct SerialTransport {
    file: File,
    path: PathBuf,
}

impl SerialTransport {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let status = Command::new("stty")
            .arg("-F")
            .arg(path)
            .args(["raw", "-echo", "min", "0", "time", "1"])
            .status()
            .map_err(|e| Error::Usb(format!("stty: {e}")))?;
        if !status.success() {
            return Err(Error::Usb(format!(
                "não foi possível preparar {} (rfcomm ligado?)",
                path.display()
            )));
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| Error::Usb(format!("{}: {e}", path.display())))?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Transport for SerialTransport {
    fn write(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.file
            .write_all(bytes)
            .map_err(|e| Error::Usb(format!("{}: {e}", self.path.display())))
    }

    fn read(&mut self, buf: &mut [u8], _timeout_ms: u32) -> Result<usize, Error> {
        match self.file.read(buf) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Err(Error::Timeout),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Err(Error::Timeout),
            Err(e) => Err(Error::Usb(format!("{}: {e}", self.path.display()))),
        }
    }
}
