//! The encoded-frame protocol between the bridge and the demo's worker.
//!
//! The bridge runs inside CARLA; the worker is the Python process that owns the
//! MoQ broadcasts. They share one Unix stream socket that carries compressed
//! video and control only: raw pixels never cross it.
//!
//! Every message is `u32 length | u8 kind | body`, where `length` counts the
//! kind byte and the body. Integers are little-endian, a string is
//! `u16 length | UTF-8`, and a blob is `u32 length | bytes`.
//! `moq_carla_sim/bridge.py` mirrors this module and tests the same vectors.

use std::io::{self, Read};

use bytes::{Buf, BufMut, Bytes, BytesMut};

/// Bumped on any incompatible change; both sides refuse a mismatch.
pub const VERSION: u16 = 1;

/// The largest message either side accepts. A 720p IDR is a few hundred kB.
pub const MAX_MESSAGE: u32 = 16 << 20;

/// Renditions per stream, so demand fits a one-byte mask.
pub const MAX_RENDITIONS: usize = 8;

const HELLO: u8 = 0x01;
const OPEN: u8 = 0x02;
const DEMAND: u8 = 0x03;
const KEYFRAME: u8 = 0x04;
const CLOSE: u8 = 0x05;

const EVENT_HELLO: u8 = 0x81;
const EVENT_CONFIG: u8 = 0x82;
const EVENT_FRAME: u8 = 0x83;
const EVENT_ERROR: u8 = 0x84;

/// One encoded size of a stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rendition {
    /// The MoQ track name the worker publishes it as.
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Target bitrate in bits per second.
    pub bitrate: u64,
    /// Frames per second the simulation steps at.
    pub framerate: u32,
}

/// Worker to bridge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// The first message on a connection.
    Hello { version: u16 },
    /// Encode the capture sources registered as `name` as stream `id`. The
    /// bridge answers with one [`Event::Config`] per rendition, in order.
    Open {
        id: u32,
        name: String,
        renditions: Vec<Rendition>,
    },
    /// The renditions that have subscribers, as a mask of `Open` indices.
    Demand { id: u32, renditions: u8 },
    /// Encode the next frame of one rendition as an IDR.
    Keyframe { id: u32, rendition: u8 },
    /// Stop encoding stream `id`.
    Close { id: u32 },
}

/// Bridge to worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The answer to [`Request::Hello`].
    Hello { version: u16 },
    /// A rendition's Annex-B parameter sets, sent before any of its frames so
    /// the worker can advertise the track before anyone subscribes.
    Config { id: u32, rendition: u8, init: Bytes },
    /// One encoded access unit.
    Frame(Frame),
    /// A failure. Stream 0 is the connection itself; any other stream has
    /// stopped encoding.
    Error { id: u32, message: String },
}

/// One encoded access unit and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub id: u32,
    pub rendition: u8,
    /// CARLA's frame counter at capture.
    pub frame: u64,
    /// Simulation time at capture, in microseconds.
    pub timestamp_us: u64,
    /// An IDR: a subscriber can start decoding here.
    pub keyframe: bool,
    /// Annex-B, with in-band parameter sets on every IDR.
    pub payload: Bytes,
}

impl Request {
    pub fn encode(&self) -> Bytes {
        let mut out = Message::new();
        match self {
            Self::Hello { version } => {
                out.kind(HELLO);
                out.buf.put_u16_le(*version);
            }
            Self::Open {
                id,
                name,
                renditions,
            } => {
                out.kind(OPEN);
                out.buf.put_u32_le(*id);
                out.string(name);
                out.buf.put_u8(renditions.len() as u8);
                for rendition in renditions {
                    out.string(&rendition.name);
                    out.buf.put_u32_le(rendition.width);
                    out.buf.put_u32_le(rendition.height);
                    out.buf.put_u64_le(rendition.bitrate);
                    out.buf.put_u32_le(rendition.framerate);
                }
            }
            Self::Demand { id, renditions } => {
                out.kind(DEMAND);
                out.buf.put_u32_le(*id);
                out.buf.put_u8(*renditions);
            }
            Self::Keyframe { id, rendition } => {
                out.kind(KEYFRAME);
                out.buf.put_u32_le(*id);
                out.buf.put_u8(*rendition);
            }
            Self::Close { id } => {
                out.kind(CLOSE);
                out.buf.put_u32_le(*id);
            }
        }
        out.finish()
    }

    pub fn decode(kind: u8, mut body: Bytes) -> io::Result<Self> {
        let body = &mut body;
        let request = match kind {
            HELLO => Self::Hello {
                version: u16_le(body)?,
            },
            OPEN => {
                let id = u32_le(body)?;
                let name = string(body)?;
                let count = usize::from(u8_(body)?);
                if count == 0 || count > MAX_RENDITIONS {
                    return Err(invalid(format!("stream {name:?} has {count} renditions")));
                }
                let mut renditions = Vec::with_capacity(count);
                for _ in 0..count {
                    renditions.push(Rendition {
                        name: string(body)?,
                        width: u32_le(body)?,
                        height: u32_le(body)?,
                        bitrate: u64_le(body)?,
                        framerate: u32_le(body)?,
                    });
                }
                Self::Open {
                    id,
                    name,
                    renditions,
                }
            }
            DEMAND => Self::Demand {
                id: u32_le(body)?,
                renditions: u8_(body)?,
            },
            KEYFRAME => Self::Keyframe {
                id: u32_le(body)?,
                rendition: u8_(body)?,
            },
            CLOSE => Self::Close { id: u32_le(body)? },
            kind => return Err(invalid(format!("unknown request kind {kind:#04x}"))),
        };
        end(body)?;
        Ok(request)
    }
}

impl Event {
    pub fn encode(&self) -> Bytes {
        let mut out = Message::new();
        match self {
            Self::Hello { version } => {
                out.kind(EVENT_HELLO);
                out.buf.put_u16_le(*version);
            }
            Self::Config {
                id,
                rendition,
                init,
            } => {
                out.kind(EVENT_CONFIG);
                out.buf.put_u32_le(*id);
                out.buf.put_u8(*rendition);
                out.blob(init);
            }
            Self::Frame(frame) => {
                out.kind(EVENT_FRAME);
                out.buf.put_u32_le(frame.id);
                out.buf.put_u8(frame.rendition);
                out.buf.put_u64_le(frame.frame);
                out.buf.put_u64_le(frame.timestamp_us);
                out.buf.put_u8(u8::from(frame.keyframe));
                out.blob(&frame.payload);
            }
            Self::Error { id, message } => {
                out.kind(EVENT_ERROR);
                out.buf.put_u32_le(*id);
                out.string(message);
            }
        }
        out.finish()
    }

    pub fn decode(kind: u8, mut body: Bytes) -> io::Result<Self> {
        let body = &mut body;
        let event = match kind {
            EVENT_HELLO => Self::Hello {
                version: u16_le(body)?,
            },
            EVENT_CONFIG => Self::Config {
                id: u32_le(body)?,
                rendition: u8_(body)?,
                init: blob(body)?,
            },
            EVENT_FRAME => Self::Frame(Frame {
                id: u32_le(body)?,
                rendition: u8_(body)?,
                frame: u64_le(body)?,
                timestamp_us: u64_le(body)?,
                keyframe: match u8_(body)? {
                    0 => false,
                    1 => true,
                    other => return Err(invalid(format!("keyframe flag {other}"))),
                },
                payload: blob(body)?,
            }),
            EVENT_ERROR => Self::Error {
                id: u32_le(body)?,
                message: string(body)?,
            },
            kind => return Err(invalid(format!("unknown event kind {kind:#04x}"))),
        };
        end(body)?;
        Ok(event)
    }
}

/// Read one message's kind and body, or `None` at a clean end of stream, which
/// is only between messages: a torn header is an error like a torn body.
pub fn read(reader: &mut impl Read) -> io::Result<Option<(u8, Bytes)>> {
    let mut length = [0u8; 4];
    let mut filled = 0;
    while filled < length.len() {
        match reader.read(&mut length[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "end of stream inside a message header",
                ));
            }
            Ok(read) => filled += read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    let length = u32::from_le_bytes(length);
    if length == 0 || length > MAX_MESSAGE {
        return Err(invalid(format!("message length {length}")));
    }
    let mut message = vec![0u8; length as usize];
    reader.read_exact(&mut message)?;
    let mut message = Bytes::from(message);
    let kind = message.get_u8();
    Ok(Some((kind, message)))
}

struct Message {
    buf: BytesMut,
}

impl Message {
    fn new() -> Self {
        let mut buf = BytesMut::new();
        // The length is patched in by `finish`.
        buf.put_u32_le(0);
        Self { buf }
    }

    fn kind(&mut self, kind: u8) {
        self.buf.put_u8(kind);
    }

    fn string(&mut self, value: &str) {
        let len = u16::try_from(value.len()).expect("protocol string longer than 64 KiB");
        self.buf.put_u16_le(len);
        self.buf.put_slice(value.as_bytes());
    }

    fn blob(&mut self, value: &[u8]) {
        let len = u32::try_from(value.len()).expect("protocol blob longer than 4 GiB");
        self.buf.put_u32_le(len);
        self.buf.put_slice(value);
    }

    fn finish(mut self) -> Bytes {
        let length = u32::try_from(self.buf.len() - 4).expect("protocol message longer than 4 GiB");
        self.buf[..4].copy_from_slice(&length.to_le_bytes());
        self.buf.freeze()
    }
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn need(body: &Bytes, len: usize) -> io::Result<()> {
    if body.remaining() < len {
        return Err(invalid(format!(
            "truncated message: need {len} bytes, have {}",
            body.remaining()
        )));
    }
    Ok(())
}

fn u8_(body: &mut Bytes) -> io::Result<u8> {
    need(body, 1)?;
    Ok(body.get_u8())
}

fn u16_le(body: &mut Bytes) -> io::Result<u16> {
    need(body, 2)?;
    Ok(body.get_u16_le())
}

fn u32_le(body: &mut Bytes) -> io::Result<u32> {
    need(body, 4)?;
    Ok(body.get_u32_le())
}

fn u64_le(body: &mut Bytes) -> io::Result<u64> {
    need(body, 8)?;
    Ok(body.get_u64_le())
}

fn string(body: &mut Bytes) -> io::Result<String> {
    let len = usize::from(u16_le(body)?);
    need(body, len)?;
    String::from_utf8(body.split_to(len).to_vec()).map_err(|err| invalid(format!("string: {err}")))
}

fn blob(body: &mut Bytes) -> io::Result<Bytes> {
    let len = u32_le(body)? as usize;
    need(body, len)?;
    Ok(body.split_to(len))
}

fn end(body: &Bytes) -> io::Result<()> {
    if body.has_remaining() {
        return Err(invalid(format!("{} trailing bytes", body.remaining())));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shared with `moq_carla_sim/bridge.py`'s tests: both sides must produce and
    // accept exactly these bytes.
    const HELLO_HEX: &str = "03000000010100";
    const OPEN_HEX: &str = "2c00000002070000000c00747275636b312f66726f6e740102007364aa010000f000000000350c00000000001e000000";
    const DEMAND_HEX: &str = "06000000030700000002";
    const KEYFRAME_HEX: &str = "06000000040700000001";
    const CLOSE_HEX: &str = "050000000507000000";
    const EVENT_HELLO_HEX: &str = "03000000810100";
    const CONFIG_HEX: &str = "0f000000820700000000050000000000000167";
    const FRAME_HEX: &str =
        "20000000830700000001d204000000000000404b4c000000000001050000000000000165";
    const ERROR_HEX: &str = "0b00000084000000000400626f6f6d";

    fn requests() -> Vec<(Request, &'static str)> {
        vec![
            (Request::Hello { version: 1 }, HELLO_HEX),
            (
                Request::Open {
                    id: 7,
                    name: "truck1/front".into(),
                    renditions: vec![Rendition {
                        name: "sd".into(),
                        width: 426,
                        height: 240,
                        bitrate: 800_000,
                        framerate: 30,
                    }],
                },
                OPEN_HEX,
            ),
            (
                Request::Demand {
                    id: 7,
                    renditions: 0b10,
                },
                DEMAND_HEX,
            ),
            (
                Request::Keyframe {
                    id: 7,
                    rendition: 1,
                },
                KEYFRAME_HEX,
            ),
            (Request::Close { id: 7 }, CLOSE_HEX),
        ]
    }

    fn events() -> Vec<(Event, &'static str)> {
        vec![
            (Event::Hello { version: 1 }, EVENT_HELLO_HEX),
            (
                Event::Config {
                    id: 7,
                    rendition: 0,
                    init: Bytes::from_static(&[0, 0, 0, 1, 0x67]),
                },
                CONFIG_HEX,
            ),
            (
                Event::Frame(Frame {
                    id: 7,
                    rendition: 1,
                    frame: 1234,
                    timestamp_us: 5_000_000,
                    keyframe: true,
                    payload: Bytes::from_static(&[0, 0, 0, 1, 0x65]),
                }),
                FRAME_HEX,
            ),
            (
                Event::Error {
                    id: 0,
                    message: "boom".into(),
                },
                ERROR_HEX,
            ),
        ]
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn unhex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn requests_match_the_shared_vectors() {
        for (request, expected) in requests() {
            assert_eq!(hex(&request.encode()), expected, "{request:?}");
            let wire = unhex(expected);
            let (kind, body) = read(&mut &wire[..]).unwrap().unwrap();
            assert_eq!(Request::decode(kind, body).unwrap(), request);
        }
    }

    #[test]
    fn events_match_the_shared_vectors() {
        for (event, expected) in events() {
            assert_eq!(hex(&event.encode()), expected, "{event:?}");
            let wire = unhex(expected);
            let (kind, body) = read(&mut &wire[..]).unwrap().unwrap();
            assert_eq!(Event::decode(kind, body).unwrap(), event);
        }
    }

    #[test]
    fn rejects_malformed_messages() {
        // Truncated body, and a header torn after 1 to 3 bytes.
        let wire = unhex("06000000030700");
        assert!(read(&mut &wire[..]).is_err());
        for torn in 1..4 {
            assert!(read(&mut &wire[..torn]).is_err(), "{torn} header bytes");
        }
        // Trailing bytes after a complete request.
        let (kind, body) = read(&mut &unhex("0700000003070000000200")[..])
            .unwrap()
            .unwrap();
        assert!(Request::decode(kind, body).is_err());
        // Unknown kind.
        let (kind, body) = read(&mut &unhex("0100000009")[..]).unwrap().unwrap();
        assert!(Request::decode(kind, body).is_err());
        // Too many renditions.
        let mut open = Request::Open {
            id: 1,
            name: "x".into(),
            renditions: Vec::new(),
        }
        .encode()
        .to_vec();
        assert!(Request::decode(open[4], Bytes::copy_from_slice(&open[5..])).is_err());
        open[4 + 1 + 4 + 3] = 9;
        assert!(Request::decode(open[4], Bytes::copy_from_slice(&open[5..])).is_err());
        // Zero and oversized lengths.
        assert!(read(&mut &unhex("00000000")[..]).is_err());
        assert!(read(&mut &unhex("ffffffff01")[..]).is_err());
        // A clean end of stream between messages.
        assert!(read(&mut &[][..]).unwrap().is_none());
    }
}
