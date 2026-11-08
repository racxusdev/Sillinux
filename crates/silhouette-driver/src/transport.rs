//! Transport abstraction, query helpers and a mock transport for tests.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::command;
use crate::error::Error;
use crate::job::Job;

/// A bidirectional byte transport to the cutter.
pub trait Transport {
    /// Write one command. Implementations must send `bytes` as a single
    /// transfer (do not split or concatenate commands).
    fn write(&mut self, bytes: &[u8]) -> Result<(), Error>;

    /// Read up to `buf.len()` bytes, waiting at most `timeout_ms`.
    fn read(&mut self, buf: &mut [u8], timeout_ms: u32) -> Result<usize, Error>;
}

impl<T: Transport + ?Sized> Transport for &mut T {
    fn write(&mut self, bytes: &[u8]) -> Result<(), Error> {
        (**self).write(bytes)
    }

    fn read(&mut self, buf: &mut [u8], timeout_ms: u32) -> Result<usize, Error> {
        (**self).read(buf, timeout_ms)
    }
}

impl<T: Transport + ?Sized> Transport for Box<T> {
    fn write(&mut self, bytes: &[u8]) -> Result<(), Error> {
        (**self).write(bytes)
    }

    fn read(&mut self, buf: &mut [u8], timeout_ms: u32) -> Result<usize, Error> {
        (**self).read(buf, timeout_ms)
    }
}

/// Device status reported by `ESC + ENQ`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ready,
    Moving,
    /// The reference documents this as "media unloaded". Note that a Cameo 5
    /// reported `2` while successfully cutting, so this is an indicator, not a
    /// gate. See `re_session.md`, Round 6.
    Unloaded,
    Paused,
    Cancelled,
    Unknown(u8),
}

impl Status {
    pub fn from_byte(b: u8) -> Self {
        match b {
            b'0' => Status::Ready,
            b'1' => Status::Moving,
            b'2' => Status::Unloaded,
            b'3' => Status::Paused,
            b'4' => Status::Cancelled,
            other => Status::Unknown(other),
        }
    }
}

/// Parameters for an automatic registration-mark scan (`TB123`).
///
/// See the public inkscape-silhouette implementation: the distances are those
/// between the marks' outer corners, and `top`/`left` start 10 mm before them.
/// All values are SU (`1 SU = 0.05 mm`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegMarkScan {
    pub arm_su: i32,
    pub stroke_su: i32,
    pub height_su: i32,
    pub width_su: i32,
    pub top_su: i32,
    pub left_su: i32,
    pub quad: bool,
}

impl RegMarkScan {
    /// The command sequence, one command per USB write.
    #[must_use]
    pub fn commands(&self) -> Vec<Vec<u8>> {
        let search = if self.quad {
            command::tb124(self.height_su, self.width_su, self.top_su, self.left_su)
        } else {
            command::tb123(self.height_su, self.width_su, self.top_su, self.left_su)
        };
        vec![
            command::tb50(0),
            command::tb99(),
            command::tb52(2),
            command::tb51(self.arm_su),
            command::tb53(self.stroke_su),
            command::tb55(1),
            search,
        ]
    }
}

/// A transport plus the framing rules of the device.
pub struct Session<T: Transport> {
    transport: T,
}

impl<T: Transport> Session<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Send `ESC + EOT`. The device answers silently.
    pub fn initialize(&mut self) -> Result<(), Error> {
        self.transport.write(&command::init())
    }

    /// Write one command and read until ETX.
    pub fn query(&mut self, cmd: &[u8], timeout_ms: u32) -> Result<Vec<u8>, Error> {
        self.transport.write(cmd)?;
        self.read_frame(timeout_ms)
    }

    /// Read one ETX-terminated response frame, without the ETX byte.
    fn read_frame(&mut self, timeout_ms: u32) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        let mut buf = [0u8; 64];

        for _ in 0..64 {
            let n = self.transport.read(&mut buf, timeout_ms)?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
            if let Some(pos) = out.iter().position(|&b| b == command::ETX) {
                out.truncate(pos);
                return Ok(out);
            }
        }

        Err(Error::Protocol(
            "response was not terminated by ETX".to_string(),
        ))
    }

    /// Firmware string, e.g. `"CAMEO 5 V1.07"`.
    pub fn firmware(&mut self) -> Result<String, Error> {
        let raw = self.query(&command::firmware_query(), 1000)?;
        Ok(String::from_utf8_lossy(&raw).trim().to_string())
    }

    /// Current status.
    ///
    /// The reply is space-padded (`"    0"`), so the digit is located instead of
    /// reading the first byte. Reading byte 0 used to report `Unknown(0x20)`
    /// for every status, which made [`Session::wait_idle`] return immediately
    /// while the carriage was still moving.
    pub fn status(&mut self) -> Result<Status, Error> {
        let raw = self.query(&command::status_query(), 1000)?;
        raw.iter()
            .copied()
            .find(|b| b.is_ascii_digit())
            .map(Status::from_byte)
            .ok_or_else(|| Error::Protocol(format!("unexpected status reply {raw:?}")))
    }

    /// Tool codes for holder 1 and holder 2, e.g. `(2, 5)`.
    ///
    /// The raw reply looks like `" 2, 5"` (leading space, space after comma).
    pub fn tools(&mut self) -> Result<(u8, u8), Error> {
        let raw = self.query(&command::tools_query(), 1000)?;
        let text = String::from_utf8_lossy(&raw);
        let mut parts = text.split(',');
        let a = parts
            .next()
            .and_then(|s| s.trim().parse::<u8>().ok())
            .ok_or_else(|| Error::Protocol(format!("bad tool response {text:?}")))?;
        let b = parts
            .next()
            .and_then(|s| s.trim().parse::<u8>().ok())
            .ok_or_else(|| Error::Protocol(format!("bad tool response {text:?}")))?;
        Ok((a, b))
    }

    /// Send every command in the job, **one USB write per command**.
    pub fn run(&mut self, job: &Job) -> Result<(), Error> {
        for cmd in job.commands() {
            self.transport.write(cmd)?;
        }
        Ok(())
    }

    /// Send a job with a short pause every 16 commands.
    ///
    /// The Cameo's command buffer is small. A long path sent as one burst can
    /// be truncated after the first segments, which looks like "only one
    /// element was cut". The pause lets the motion queue drain without relying
    /// on the status byte: on some firmware that byte reports the media sensor,
    /// not motion.
    pub fn run_paced(&mut self, job: &Job) -> Result<(), Error> {
        const CHUNK: usize = 16;
        const DELAY_MS: u64 = 40;
        for (index, cmd) in job.commands().iter().enumerate() {
            self.transport.write(cmd)?;
            if (index + 1) % CHUNK == 0 {
                std::thread::sleep(Duration::from_millis(DELAY_MS));
            }
        }
        Ok(())
    }

    /// Run an automatic registration-mark scan and wait for the machine.
    ///
    /// The sequence mirrors the public inkscape-silhouette driver:
    /// `TB50,0`, `TB99`, `TB52,2`, `TB51,<arm>`, `TB53,<stroke>`, `TB55,1`,
    /// `TB123,<h>,<w>,<top>,<left>`. On success the machine redefines its origin
    /// at the top-left mark, so subsequent cut coordinates must be relative to
    /// that mark.
    ///
    /// The response is one ETX-terminated frame; the first field must be `0`.
    ///
    /// **UNVERIFIED on the Cameo 5**: the scan moves the optical sensor.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Timeout`] if the machine does not answer within
    /// `timeout_ms`, or [`Error::Protocol`] if it reports a failed search.
    pub fn scan_regmarks(&mut self, scan: &RegMarkScan, timeout_ms: u32) -> Result<(), Error> {
        self.scan_regmarks_reply(scan, timeout_ms).map(|_| ())
    }

    /// Like [`Session::scan_regmarks`], but returns the raw reply text.
    ///
    /// The first field is `0` on success. Some firmware sends two or three
    /// frames for one `TB123` (for example `0,0`, then `0`, then `1`); the
    /// extras are drained here so they cannot be mistaken for status replies
    /// by the next [`Session::wait_idle`] or `ESC+ENQ` query.
    pub fn scan_regmarks_reply(
        &mut self,
        scan: &RegMarkScan,
        timeout_ms: u32,
    ) -> Result<String, Error> {
        for cmd in scan.commands() {
            self.transport.write(&cmd)?;
        }
        let raw = self.read_frame(timeout_ms)?;
        let text = String::from_utf8_lossy(&raw).trim().to_string();
        let first = text.split(',').next().unwrap_or("").trim();
        if first != "0" {
            return Err(Error::Protocol(format!(
                "registration marks not found (reply {text:?})"
            )));
        }
        let mut reply = text;
        for _ in 0..3 {
            match self.read_frame(400) {
                Ok(raw) => {
                    let extra = String::from_utf8_lossy(&raw).trim().to_string();
                    if !extra.is_empty() {
                        reply.push_str(" | ");
                        reply.push_str(&extra);
                    }
                }
                Err(_) => break,
            }
        }
        Ok(reply)
    }

    /// Wait until the machine stops reporting `Moving`.
    ///
    /// `Unloaded` is treated as idle: the media sensor does not gate motion on
    /// the Cameo 5. Polls every 100 ms and fails after `timeout_ms`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Timeout`] if the machine stays busy for `timeout_ms`, or
    /// a transport/protocol error from the status query.
    pub fn wait_idle(&mut self, timeout_ms: u32) -> Result<(), Error> {
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
        loop {
            match self.status() {
                Ok(Status::Moving) => {}
                Ok(_) => return Ok(()),
                Err(Error::Timeout) => {}
                Err(e) => return Err(e),
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Send the job, polling status after every `every` commands.
    ///
    /// Polling mid-job is what the webapp does, but it is not required by the
    /// verified sequence; prefer [`Session::run`] unless you have evidence that
    /// the device's buffer is overflowing.
    pub fn run_polling(&mut self, job: &Job, every: usize) -> Result<(), Error> {
        let every = every.max(1);
        for (i, cmd) in job.commands().iter().enumerate() {
            self.transport.write(cmd)?;
            if (i + 1) % every == 0 {
                let _ = self.status();
            }
        }
        Ok(())
    }
}

/// In-memory transport for tests: records writes and replays queued responses.
#[derive(Debug, Default)]
pub struct MockTransport {
    writes: Vec<Vec<u8>>,
    pending: Vec<u8>,
    responses: VecDeque<Vec<u8>>,
}

impl MockTransport {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a response that the next read will return.
    pub fn queue(&mut self, response: &[u8]) -> &mut Self {
        self.responses.push_back(response.to_vec());
        self
    }

    pub fn writes(&self) -> &[Vec<u8>] {
        &self.writes
    }
}

impl Transport for MockTransport {
    fn write(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.writes.push(bytes.to_vec());
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8], _timeout_ms: u32) -> Result<usize, Error> {
        if self.pending.is_empty() {
            match self.responses.pop_front() {
                Some(r) => self.pending = r,
                None => return Err(Error::Timeout),
            }
        }
        let n = buf.len().min(self.pending.len());
        buf[..n].copy_from_slice(&self.pending[..n]);
        self.pending.drain(..n);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a4_scan() -> RegMarkScan {
        RegMarkScan {
            arm_su: 400,
            stroke_su: 10,
            height_su: 5540,
            width_su: 3800,
            top_su: 0,
            left_su: 0,
            quad: false,
        }
    }

    #[test]
    fn scan_sends_the_reference_sequence_one_write_per_command() {
        let mut session = Session::new(MockTransport::new());
        session.transport_mut().queue(b"    0\x03");
        session.scan_regmarks(&a4_scan(), 1000).unwrap();

        let writes = session.transport().writes();
        assert_eq!(writes.len(), 7);
        assert_eq!(writes[0], b"TB50,0\x03");
        assert_eq!(writes[1], b"TB99\x03");
        assert_eq!(writes[2], b"TB52,2\x03");
        assert_eq!(writes[3], b"TB51,400\x03");
        assert_eq!(writes[4], b"TB53,10\x03");
        assert_eq!(writes[5], b"TB55,1\x03");
        assert_eq!(writes[6], b"TB123,5540,3800,0,0\x03");
    }

    #[test]
    fn scan_accepts_a_comma_separated_reply() {
        let mut session = Session::new(MockTransport::new());
        session.transport_mut().queue(b"    0,    0\x03");
        assert!(session.scan_regmarks(&a4_scan(), 1000).is_ok());
    }

    #[test]
    fn scan_drains_trailing_frames() {
        let mut session = Session::new(MockTransport::new());
        session.transport_mut().queue(b"    0,    0\x03");
        session.transport_mut().queue(b"    0\x03");
        session.transport_mut().queue(b"    1\x03");
        let reply = session.scan_regmarks_reply(&a4_scan(), 1000).unwrap();
        assert!(reply.contains("0,    0"));
        session.transport_mut().queue(b"    0\x03");
        assert_eq!(session.status().unwrap(), Status::Ready);
    }

    #[test]
    fn scan_reports_a_failed_search() {
        let mut session = Session::new(MockTransport::new());
        session.transport_mut().queue(b"    1\x03");
        let err = session.scan_regmarks(&a4_scan(), 1000).unwrap_err();
        assert!(format!("{err}").contains("registration marks not found"));
    }

    #[test]
    fn run_paced_writes_every_command() {
        let mut session = Session::new(MockTransport::new());
        let mut job = Job::new();
        for _ in 0..40 {
            job.push(b"X\x03".to_vec());
        }
        session.run_paced(&job).unwrap();
        assert_eq!(session.transport().writes().len(), 40);
    }

    #[test]
    fn wait_idle_returns_on_ready() {
        let mut session = Session::new(MockTransport::new());
        session.transport_mut().queue(b"0\x03");
        assert!(session.wait_idle(1000).is_ok());
    }

    #[test]
    fn padded_status_reply_is_parsed() {
        let mut session = Session::new(MockTransport::new());
        session.transport_mut().queue(b"    1\x03");
        assert_eq!(session.status().unwrap(), Status::Moving);
        session.transport_mut().queue(b"    0\x03");
        assert_eq!(session.status().unwrap(), Status::Ready);
    }

    #[test]
    fn wait_idle_times_out_while_moving() {
        let mut session = Session::new(MockTransport::new());
        for _ in 0..4 {
            session.transport_mut().queue(b"1\x03");
        }
        assert!(session.wait_idle(150).is_err());
    }
}
