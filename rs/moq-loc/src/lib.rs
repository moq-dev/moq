//! Wire encoding for the Low Overhead Container (LOC) defined in
//! [draft-ietf-moq-loc](https://www.ietf.org/archive/id/draft-ietf-moq-loc-00.html).
//!
//! A LOC frame is laid out as:
//!
//! ```text
//! [varint: properties_length]
//! [properties_block: properties_length bytes of KVPs]
//! [codec_bitstream: remaining bytes]
//! ```
//!
//! Each KVP starts with a delta-encoded type id. Even types carry a single
//! varint value, odd types carry length-prefixed bytes. Recognized types:
//!
//! | ID   | Name        | Decoded into       |
//! |------|-------------|--------------------|
//! | 0x06 | Timestamp   | [`Frame::timestamp`] (required) |
//! | 0x08 | Timescale   | [`Frame::timescale`] (optional, per-frame override) |
//! | 0x0d | Video Config | Skipped. The hang catalog's `description` is authoritative. |
//!
//! Any other property is silently skipped on decode and never emitted on
//! encode. Public properties are not handled here. They belong in the MoQ
//! object header and are stripped by the transport layer.
//!
//! Varint encoding is QUIC-style throughout, matching the rest of the moq
//! stack.

use bytes::{Buf, Bytes, BytesMut};

/// Property IDs recognized by this implementation.
const PROP_TIMESTAMP: u64 = 0x06;
const PROP_TIMESCALE: u64 = 0x08;

/// Maximum value representable as a 62-bit QUIC varint.
const VARINT_MAX: u64 = (1u64 << 62) - 1;

/// A decoded LOC frame.
#[derive(Clone, Debug)]
pub struct Frame {
	/// Presentation timestamp, in units determined by the active timescale.
	pub timestamp: u64,

	/// Per-frame timescale override (property 0x08).
	///
	/// `Some` when the frame carried an explicit timescale, `None` when it
	/// relies on the catalog's default.
	pub timescale: Option<u64>,

	/// Codec bitstream payload (the bytes after the properties block).
	pub payload: Bytes,
}

/// Errors from LOC frame encode/decode.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The frame's property block did not contain a 0x06 (Timestamp) entry.
	#[error("loc frame missing required timestamp property")]
	MissingTimestamp,

	/// The property block ran past `properties_length` or was otherwise malformed.
	#[error("malformed loc properties")]
	MalformedProperties,

	/// A varint did not fit in the buffer.
	#[error("short buffer")]
	ShortBuffer,

	/// A value exceeds the 62-bit varint range.
	#[error("value out of range")]
	OutOfRange,
}

/// Decode a LOC frame.
///
/// Consumes the properties_length prefix, walks the bounded property block,
/// and returns the remainder as `payload`.
pub fn decode(mut buf: Bytes) -> Result<Frame, Error> {
	let properties_length = read_varint(&mut buf)?;
	let properties_length: usize = properties_length.try_into().map_err(|_| Error::MalformedProperties)?;

	if properties_length > buf.remaining() {
		return Err(Error::MalformedProperties);
	}

	let mut props = buf.split_to(properties_length);

	let mut timestamp: Option<u64> = None;
	let mut timescale: Option<u64> = None;
	let mut prev_type: u64 = 0;
	let mut first = true;

	while props.has_remaining() {
		let delta = read_varint(&mut props)?;
		let abs = if first {
			first = false;
			delta
		} else {
			prev_type.checked_add(delta).ok_or(Error::MalformedProperties)?
		};
		prev_type = abs;

		if abs % 2 == 0 {
			let value = read_varint(&mut props)?;
			match abs {
				PROP_TIMESTAMP => timestamp = Some(value),
				PROP_TIMESCALE => {
					if value == 0 {
						return Err(Error::MalformedProperties);
					}
					timescale = Some(value);
				}
				_ => {}
			}
		} else {
			let len = read_varint(&mut props)?;
			let len: usize = len.try_into().map_err(|_| Error::MalformedProperties)?;
			if len > props.remaining() {
				return Err(Error::MalformedProperties);
			}
			// We don't care about any odd-typed property today; PROP_VIDEO_CONFIG
			// (0x0d) and any unknown ID are skipped.
			props.advance(len);
		}
	}

	let timestamp = timestamp.ok_or(Error::MissingTimestamp)?;

	Ok(Frame {
		timestamp,
		timescale,
		payload: buf,
	})
}

/// Encode a LOC frame with a single 0x06 Timestamp property.
///
/// Per-frame 0x08 timescale is never emitted. The encoder relies on the
/// catalog timescale to interpret `timestamp`.
pub fn encode(timestamp: u64, payload: &[u8]) -> Result<Bytes, Error> {
	let mut props = BytesMut::with_capacity(16);
	write_varint(&mut props, PROP_TIMESTAMP)?;
	write_varint(&mut props, timestamp)?;

	let mut out = BytesMut::with_capacity(props.len() + payload.len() + 8);
	write_varint(&mut out, props.len() as u64)?;
	out.extend_from_slice(&props);
	out.extend_from_slice(payload);

	Ok(out.freeze())
}

/// Decode a QUIC-style varint (2-bit length tag in top bits).
fn read_varint<B: Buf>(buf: &mut B) -> Result<u64, Error> {
	if !buf.has_remaining() {
		return Err(Error::ShortBuffer);
	}
	let b = buf.get_u8();
	let tag = b >> 6;
	let mut bytes = [0u8; 8];
	bytes[0] = b & 0b0011_1111;
	let value = match tag {
		0b00 => u64::from(bytes[0]),
		0b01 => {
			if buf.remaining() < 1 {
				return Err(Error::ShortBuffer);
			}
			buf.copy_to_slice(&mut bytes[1..2]);
			u64::from(u16::from_be_bytes(bytes[..2].try_into().unwrap()))
		}
		0b10 => {
			if buf.remaining() < 3 {
				return Err(Error::ShortBuffer);
			}
			buf.copy_to_slice(&mut bytes[1..4]);
			u64::from(u32::from_be_bytes(bytes[..4].try_into().unwrap()))
		}
		0b11 => {
			if buf.remaining() < 7 {
				return Err(Error::ShortBuffer);
			}
			buf.copy_to_slice(&mut bytes[1..8]);
			u64::from_be_bytes(bytes)
		}
		_ => unreachable!(),
	};
	Ok(value)
}

/// Encode a QUIC-style varint (2-bit length tag in top bits).
fn write_varint<B: bytes::BufMut>(buf: &mut B, value: u64) -> Result<(), Error> {
	if value > VARINT_MAX {
		return Err(Error::OutOfRange);
	}
	if value < (1u64 << 6) {
		if buf.remaining_mut() < 1 {
			return Err(Error::ShortBuffer);
		}
		buf.put_u8(value as u8);
	} else if value < (1u64 << 14) {
		if buf.remaining_mut() < 2 {
			return Err(Error::ShortBuffer);
		}
		buf.put_u16(value as u16 | 0b01 << 14);
	} else if value < (1u64 << 30) {
		if buf.remaining_mut() < 4 {
			return Err(Error::ShortBuffer);
		}
		buf.put_u32(value as u32 | 0b10 << 30);
	} else {
		if buf.remaining_mut() < 8 {
			return Err(Error::ShortBuffer);
		}
		buf.put_u64(value | 0b11 << 62);
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn roundtrip() {
		let payload = Bytes::from_static(b"hello world");
		let encoded = encode(12345, &payload).unwrap();

		let frame = decode(encoded).unwrap();
		assert_eq!(frame.timestamp, 12345);
		assert_eq!(frame.timescale, None);
		assert_eq!(frame.payload, payload);
	}

	#[test]
	fn decode_per_frame_timescale() {
		// Manually craft: properties = [delta=0x06 timestamp=96000, delta=0x02 (abs=0x08) timescale=48000]
		let mut props = BytesMut::new();
		write_varint(&mut props, PROP_TIMESTAMP).unwrap();
		write_varint(&mut props, 96_000).unwrap();
		write_varint(&mut props, PROP_TIMESCALE - PROP_TIMESTAMP).unwrap(); // delta = 2
		write_varint(&mut props, 48_000).unwrap();

		let mut frame = BytesMut::new();
		write_varint(&mut frame, props.len() as u64).unwrap();
		frame.extend_from_slice(&props);
		frame.extend_from_slice(b"payload");

		let decoded = decode(frame.freeze()).unwrap();
		assert_eq!(decoded.timestamp, 96_000);
		assert_eq!(decoded.timescale, Some(48_000));
		assert_eq!(decoded.payload, Bytes::from_static(b"payload"));
	}

	#[test]
	fn decode_skips_video_config() {
		// properties = [delta=0x06 timestamp=10, delta=0x07 (abs=0x0d, video config) bytes=[1,2,3]]
		let mut props = BytesMut::new();
		write_varint(&mut props, PROP_TIMESTAMP).unwrap();
		write_varint(&mut props, 10).unwrap();
		write_varint(&mut props, 0x0d - PROP_TIMESTAMP).unwrap(); // delta = 7 -> abs 0x0d (Video Config)
		write_varint(&mut props, 3).unwrap(); // length
		props.extend_from_slice(&[0x01, 0x02, 0x03]);

		let mut frame = BytesMut::new();
		write_varint(&mut frame, props.len() as u64).unwrap();
		frame.extend_from_slice(&props);
		frame.extend_from_slice(b"data");

		let decoded = decode(frame.freeze()).unwrap();
		assert_eq!(decoded.timestamp, 10);
		assert_eq!(decoded.timescale, None);
		assert_eq!(decoded.payload, Bytes::from_static(b"data"));
	}

	#[test]
	fn decode_missing_timestamp_errors() {
		// properties = [delta=0x08 timescale=1000], no timestamp
		let mut props = BytesMut::new();
		write_varint(&mut props, PROP_TIMESCALE).unwrap();
		write_varint(&mut props, 1000).unwrap();

		let mut frame = BytesMut::new();
		write_varint(&mut frame, props.len() as u64).unwrap();
		frame.extend_from_slice(&props);
		frame.extend_from_slice(b"x");

		assert!(matches!(decode(frame.freeze()), Err(Error::MissingTimestamp)));
	}

	#[test]
	fn decode_empty_properties_errors() {
		let mut frame = BytesMut::new();
		write_varint(&mut frame, 0).unwrap();
		frame.extend_from_slice(b"payload");

		assert!(matches!(decode(frame.freeze()), Err(Error::MissingTimestamp)));
	}

	#[test]
	fn decode_rejects_zero_timescale() {
		// Per-frame 0x08 timescale of 0 is invalid (would divide by zero).
		let mut props = BytesMut::new();
		write_varint(&mut props, PROP_TIMESTAMP).unwrap();
		write_varint(&mut props, 10).unwrap();
		write_varint(&mut props, PROP_TIMESCALE - PROP_TIMESTAMP).unwrap();
		write_varint(&mut props, 0).unwrap();

		let mut frame = BytesMut::new();
		write_varint(&mut frame, props.len() as u64).unwrap();
		frame.extend_from_slice(&props);
		frame.extend_from_slice(b"x");

		assert!(matches!(decode(frame.freeze()), Err(Error::MalformedProperties)));
	}

	#[test]
	fn decode_overflowing_properties_length_errors() {
		let mut frame = BytesMut::new();
		write_varint(&mut frame, 100).unwrap(); // claims 100 bytes of properties
		frame.extend_from_slice(&[0x06]); // only 1 byte follows

		assert!(matches!(decode(frame.freeze()), Err(Error::MalformedProperties)));
	}
}
