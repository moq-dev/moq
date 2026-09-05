//! Software H.264 decode backend via Cisco's openh264 (vendored, statically linked).
//!
//! The portable fallback when no hardware decoder is available. Accepts Annex-B
//! H.264 access units (SPS/PPS inline ahead of each keyframe) and returns packed
//! I420.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use bytes::Bytes;
use moq_net::Timestamp;
use openh264::OpenH264API;
use openh264::decoder::{Decoder, DecoderConfig, Flush};
use openh264::formats::YUVSource;
use openh264_sys2::{
	DECODING_STATE, dsBitstreamError, dsDataErrorConcealed, dsDepLayerLost, dsErrorFree, dsNoParamSets, dsOutOfMemory,
	dsRefListNullPtrs, dsRefLost,
};

use super::{Backend, Codec, Config};
use crate::frame::{I420, Surface};
use crate::{Error, Frame};

pub(crate) const NAME: &str = "openh264";

/// The decoding states that describe one picture rather than the decoder.
///
/// Every one of them is ordinary in a live stream. A skipped group loses the
/// reference chain (`dsRefLost`), a subscriber that joins mid-sequence has no
/// parameter sets yet (`dsNoParamSets`), a truncated access unit is a bitstream
/// error, and openh264 reports `dsOutOfMemory` when its picture pool runs dry,
/// having already reinitialised itself before returning. In all of them the
/// decoder is still usable and the next keyframe restores the picture, so they
/// cost a picture rather than the stream.
///
/// `dsInvalidArgument` and `dsInitialOptExpected` are absent on purpose: they
/// say the decoder was driven wrongly, which no amount of further bitstream
/// fixes. So is `dsDstBufNeedExpan`, which says the picture does not fit the
/// buffer handed in. `dsFramePending` is absent because it is not a lost
/// picture at all: `codec_app_def.h` defines it as needing more throughput
/// before a picture comes out, and openh264 only ever sets it while parsing
/// without decoding, which this backend never asks for.
const PICTURE_LOST: DECODING_STATE = dsRefLost
	| dsBitstreamError
	| dsDepLayerLost
	| dsNoParamSets
	| dsDataErrorConcealed
	| dsRefListNullPtrs
	| dsOutOfMemory;

/// The subset of [`PICTURE_LOST`] after which openh264 has emptied its
/// reordering buffer.
///
/// This is the distinction the timestamp queue turns on. `DecodeFrame2WithCtx`
/// calls `ResetDecoder` for exactly these two before returning
/// (`welsDecoderExt.cpp`), and `ResetDecoder` runs
/// `ResetReorderingPictureBuffers`, so every picture the decoder was holding is
/// gone and the timestamps waiting for those pictures are stale.
///
/// The rest fall through to `ReorderPicturesInDisplay` with the buffer intact.
/// Their pictures are still coming, so discarding the queue there would hand
/// each of them the timestamp of a picture several places later: on a stream
/// that reorders, one skipped group would leave the whole rest of the session
/// stamped a reordering depth early, spacing intact, and nothing would look
/// wrong except that the audio never lines up again.
const DECODER_RESET: DECODING_STATE = dsOutOfMemory | dsRefListNullPtrs;

/// How many access unit timestamps may be outstanding before the oldest is
/// dropped.
///
/// H.264 caps the decoded picture buffer at 16 frames, so a conforming stream
/// never has more than that many pictures in flight and this is only reached by
/// one that codes pictures the decoder never hands back. The cap keeps that from
/// growing without bound.
const MAX_PENDING: usize = 32;

pub(crate) struct Openh264 {
	decoder: Decoder,
	/// The timestamps of access units whose picture has not come out yet.
	///
	/// Access units arrive in decode order and pictures come back in display
	/// order, which differ as soon as the stream codes B slices, so the
	/// timestamp handed to the call a picture falls out of is not that picture's.
	/// A conforming decoder never releases a picture before every picture that
	/// displays ahead of it has been fed in, so the oldest timestamp still
	/// outstanding always belongs to the next picture out.
	pending: BinaryHeap<Reverse<Timestamp>>,
	/// Access units the decoder has refused since the last picture came out, so
	/// a run of them logs once rather than once per picture.
	lost: u64,
}

/// Builds an openh264 decoder configured the way this backend needs one.
///
/// Shared with [`Backend::flush`], which replaces the decoder rather than
/// reusing it: draining goes through `FlushFrame`, whose picture release is the
/// one that does not work single threaded, so every picture drained leaks a
/// slot from a pool of `num_ref_frames + 2`. At the end of a track that costs
/// nothing, because the decoder is about to be dropped. `Decoder::flush` is
/// public and documented as leaving the decoder usable, though, so a caller
/// draining at every seek would exhaust the pool in two or three of them and
/// then lose a group to the reset that follows.
fn new_decoder() -> Result<Decoder, Error> {
	let config = DecoderConfig::new().flush_after_decode(Flush::NoFlush);
	Decoder::with_api_config(OpenH264API::from_source(), config)
		.map_err(|e| Error::Codec(anyhow::anyhow!("openh264 decoder init: {e}")))
}

impl Openh264 {
	/// openh264 decodes H.264 only; `config` is accepted for signature parity
	/// (no hardware scaler; callers scale the CPU frames themselves).
	pub(crate) fn open(codec: Codec, config: &Config) -> Result<Box<dyn Backend>, Error> {
		Ok(Box::new(Self::new(codec, config)?))
	}

	/// The decoder itself, for a caller that wants the concrete type: the tests
	/// drain it through a method the [`Backend`] trait does not carry.
	fn new(codec: Codec, _config: &Config) -> Result<Self, Error> {
		if codec != Codec::H264 {
			return Err(Error::Codec(anyhow::anyhow!(
				"openh264 cannot decode {}",
				codec.label()
			)));
		}
		// `Flush::NoFlush`, against the crate's default. The default calls
		// `FlushFrame` after any decode that produced no picture, and
		// `FlushFrame` releases the reordering slot through a picture buffer
		// pointer openh264 only sets on its threaded path: single threaded that
		// pointer is null, so the picture is handed out and its reference count
		// never comes back down. The pool holds `num_ref_frames + 2` pictures, so
		// a stream that reorders (any stream with B slices) exhausts it within a
		// second, and every access unit after that fails with `dsOutOfMemory`
		// until the next keyframe. Letting openh264 release pictures on its own
		// schedule costs one picture of delay at the start of a reordering
		// sequence and nothing at all on a baseline one.
		let decoder = new_decoder()?;

		tracing::info!(decoder = NAME, "opened H.264 decoder");
		Ok(Self {
			decoder,
			pending: BinaryHeap::new(),
			lost: 0,
		})
	}

	/// Records an access unit the decoder refused.
	///
	/// Returns no frames for a state that describes the picture, so playback
	/// carries on and recovers at the next keyframe, and the error itself for a
	/// state that describes the decoder.
	///
	/// Only a state in [`DECODER_RESET`] takes the outstanding timestamps with
	/// it. After the others the decoder still holds the pictures those
	/// timestamps belong to.
	fn picture_lost(&mut self, timestamp: Timestamp, err: &openh264::Error) -> Result<Vec<Frame>, Error> {
		let state = i32::try_from(err.native_code()).unwrap_or(dsErrorFree);
		if state == dsErrorFree || state & !PICTURE_LOST != 0 {
			return Err(Error::Codec(anyhow::anyhow!("openh264 decode: {err}")));
		}

		if state & DECODER_RESET != 0 {
			self.pending.clear();
		} else {
			// The decoder kept every older reordered picture, but refused this
			// access unit. Remove its timestamp by value: it need not be the oldest
			// timestamp in the heap when access units arrive in decode order.
			let mut removed = false;
			self.pending.retain(|pending| {
				if !removed && pending.0 == timestamp {
					removed = true;
					false
				} else {
					true
				}
			});
		}
		self.lost += 1;
		if self.lost == 1 {
			tracing::warn!(
				decoder = NAME,
				state = format_args!("{state:#06x}"),
				"picture lost, waiting for the next keyframe"
			);
		} else {
			tracing::trace!(decoder = NAME, state = format_args!("{state:#06x}"), "picture lost");
		}
		Ok(Vec::new())
	}

	/// Takes the timestamp the next picture out belongs to, reporting a recovery
	/// on the way if pictures had been going missing.
	fn picture_out(&mut self) -> Option<Timestamp> {
		if self.lost > 0 {
			tracing::info!(decoder = NAME, lost = self.lost, "picture recovered");
			self.lost = 0;
		}
		self.pending.pop().map(|Reverse(timestamp)| timestamp)
	}

	/// Remembers `timestamp` as belonging to a picture still to come.
	fn picture_in(&mut self, timestamp: Timestamp) {
		self.pending.push(Reverse(timestamp));
		if self.pending.len() > MAX_PENDING {
			// Preserve the oldest timestamps, which are the pictures the decoder
			// could still hand out next, and discard the newest. Only a stream that
			// never hands its pictures back gets here, so rebuilding the small heap
			// does not matter.
			let mut kept: Vec<_> = self.pending.drain().collect();
			kept.sort_unstable_by_key(|pending| pending.0);
			kept.truncate(MAX_PENDING);
			self.pending = kept.into_iter().collect();
		}
	}
}

/// Copies one decoded picture out of openh264's own buffers.
fn picture(yuv: &impl YUVSource, timestamp: Timestamp) -> Result<Frame, Error> {
	let (width, height) = yuv.dimensions();
	if width % 2 != 0 || height % 2 != 0 {
		return Err(Error::Codec(anyhow::anyhow!(
			"decoded frame has odd dimensions {width}x{height}, expected 4:2:0"
		)));
	}
	let (y_stride, uv_stride, _) = yuv.strides();

	let frame = I420::from_planes(
		yuv.y(),
		yuv.u(),
		yuv.v(),
		y_stride,
		uv_stride,
		width as u32,
		height as u32,
	);
	Ok(Frame::new(Surface::I420(frame), timestamp))
}

impl Backend for Openh264 {
	fn decode(&mut self, access_unit: Bytes, timestamp: Timestamp, _keyframe: bool) -> Result<Vec<Frame>, Error> {
		self.picture_in(timestamp);

		// `None` means the decoder took the access unit but has no picture yet:
		// parameter sets only, or a picture its reordering buffer is holding.
		// The timestamp is corrected below, once the borrow of the decoder ends.
		let decoded = match self.decoder.decode(&access_unit) {
			Ok(Some(yuv)) => Some(picture(&yuv, timestamp)?),
			Ok(None) => None,
			Err(err) => return self.picture_lost(timestamp, &err),
		};

		match decoded {
			Some(mut frame) => {
				if let Some(timestamp) = self.picture_out() {
					frame.timestamp = timestamp;
				}
				Ok(vec![frame])
			}
			None => Ok(Vec::new()),
		}
	}

	/// Return the reordered pictures still buffered at the end of the stream.
	fn flush(&mut self) -> Result<Vec<Frame>, Error> {
		let tail = self
			.decoder
			.flush_remaining()
			.map_err(|e| Error::Codec(anyhow::anyhow!("openh264 flush: {e}")))?;
		// `tail` borrows the decoder, so copy the pictures before taking their
		// timestamps. The placeholder is replaced before any frame reaches a
		// caller; an unstamped picture is dropped below.
		let decoded: Vec<Frame> = tail
			.iter()
			.map(|yuv| picture(yuv, Timestamp::ZERO))
			.collect::<Result<_, _>>()?;
		drop(tail);

		let mut frames = Vec::with_capacity(decoded.len());
		let mut unstamped = 0usize;
		for mut frame in decoded {
			match self.picture_out() {
				Some(timestamp) => {
					frame.timestamp = timestamp;
					frames.push(frame);
				}
				None => unstamped += 1,
			}
		}
		if unstamped > 0 {
			tracing::warn!(
				decoder = NAME,
				dropped = unstamped,
				"flushed pictures had no timestamp waiting for them"
			);
		}
		self.pending.clear();
		self.lost = 0;
		// `FlushFrame` leaks a single-threaded decoder's picture-pool slots, so
		// replace the drained decoder before it is reused for another stream.
		self.decoder = new_decoder()?;
		Ok(frames)
	}

	fn name(&self) -> &str {
		NAME
	}
}

#[cfg(test)]
mod tests {
	use std::collections::HashMap;

	use moq_mux::codec::annexb;

	use super::*;

	/// libx264 at High profile with three B frames, 30 pictures at 64x64 and a
	/// keyframe every 15. Generated with:
	///
	/// ```text
	/// ffmpeg -f lavfi -i "testsrc2=s=64x64:r=30" -frames:v 30 -c:v libx264 \
	///     -profile:v high -bf 3 -g 15 -pix_fmt yuv420p -f h264 \
	///     bframes_64x64_pattern_30f.h264
	/// ```
	const BFRAMES: &[u8] = include_bytes!("../test_data/bframes_64x64_pattern_30f.h264");

	/// How many pictures the fixture codes.
	const PICTURES: usize = 30;

	/// The interval between fixture pictures: 30fps, the rate it was generated at.
	const FRAME_MICROS: u64 = 33_333;

	fn nal_type(nal: &Bytes) -> u8 {
		nal.first().map_or(0, |b| b & 0x1f)
	}

	/// Splits an Annex-B stream into one access unit per coded picture.
	///
	/// A slice opens a new access unit, and so does a parameter set, SEI, or
	/// delimiter that follows one: libx264 writes the parameter sets immediately
	/// ahead of each IDR, so they belong to the picture after them.
	fn access_units() -> Vec<(Bytes, bool)> {
		let mut buf = Bytes::from_static(BFRAMES);
		let mut iter = annexb::NalIterator::new(&mut buf);
		let mut nals: Vec<Bytes> = iter
			.by_ref()
			.map(|nal| nal.expect("fixture is valid Annex-B"))
			.collect();
		nals.extend(iter.flush().expect("fixture is valid Annex-B"));

		let is_slice = |nal: &Bytes| matches!(nal_type(nal), 1 | 5);
		let mut units = Vec::new();
		let mut current: Vec<Bytes> = Vec::new();
		for nal in nals {
			if current.iter().any(is_slice) && matches!(nal_type(&nal), 1 | 5 | 6 | 7 | 8 | 9) {
				units.push((
					annexb::build_prefix(current.iter()),
					current.iter().any(|nal| nal_type(nal) == 5),
				));
				current.clear();
			}
			current.push(nal);
		}
		if current.iter().any(is_slice) {
			units.push((
				annexb::build_prefix(current.iter()),
				current.iter().any(|nal| nal_type(nal) == 5),
			));
		}
		assert_eq!(units.len(), PICTURES, "the fixture split wrongly");
		units
	}

	fn at(index: usize) -> Timestamp {
		Timestamp::from_micros(index as u64 * FRAME_MICROS).expect("fixture timestamp")
	}

	fn open() -> Openh264 {
		Openh264::new(Codec::H264, &Config::new()).expect("the software decoder always opens")
	}

	fn decode_fixture(broken: Option<usize>) -> Vec<Frame> {
		let mut decoder = open();
		let mut frames = Vec::new();
		for (index, (payload, keyframe)) in access_units().into_iter().enumerate() {
			let payload = if broken == Some(index) {
				payload.slice(..payload.len() / 3)
			} else {
				payload
			};
			frames.extend(
				decoder
					.decode(payload, at(index), keyframe)
					.unwrap_or_else(|e| panic!("access unit {index} ended the stream: {e}")),
			);
		}
		frames.extend(decoder.flush().expect("the drain works"));
		frames
	}

	fn pixels(frame: &Frame) -> Vec<u8> {
		frame.surface.to_i420().expect("software output is I420").data.clone()
	}

	/// Draining goes through the release path that does not work single
	/// threaded, so the decoder is replaced rather than reused. Flushing
	/// repeatedly on a reordering stream would otherwise exhaust the pool.
	#[test]
	fn repeated_flushes_keep_decoding() {
		let mut decoder = open();
		for round in 0..6 {
			let mut decoded = 0;
			for (index, (payload, keyframe)) in access_units().into_iter().enumerate() {
				decoded += decoder
					.decode(payload, at(index), keyframe)
					.unwrap_or_else(|e| panic!("round {round} access unit {index}: {e}"))
					.len();
			}
			decoded += decoder.flush().expect("the drain works").len();
			assert_eq!(decoded, PICTURES, "round {round} lost pictures");
		}
	}

	/// Regression: a stream that reorders used to exhaust openh264's picture pool
	/// within a second, because the crate's flush-after-decode released the
	/// reordering slot without releasing the picture. Every access unit after
	/// that failed with `dsOutOfMemory` until the next keyframe, which a player
	/// sees as a picture that appears for a third of a second and then freezes.
	#[test]
	fn a_stream_with_b_frames_decodes_all_the_way_through() {
		let mut decoder = open();
		let mut decoded = 0;
		for (index, (payload, keyframe)) in access_units().into_iter().enumerate() {
			decoded += decoder
				.decode(payload, at(index), keyframe)
				.unwrap_or_else(|e| panic!("access unit {index} failed: {e}"))
				.len();
		}
		decoded += decoder.flush().expect("the drain works").len();

		assert_eq!(decoded, PICTURES, "the stream lost pictures");
	}

	/// A reordered picture carries the timestamp of the access unit it was coded
	/// in, not of the call it fell out of. The two differ by several frames once
	/// B slices are in play, so stamping a picture with the call's timestamp
	/// scrambles playout.
	#[test]
	fn reordered_pictures_keep_their_own_timestamps() {
		let mut decoder = open();
		let mut timestamps = Vec::new();
		for (index, (payload, keyframe)) in access_units().into_iter().enumerate() {
			for frame in decoder.decode(payload, at(index), keyframe).expect("decodes") {
				timestamps.push(frame.timestamp.as_micros());
			}
		}
		for frame in decoder.flush().expect("the drain works") {
			timestamps.push(frame.timestamp.as_micros());
		}

		let expected: Vec<u128> = (0..PICTURES).map(|i| at(i).as_micros()).collect();
		assert_eq!(timestamps, expected, "pictures came out mis-stamped");
	}

	/// A truncated access unit costs the picture, not the stream: the decoder
	/// keeps taking access units and the next keyframe restores the picture.
	#[test]
	fn a_truncated_access_unit_costs_pictures_not_the_stream() {
		let mut decoder = open();
		let units = access_units();
		let broken = 5;
		let mut decoded = 0;
		let mut recovered = 0;
		for (index, (payload, keyframe)) in units.into_iter().enumerate() {
			let payload = if index == broken {
				payload.slice(..payload.len() / 3)
			} else {
				payload
			};
			let frames = decoder
				.decode(payload, at(index), keyframe)
				.unwrap_or_else(|e| panic!("access unit {index} ended the stream: {e}"))
				.len();
			decoded += frames;
			// The fixture's second keyframe is at 15, so anything after it is the
			// decoder having carried on rather than pictures buffered before the
			// break.
			if index > broken {
				recovered += frames;
			}
		}
		assert!(decoded > 0, "nothing decoded at all");
		assert!(recovered > 0, "the decoder never recovered from the truncated unit");
	}

	/// A refused access unit is removed from the pending timestamps by value. In
	/// decode order it may be newer than reordered pictures still in the decoder,
	/// so popping the oldest would shift every later picture's presentation time.
	#[test]
	fn a_truncated_access_unit_does_not_shift_later_timestamps() {
		let clean = decode_fixture(None);
		let expected: HashMap<Vec<u8>, Timestamp> =
			clean.iter().map(|frame| (pixels(frame), frame.timestamp)).collect();
		assert_eq!(expected.len(), clean.len(), "fixture pictures are not unique");

		let broken = 5;
		let mut recovered = 0;
		for frame in decode_fixture(Some(broken)) {
			let Some(timestamp) = expected.get(&pixels(&frame)) else {
				continue;
			};
			assert_eq!(
				frame.timestamp, *timestamp,
				"a surviving picture was stamped as a lost one"
			);
			if *timestamp >= at(15) {
				recovered += 1;
			}
		}
		assert!(recovered > 0, "the second GOP never recovered clean pictures");
	}

	/// A malformed stream can code pictures OpenH264 never returns. The bound on
	/// pending timestamps keeps the oldest candidates and drops the newest one.
	#[test]
	fn pending_limit_preserves_the_next_picture() {
		let mut decoder = open();
		for index in 0..=MAX_PENDING {
			decoder.picture_in(at(index));
		}

		let timestamps: Vec<_> = std::iter::from_fn(|| decoder.picture_out()).collect();
		let expected: Vec<_> = (0..MAX_PENDING).map(at).collect();
		assert_eq!(timestamps, expected);
	}
}
