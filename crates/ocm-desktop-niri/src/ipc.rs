//! Low-level Niri IPC socket client: one request line → one reply line,
//! and the persistent event-stream read side.
//!
//! Wire format (niri-ipc, verified against niri 26.04): requests are JSON
//! enums on one line (`{"Outputs":null}`); replies are
//! `{"Ok":<payload>}` / `{"Err":"..."}` on one line. After an
//! `{"EventStream":null}` request the reply is followed by an endless
//! stream of JSON event lines.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// Errors of the IPC client.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("ipc i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("ipc protocol error: {0}")]
    Protocol(String),
}

/// A connected Niri IPC socket.
pub struct IpcConnection {
    stream: UnixStream,
}

impl IpcConnection {
    pub fn connect(socket: &Path) -> std::io::Result<Self> {
        let stream = UnixStream::connect(socket)?;
        Ok(Self::from_stream(stream))
    }

    /// Wrap an already-connected stream (tests build providers over
    /// socket pairs so the mapping logic runs without a compositor).
    pub fn from_stream(stream: UnixStream) -> Self {
        // One-shot request/reply round trips must never hang the app if
        // the compositor stalls; the event-stream connection switches to
        // non-blocking after its handshake.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
        Self { stream }
    }

    /// Send one request (JSON without the trailing newline) and read one
    /// reply line. Returns the payload of the `Ok` reply, or a protocol
    /// error carrying the `Err` string.
    pub fn request(&mut self, request: &str) -> Result<serde_json::Value, IpcError> {
        self.stream
            .write_all(request.as_bytes())
            .and_then(|_| self.stream.write_all(b"\n"))
            .and_then(|_| self.stream.flush())?;
        let mut line = String::new();
        read_line(&mut self.stream, &mut line)?;
        let reply: serde_json::Value =
            serde_json::from_str(&line).map_err(|e| IpcError::Protocol(e.to_string()))?;
        match reply {
            serde_json::Value::Object(map) if map.len() == 1 => {
                let (key, value) = map.into_iter().next().expect("checked len");
                match key.as_str() {
                    "Ok" => Ok(value),
                    "Err" => Err(IpcError::Protocol(format!(
                        "niri rejected the request: {}",
                        serde_json_to_string(&value)
                    ))),
                    other => Err(IpcError::Protocol(format!(
                        "unexpected reply key {other:?}"
                    ))),
                }
            }
            other => Err(IpcError::Protocol(format!(
                "unexpected reply shape: {}",
                serde_json_to_string(&other)
            ))),
        }
    }

    /// Duplicate the socket fd for event-loop readiness polling.
    pub fn try_clone(&self) -> std::io::Result<UnixStream> {
        self.stream.try_clone()
    }

    /// Switch to non-blocking mode (event-stream polling).
    pub fn set_nonblocking(&self) -> std::io::Result<()> {
        self.stream.set_nonblocking(true)
    }

    pub fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.stream.as_raw_fd()
    }

    /// Read whatever is available right now into the given line buffer
    /// (non-blocking; `WouldBlock` is not an error). Returns the number
    /// of bytes appended.
    pub fn read_available(&self, line_buf: &mut Vec<u8>) -> Result<usize, IpcError> {
        let mut total = 0;
        let mut chunk = [0u8; 8192];
        loop {
            match (&self.stream).read(&mut chunk) {
                Ok(0) => {
                    // EOF: the compositor closed the stream.
                    if total == 0 {
                        return Err(IpcError::Io(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "niri closed the event stream",
                        )));
                    }
                    break;
                }
                Ok(n) => {
                    line_buf.extend_from_slice(&chunk[..n]);
                    total += n;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(total)
    }

    /// Split complete lines (newline-terminated) out of the byte buffer,
    /// leaving any partial tail for the next read. Lines are lossily
    /// UTF-8-decoded (niri titles can technically carry anything; a bad
    /// byte must not kill the stream).
    pub fn take_lines(&self, line_buf: &mut Vec<u8>) -> Vec<String> {
        let mut lines = Vec::new();
        let mut start = 0;
        for (i, b) in line_buf.iter().enumerate() {
            if *b == b'\n' {
                let text = String::from_utf8_lossy(&line_buf[start..i]);
                if !text.trim().is_empty() {
                    lines.push(text.into_owned());
                }
                start = i + 1;
            }
        }
        line_buf.drain(..start);
        lines
    }
}

/// Read one `\n`-terminated line byte by byte (the stream is line-
/// delimited; niri never sends embedded newlines).
fn read_line(stream: &mut UnixStream, out: &mut String) -> std::io::Result<()> {
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "niri closed the connection mid-reply",
            ));
        }
        if byte[0] == b'\n' {
            return Ok(());
        }
        out.push(byte[0] as char);
    }
}

/// Compact single-line rendering for error messages.
fn serde_json_to_string(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unprintable>".to_string())
}
