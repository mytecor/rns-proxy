//! Frame protocol — wire-compatible with the Python implementation.
//!
//! All TCP sessions are multiplexed over a single RNS link. Each session gets
//! a unique `session_id`. Data is exchanged as frames:
//!
//! ```text
//! [1 byte type][4 bytes session_id][2 bytes payload length][payload]
//! ```

use std::fmt;

// ---------------------------------------------------------------------------
// Frame types and constants
// ---------------------------------------------------------------------------

/// Frame header size: 1 (type) + 4 (session_id) + 2 (payload length) = 7
pub const HDR_SIZE: usize = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
    /// Client requests to open a connection to host:port
    Connect = 0x01,
    /// Server confirms the connection succeeded
    ConnectOk = 0x02,
    /// Server reports a connection error (payload = UTF-8 reason)
    ConnectErr = 0x03,
    /// Bidirectional data transfer
    Data = 0x04,
    /// Either side closes the session
    Close = 0x05,
}

impl FrameType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(Self::Connect),
            0x02 => Some(Self::ConnectOk),
            0x03 => Some(Self::ConnectErr),
            0x04 => Some(Self::Data),
            0x05 => Some(Self::Close),
            _ => None,
        }
    }
}

impl fmt::Display for FrameType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect => write!(f, "CONNECT"),
            Self::ConnectOk => write!(f, "CONN_OK"),
            Self::ConnectErr => write!(f, "CONN_ERR"),
            Self::Data => write!(f, "DATA"),
            Self::Close => write!(f, "CLOSE"),
        }
    }
}

// ---------------------------------------------------------------------------
// Frame encode / decode
// ---------------------------------------------------------------------------

/// A multiplexed frame on the wire.
#[derive(Debug, Clone)]
pub struct Frame {
    pub frame_type: FrameType,
    pub session_id: u32,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(frame_type: FrameType, session_id: u32, payload: Vec<u8>) -> Self {
        Self {
            frame_type,
            session_id,
            payload,
        }
    }

    /// Encode a frame into bytes (wire format).
    pub fn encode(&self) -> Vec<u8> {
        let len = self.payload.len() as u16;
        let mut buf = Vec::with_capacity(HDR_SIZE + self.payload.len());
        buf.push(self.frame_type as u8);
        buf.extend_from_slice(&self.session_id.to_be_bytes());
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&self.payload);
        buf
    }

    /// Decode a frame from a complete buffer (header + payload).
    /// Returns `None` if the buffer is too small or the type is unknown.
    pub fn decode(buf: &[u8]) -> Option<(Self, usize)> {
        if buf.len() < HDR_SIZE {
            return None;
        }
        let frame_type = FrameType::from_u8(buf[0])?;
        let session_id = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
        let payload_len = u16::from_be_bytes([buf[5], buf[6]]) as usize;
        let total = HDR_SIZE + payload_len;
        if buf.len() < total {
            return None;
        }
        let payload = buf[HDR_SIZE..total].to_vec();
        Some((
            Self {
                frame_type,
                session_id,
                payload,
            },
            total,
        ))
    }
}

// ---------------------------------------------------------------------------
// CONNECT payload helpers
// ---------------------------------------------------------------------------

/// Build a CONNECT frame payload: `[1 byte host_len][host bytes][2 bytes port BE]`
pub fn encode_connect_payload(host: &str, port: u16) -> Vec<u8> {
    let h = host.as_bytes();
    let mut buf = Vec::with_capacity(1 + h.len() + 2);
    buf.push(h.len() as u8);
    buf.extend_from_slice(h);
    buf.extend_from_slice(&port.to_be_bytes());
    buf
}

/// Parse a CONNECT frame payload. Returns `(host, port)`.
pub fn decode_connect_payload(data: &[u8]) -> Option<(String, u16)> {
    if data.is_empty() {
        return None;
    }
    let n = data[0] as usize;
    if data.len() < 1 + n + 2 {
        return None;
    }
    let host = String::from_utf8(data[1..1 + n].to_vec()).ok()?;
    let port = u16::from_be_bytes([data[1 + n], data[2 + n]]);
    Some((host, port))
}
