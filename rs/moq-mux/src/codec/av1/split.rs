//! AV1 OBU stream splitter.
//!
//! The AV1 analogue of [`crate::codec::h264::Split`]: turns a raw OBU byte
//! stream into [`crate::container::Frame`]s. It finds temporal-unit boundaries
//! and flags keyframes (a sequence header or a `KEY_FRAME`), and stamps
//! wall-clock timestamps when the caller has none (stdin). It owns no track,
//! catalog, or codec config. AV1 carries the sequence header inline ahead of
//! keyframes, so unlike H.264/H.265 there is nothing to cache or re-insert; the
//! importer parses the config out of the frames it emits.

use bytes::{Buf, Bytes, BytesMut};
use scuffle_av1::{ObuHeader, ObuType};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::Error;
use crate::Result;

/// AV1 OBU stream splitter: bytes in, [`Frame`](crate::container::Frame)s out.
///
/// Feed bytes via [`decode_stream`](Self::decode_stream) (unknown frame
/// boundaries, e.g. stdin), [`decode_frame`](Self::decode_frame) (one complete
/// temporal unit per call), or [`decode_from`](Self::decode_from) (an async
/// reader). Each returns the frames it produced. [`seed`](Self::seed) feeds
/// leading metadata OBUs (e.g. a sequence header) into the next frame.
pub struct Split {
	current: Au,
	zero: Option<tokio::time::Instant>,
	pending: Vec<crate::container::Frame>,
}

#[derive(Default)]
struct Au {
	chunks: BytesMut,
	contains_keyframe: bool,
	contains_frame: bool,
}

impl Default for Split {
	fn default() -> Self {
		Self::new()
	}
}

impl Split {
	/// A fresh splitter.
	pub fn new() -> Self {
		Self {
			current: Au::default(),
			zero: None,
			pending: Vec::new(),
		}
	}

	/// Feed leading metadata OBUs (e.g. a sequence header) into the in-flight
	/// access unit without completing a frame, so they prefix the next keyframe.
	/// The buffer must not contain a completed frame (no timestamp is available).
	pub fn seed<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> Result<()> {
		let mut obus = ObuIterator::new(buf);
		while let Some(obu) = obus.next().transpose()? {
			self.decode_obu(obu, None)?;
		}
		if let Some(obu) = obus.flush()? {
			self.decode_obu(obu, None)?;
		}
		Ok(())
	}

	/// Decode from an asynchronous reader, returning all frames produced.
	pub async fn decode_from<T: AsyncRead + Unpin>(&mut self, reader: &mut T) -> Result<Vec<crate::container::Frame>> {
		let mut frames = Vec::new();
		let mut buffer = BytesMut::new();
		while reader.read_buf(&mut buffer).await? > 0 {
			frames.extend(self.decode_stream(&mut buffer, None)?);
		}
		Ok(frames)
	}

	/// Decode a buffer where frame boundaries are unknown, returning the frames
	/// it produced. The final temporal unit stays buffered until the next call
	/// (or [`decode_frame`](Self::decode_frame)).
	pub fn decode_stream<T: Buf + AsRef<[u8]>>(
		&mut self,
		buf: &mut T,
		pts: Option<moq_net::Timestamp>,
	) -> Result<Vec<crate::container::Frame>> {
		let obus = ObuIterator::new(buf);
		for obu in obus {
			// Resolve a timestamp per OBU so a wall-clock stream doesn't reuse one.
			let pts = self.pts(pts)?;
			self.decode_obu(obu?, Some(pts))?;
		}
		Ok(std::mem::take(&mut self.pending))
	}

	/// Decode a buffer holding one complete temporal unit, returning the frames
	/// it produced. The unit is flushed before returning.
	pub fn decode_frame<T: Buf + AsRef<[u8]>>(
		&mut self,
		buf: &mut T,
		pts: Option<moq_net::Timestamp>,
	) -> Result<Vec<crate::container::Frame>> {
		let pts = self.pts(pts)?;
		let mut obus = ObuIterator::new(buf);
		while let Some(obu) = obus.next().transpose()? {
			self.decode_obu(obu, Some(pts))?;
		}
		if let Some(obu) = obus.flush()? {
			self.decode_obu(obu, Some(pts))?;
		}
		self.maybe_start_frame(Some(pts))?;
		Ok(std::mem::take(&mut self.pending))
	}

	fn decode_obu(&mut self, obu_data: Bytes, pts: Option<moq_net::Timestamp>) -> Result<()> {
		if obu_data.is_empty() {
			return Err(Error::ObuTooShort.into());
		}

		// Parse the OBU header to learn the type; the payload offset is whatever
		// the parser consumed (header + optional extension + LEB128 size).
		let mut reader = &obu_data[..];
		let header = ObuHeader::parse(&mut reader)?;
		let payload_offset = obu_data.len() - reader.len();

		match header.obu_type {
			// A sequence header anchors a keyframe; the importer parses the config.
			ObuType::SequenceHeader => {
				self.current.contains_keyframe = true;
			}
			ObuType::TemporalDelimiter => {
				self.maybe_start_frame(pts)?;
			}
			ObuType::FrameHeader | ObuType::Frame => {
				let is_keyframe = obu_data.get(payload_offset).is_some_and(|first_byte| {
					let show_existing_frame = (first_byte >> 7) & 1;
					if show_existing_frame == 1 {
						self.current.contains_keyframe
					} else {
						let frame_type = (first_byte >> 5) & 0b11;
						frame_type == 0 // KEY_FRAME
					}
				});

				if is_keyframe {
					self.current.contains_keyframe = true;
				}
				self.current.contains_frame = true;
			}
			ObuType::Metadata => {
				self.maybe_start_frame(pts)?;
			}
			ObuType::TileGroup | ObuType::TileList => {
				self.current.contains_frame = true;
			}
			_ => {}
		}

		tracing::trace!(?header.obu_type, "parsed OBU");

		self.current.chunks.extend_from_slice(&obu_data);
		Ok(())
	}

	fn maybe_start_frame(&mut self, pts: Option<moq_net::Timestamp>) -> Result<()> {
		if !self.current.contains_frame {
			return Ok(());
		}

		let pts = pts.ok_or(Error::MissingTimestamp)?;
		let keyframe = self.current.contains_keyframe;
		let payload = std::mem::take(&mut self.current.chunks).freeze();
		self.current.contains_keyframe = false;
		self.current.contains_frame = false;

		self.pending.push(crate::container::Frame {
			timestamp: pts,
			payload,
			keyframe,
			duration: None,
		});
		Ok(())
	}

	/// Drop any in-flight temporal unit.
	///
	/// Pre-reset OBUs would otherwise leak into a later frame with the wrong
	/// timestamp.
	pub fn reset(&mut self) {
		self.current = Au::default();
	}

	fn pts(&mut self, hint: Option<moq_net::Timestamp>) -> Result<moq_net::Timestamp> {
		if let Some(pts) = hint {
			return Ok(pts);
		}
		let zero = self.zero.get_or_insert_with(tokio::time::Instant::now);
		Ok(moq_net::Timestamp::from_micros(zero.elapsed().as_micros() as u64)?)
	}
}

/// Iterator over AV1 Open Bitstream Units (OBUs).
pub(super) struct ObuIterator<'a, T: Buf + AsRef<[u8]> + 'a> {
	buf: &'a mut T,
}

impl<'a, T: Buf + AsRef<[u8]> + 'a> ObuIterator<'a, T> {
	pub fn new(buf: &'a mut T) -> Self {
		Self { buf }
	}

	pub fn flush(self) -> Result<Option<Bytes>> {
		let remaining = self.buf.remaining();
		if remaining == 0 {
			return Ok(None);
		}
		Ok(Some(self.buf.copy_to_bytes(remaining)))
	}
}

impl<'a, T: Buf + AsRef<[u8]> + 'a> Iterator for ObuIterator<'a, T> {
	type Item = Result<Bytes>;

	fn next(&mut self) -> Option<Self::Item> {
		if self.buf.remaining() == 0 {
			return None;
		}

		let data = self.buf.as_ref();
		if data.is_empty() {
			return None;
		}

		// OBU header: forbidden(1) | type(4) | extension_flag(1) | has_size(1) | reserved(1)
		let header = data[0];
		let has_extension = (header >> 2) & 1 == 1;
		let has_size = (header >> 1) & 1 == 1;

		if !has_size {
			let remaining = self.buf.remaining();
			return Some(Ok(self.buf.copy_to_bytes(remaining)));
		}

		// LEB128 size field follows the header byte and optional extension byte.
		let mut size: usize = 0;
		let mut offset = if has_extension { 2 } else { 1 };
		let mut shift = 0;

		loop {
			if offset >= data.len() {
				return None;
			}

			let byte = data[offset];
			offset += 1;

			size |= ((byte & 0x7F) as usize) << shift;
			shift += 7;

			if byte & 0x80 == 0 {
				break;
			}
			if shift >= 56 {
				return Some(Err(Error::ObuSizeTooLarge.into()));
			}
		}

		let total_size = offset + size;
		if total_size > self.buf.remaining() {
			return None;
		}

		Some(Ok(self.buf.copy_to_bytes(total_size)))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	// OBU header byte: forbidden(0) | type(4) | extension_flag(0) | has_size(1) | reserved(0).
	fn obu(obu_type: u8, payload: &[u8]) -> Vec<u8> {
		let mut o = vec![(obu_type << 3) | 0b010, payload.len() as u8];
		o.extend_from_slice(payload);
		o
	}

	fn td() -> Vec<u8> {
		obu(2, &[]) // OBU_TEMPORAL_DELIMITER
	}
	fn seq_header() -> Vec<u8> {
		obu(1, &[0xaa, 0xbb]) // OBU_SEQUENCE_HEADER (payload not parsed by the splitter)
	}
	fn key_frame() -> Vec<u8> {
		obu(6, &[0x00, 0x11]) // OBU_FRAME, first byte: show_existing=0, frame_type=0 (KEY_FRAME)
	}
	fn inter_frame() -> Vec<u8> {
		obu(6, &[0x20, 0x11]) // OBU_FRAME, first byte: frame_type=1 (INTER_FRAME)
	}

	fn cat(parts: &[Vec<u8>]) -> BytesMut {
		let mut buf = BytesMut::new();
		for p in parts {
			buf.extend_from_slice(p);
		}
		buf
	}

	fn ts() -> moq_net::Timestamp {
		moq_net::Timestamp::from_micros(0).unwrap()
	}

	/// A temporal unit with a sequence header + KEY_FRAME emits one keyframe.
	#[tokio::test(start_paused = true)]
	async fn decode_frame_keyframe() {
		let mut split = Split::new();
		let frames = split
			.decode_frame(&mut cat(&[td(), seq_header(), key_frame()]), Some(ts()))
			.unwrap();
		assert_eq!(frames.len(), 1);
		assert!(frames[0].keyframe);
	}

	/// A frame with no sequence header and INTER frame_type is not a keyframe.
	#[tokio::test(start_paused = true)]
	async fn decode_frame_delta_is_not_keyframe() {
		let mut split = Split::new();
		let frames = split
			.decode_frame(&mut cat(&[td(), inter_frame()]), Some(ts()))
			.unwrap();
		assert_eq!(frames.len(), 1);
		assert!(!frames[0].keyframe);
	}

	/// In streaming mode the next temporal delimiter closes the previous unit, so
	/// the trailing one stays buffered.
	#[tokio::test(start_paused = true)]
	async fn decode_stream_emits_on_next_boundary() {
		let mut split = Split::new();
		let frames = split
			.decode_stream(
				&mut cat(&[td(), seq_header(), key_frame(), td(), inter_frame()]),
				Some(ts()),
			)
			.unwrap();
		// Only the keyframe is complete; the inter frame waits for the next TD.
		assert_eq!(frames.len(), 1);
		assert!(frames[0].keyframe);
	}
}
