//! screen-relay's stream protocol (`nsl1`), as far as a viewer needs it.
//!
//! The host never looks inside these messages: it pipes them between the
//! engine's `/stream` WebSocket and a device tunnel. The CLI and the phone
//! decode frames and build ACKs and input records with this module. The wire
//! format is screen-relay's `PROTOCOL.md`: a 20-byte big-endian header, then
//! the payload.

use thiserror::Error;

pub const MAGIC: &[u8; 4] = b"NSL1";
pub const VERSION: u8 = 1;
pub const HEADER_SIZE: usize = 20;
pub const INPUT_SIZE: usize = 16;
/// WebSocket subprotocol the engine speaks.
pub const SUBPROTOCOL: &str = "nsl1";
/// Input coordinates run from 0 to this across the desktop.
pub const COORD_MAX: u16 = 65535;

pub const T_FRAME: u8 = 1;
pub const T_ACK: u8 = 2;
pub const T_INPUT: u8 = 3;
pub const T_CURSOR: u8 = 6;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ScreenError {
    #[error("not a screen-relay message")]
    BadHeader,
    #[error("malformed {0} message")]
    Malformed(&'static str),
}

/// The header fields every message carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub kind: u8,
    pub flags: u16,
    pub generation: u32,
    pub sequence: u32,
}

/// One H.264 access unit (Annex B, with SPS and PPS on keyframes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub generation: u32,
    pub sequence: u32,
    pub keyframe: bool,
    pub width: u16,
    pub height: u16,
    pub capture_us: u64,
    pub data: Vec<u8>,
}

/// A pointer image: straight-alpha RGBA, row by row. Empty restores the arrow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub width: u16,
    pub height: u16,
    pub hot_x: u16,
    pub hot_y: u16,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerMessage {
    Frame(Frame),
    Cursor(Cursor),
    /// A type a viewer does not need (control-session replies).
    Other(Header),
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_be_bytes([b[i], b[i + 1]])
}
fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// Read and check the header of one message.
pub fn header(msg: &[u8]) -> Result<Header, ScreenError> {
    if msg.len() < HEADER_SIZE || &msg[..4] != MAGIC || msg[4] != VERSION {
        return Err(ScreenError::BadHeader);
    }
    let h = Header {
        kind: msg[5],
        flags: u16_at(msg, 6),
        generation: u32_at(msg, 8),
        sequence: u32_at(msg, 12),
    };
    if !(1..=6).contains(&h.kind)
        || h.generation == 0
        || u32_at(msg, 16) as usize != msg.len() - HEADER_SIZE
    {
        return Err(ScreenError::BadHeader);
    }
    Ok(h)
}

/// Decode one message the engine sent on `/stream`.
pub fn decode(msg: &[u8]) -> Result<ServerMessage, ScreenError> {
    let h = header(msg)?;
    let p = &msg[HEADER_SIZE..];
    Ok(match h.kind {
        T_FRAME => {
            if p.len() < 12 || h.flags > 1 {
                return Err(ScreenError::Malformed("frame"));
            }
            ServerMessage::Frame(Frame {
                generation: h.generation,
                sequence: h.sequence,
                keyframe: h.flags & 1 == 1,
                width: u16_at(p, 0),
                height: u16_at(p, 2),
                capture_us: u64::from_be_bytes(p[4..12].try_into().unwrap()),
                data: p[12..].to_vec(),
            })
        }
        T_CURSOR => {
            if p.len() < 8 {
                return Err(ScreenError::Malformed("cursor"));
            }
            let (w, hgt) = (u16_at(p, 0), u16_at(p, 2));
            if p.len() != 8 + w as usize * hgt as usize * 4 {
                return Err(ScreenError::Malformed("cursor"));
            }
            ServerMessage::Cursor(Cursor {
                width: w,
                height: hgt,
                hot_x: u16_at(p, 4),
                hot_y: u16_at(p, 6),
                rgba: p[8..].to_vec(),
            })
        }
        _ => ServerMessage::Other(h),
    })
}

fn envelope(kind: u8, generation: u32, sequence: u32, payload: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(HEADER_SIZE + payload.len());
    b.extend_from_slice(MAGIC);
    b.push(VERSION);
    b.push(kind);
    b.extend_from_slice(&0u16.to_be_bytes());
    b.extend_from_slice(&generation.to_be_bytes());
    b.extend_from_slice(&sequence.to_be_bytes());
    b.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    b.extend_from_slice(payload);
    b
}

/// Acknowledge a frame once it was drawn or dropped for a newer one.
/// `wait_us` is how long it waited in the viewer after it arrived.
pub fn ack(generation: u32, sequence: u32, wait_us: u32) -> Vec<u8> {
    envelope(T_ACK, generation, sequence, &wait_us.to_be_bytes())
}

/// One input event. Coordinates are 0..=65535 across the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// A physical key by HID usage (0..=231).
    Key { hid: u16, down: bool },
    PointerMove { x: u16, y: u16 },
    /// Button 1 left, 2 middle, 3 right.
    Button { button: u16, down: bool, x: u16, y: u16 },
    /// `delta` is a multiple of 120 (one notch), -1200..=1200.
    Wheel { horizontal: bool, delta: i32, x: u16, y: u16 },
    /// Type one character without touching held keys.
    Text(char),
    ReleaseAll,
}

impl Input {
    /// The 16-byte record from `PROTOCOL.md`.
    pub fn record(&self) -> [u8; INPUT_SIZE] {
        let mut r = [0u8; INPUT_SIZE];
        let mut put = |kind: u8, code: u16, value: i32, x: u16, y: u16| {
            r[0] = kind;
            r[2..4].copy_from_slice(&code.to_be_bytes());
            r[4..8].copy_from_slice(&value.to_be_bytes());
            r[8..10].copy_from_slice(&x.to_be_bytes());
            r[10..12].copy_from_slice(&y.to_be_bytes());
        };
        match *self {
            Input::Key { hid, down } => put(if down { 1 } else { 2 }, hid.min(231), 0, 0, 0),
            Input::PointerMove { x, y } => put(3, 0, 0, x, y),
            Input::Button { button, down, x, y } => {
                put(if down { 4 } else { 5 }, button.clamp(1, 3), 0, x, y)
            }
            Input::Wheel { horizontal, delta, x, y } => {
                let notches = (delta / 120).clamp(-10, 10) * 120;
                put(6, horizontal as u16, notches, x, y)
            }
            Input::Text(c) => put(8, 0, c as i32, 0, 0),
            Input::ReleaseAll => put(7, 0, 0, 0, 0),
        }
        r
    }

    /// The whole message for `/stream`, stamped with the stream's generation.
    pub fn message(&self, generation: u32) -> Vec<u8> {
        envelope(T_INPUT, generation, 0, &self.record())
    }
}

/// Split Annex B data into NAL units (without start codes).
pub fn nal_units(annex_b: &[u8]) -> Vec<&[u8]> {
    let mut starts = vec![];
    let mut i = 0;
    while i + 3 <= annex_b.len() {
        if annex_b[i] == 0 && annex_b[i + 1] == 0 && annex_b[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = vec![];
    for (k, &s) in starts.iter().enumerate() {
        let mut end = starts.get(k + 1).map(|n| n - 3).unwrap_or(annex_b.len());
        // A 4-byte start code leaves one zero behind the previous unit.
        while end > s && annex_b[end - 1] == 0 && k + 1 < starts.len() {
            end -= 1;
        }
        if end > s {
            out.push(&annex_b[s..end]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_msg(keyframe: bool, data: &[u8]) -> Vec<u8> {
        let mut p = vec![];
        p.extend_from_slice(&1280u16.to_be_bytes());
        p.extend_from_slice(&720u16.to_be_bytes());
        p.extend_from_slice(&42u64.to_be_bytes());
        p.extend_from_slice(data);
        let mut m = envelope(T_FRAME, 7, 3, &p);
        m[7] = keyframe as u8;
        m
    }

    #[test]
    fn decodes_a_frame() {
        let m = frame_msg(true, &[0, 0, 0, 1, 0x67, 1, 2]);
        let ServerMessage::Frame(f) = decode(&m).unwrap() else { panic!() };
        assert_eq!((f.generation, f.sequence, f.keyframe), (7, 3, true));
        assert_eq!((f.width, f.height, f.capture_us), (1280, 720, 42));
        assert_eq!(f.data, vec![0, 0, 0, 1, 0x67, 1, 2]);
    }

    #[test]
    fn rejects_bad_headers() {
        let mut m = frame_msg(false, &[1]);
        m[0] = b'X';
        assert_eq!(decode(&m), Err(ScreenError::BadHeader));
        let mut m = frame_msg(false, &[1]);
        m.push(0); // length no longer matches
        assert_eq!(decode(&m), Err(ScreenError::BadHeader));
        assert_eq!(decode(&ack(0, 1, 0)), Err(ScreenError::BadHeader));
    }

    #[test]
    fn ack_and_input_match_the_protocol() {
        let a = ack(7, 3, 1500);
        assert_eq!(header(&a).unwrap(), Header { kind: T_ACK, flags: 0, generation: 7, sequence: 3 });
        assert_eq!(&a[HEADER_SIZE..], &1500u32.to_be_bytes());

        let r = Input::Button { button: 1, down: true, x: 100, y: 65535 }.record();
        assert_eq!(r, [4, 0, 0, 1, 0, 0, 0, 0, 0, 100, 255, 255, 0, 0, 0, 0]);
        let r = Input::Text('é').record();
        assert_eq!(r[0], 8);
        assert_eq!(u32_at(&r, 4), 'é' as u32);
        let r = Input::Wheel { horizontal: false, delta: -250, x: 1, y: 2 }.record();
        assert_eq!(u32_at(&r, 4) as i32, -240);
        let m = Input::ReleaseAll.message(9);
        assert_eq!(header(&m).unwrap().kind, T_INPUT);
        assert_eq!(m.len(), HEADER_SIZE + INPUT_SIZE);
    }

    #[test]
    fn splits_nal_units() {
        let d = [0, 0, 0, 1, 0x67, 9, 0, 0, 1, 0x68, 8, 0, 0, 0, 1, 0x65, 7, 7];
        let n = nal_units(&d);
        assert_eq!(n, vec![&[0x67, 9][..], &[0x68, 8][..], &[0x65, 7, 7][..]]);
    }
}
