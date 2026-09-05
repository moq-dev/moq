//! Hardware H.264 decode via the V4L2 stateful M2M decoder (Linux).
//!
//! The mirror of the encode backend, on the same device abstraction and usually
//! on a sibling node of the same driver: a Raspberry Pi decodes on
//! `/dev/video10` and encodes on `/dev/video11`, both `bcm2835-codec`. Behind
//! the same default-on `v4l2` feature.
//!
//! Access units go in on the OUTPUT queue in decode order and pictures come back
//! on CAPTURE as CPU [`Surface::I420`](crate::Surface). The driver copies each
//! input buffer's timestamp onto the picture it produced, so presentation times
//! survive decoder delay without any bookkeeping here.
//!
//! Stateful is the operative word. The driver parses the stream itself and
//! announces the picture size with a `V4L2_EVENT_SOURCE_CHANGE`, which is why
//! the CAPTURE queue does not exist until the first parameter sets have been
//! fed: [`Backend::decode`] returns no frames until it arrives, which is the
//! buffering the trait already allows for. The same event later means the stream
//! changed size, and the CAPTURE queue is torn down and renegotiated.
//!
//! Only H.264, and only stateful. A Raspberry Pi 4's separate HEVC block and
//! Rockchip's `rkvdec` are *stateless* V4L2 decoders: they take per-slice
//! parameters through the media request API and expect userspace to have parsed
//! the bitstream, which is a different interface rather than another format
//! here. [`Config::resize`] is ignored, since these drivers scale on a separate
//! ISP node rather than on the decoder.
//!
//! Run on a Raspberry Pi 4 (`bcm2835-codec`, `/dev/video10`): a 720p H.264
//! stream decodes and plays at a steady 30fps. The decoder holds several
//! hundred milliseconds of pictures in its own queue, measured at about 690ms
//! end to end against a software decoder on the same Pi, so a caller that
//! cares about delay more than CPU may prefer openh264 there. The ioctl
//! sequence follows the kernel's stateful decoder documentation and an
//! implementation that also ran on a Pi Zero 2 W and a Pi 3.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use bytes::Bytes;
use moq_net::Timestamp;
use v4l::v4l_sys::{V4L2_CID_MIN_BUFFERS_FOR_CAPTURE, V4L2_DEC_CMD_START, V4L2_DEC_CMD_STOP};

use super::{Backend, Codec, Config};
use crate::v4l2::{self, Dequeue, Device, Dir, Format, Planes, Queue, Rect, Request, Role};
use crate::{Error, Frame, Size, Surface};

pub(crate) const NAME: &str = "v4l2";

/// Which node decodes: one that takes H.264 in and gives raw 4:2:0 back.
const ROLE: Role = Role {
	env: "MOQ_V4L2_DECODER",
	input: &[v4l2::H264],
	output: v4l2::RAW,
};

/// Access units the driver may hold at once.
const CODED_BUFFERS: u32 = 4;

/// Bytes to reserve per access unit. The catalog does not reach a backend, so
/// there is no resolution to size this from, and one megabyte clears a 1080p
/// keyframe at any bitrate worth sending over a network.
const CODED_SIZE: u32 = 1024 * 1024;

/// Pictures to allocate beyond the driver's stated minimum, so a caller holding
/// a frame or two does not stall decoding.
const SPARE_PICTURES: u32 = 3;

/// Pictures to allocate when the driver will not say what it needs.
const DEFAULT_PICTURES: u32 = 8;

/// How long [`Backend::decode`] waits for the driver to take an access unit
/// before giving up on it.
const BUFFER_TIMEOUT: Duration = Duration::from_millis(500);

/// How long a call waits for the source change that follows the first parameter
/// sets. Missing it is not an error on its own: a subscriber can join anywhere
/// in a group, so the first access units of a stream may well be undecodable
/// and the next one gets another look.
const SOURCE_CHANGE_TIMEOUT: Duration = Duration::from_millis(250);

/// How long the decoder spends waiting for that source change in total before
/// giving up on the stream.
///
/// Without a ceiling, a stream the driver can never size costs every call the
/// whole of [`SOURCE_CHANGE_TIMEOUT`] and returns nothing, which is a permanent
/// four frames a second with no error and no log. Twenty access units is far
/// more than a driver needs, since [`Decoder`](crate::decode::Decoder) holds
/// everything back until the first keyframe and the parameter sets that ride
/// with it.
const SOURCE_CHANGE_LIMIT: Duration = Duration::from_secs(5);

/// How long a resolution change waits for the driver to hand back the pictures
/// it decoded before it. Missing the tail costs those pictures but not the
/// stream, so it is bounded rather than waited out.
const DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

/// How long each wait inside those loops parks for.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Access units remembered for the pictures they will become. Well past what
/// any driver holds, so a picture the driver never produces costs a slot and not
/// the stream.
const REMEMBERED: usize = 64;

pub(crate) struct V4l2 {
	device: Device,
	/// The OUTPUT queue: access units going in.
	coded: Queue,
	/// The CAPTURE queue, which exists only once the driver has reported the
	/// stream's size.
	pictures: Option<Pictures>,
	/// When the first access unit went in, which is what
	/// [`SOURCE_CHANGE_LIMIT`] is measured from.
	since: Option<Instant>,
	/// Access units the driver has taken, by the key their picture will carry,
	/// with the timestamp to give that picture. The buffer carries whole
	/// microseconds, and a [`Timestamp`] is an instant at its own scale, which a
	/// round trip through microseconds would change.
	submitted: VecDeque<(Duration, Timestamp)>,
}

/// The CAPTURE queue plus where a picture sits in one of its buffers.
struct Pictures {
	queue: Queue,
	planes: Planes,
}

impl V4l2 {
	pub(crate) fn open(codec: Codec, _config: &Config) -> Result<Box<dyn Backend>, Error> {
		if codec != Codec::H264 {
			return Err(Error::UnsupportedCodec(format!("{NAME} decodes H.264 only")));
		}

		let device = v4l2::open(&ROLE)?;
		// Before the first access unit, so the announcement of the stream's size
		// cannot be missed.
		device.subscribe_source_change()?;

		let coded = device.set_format(
			Dir::Output,
			&Request {
				pixelformat: v4l2::H264,
				// The driver reads the real dimensions out of the bitstream. This is
				// only a hint for how large a coded buffer to allocate, which the
				// explicit `sizeimage` overrides anyway.
				size: Size::new(1920, 1088),
				sizeimage: Some(CODED_SIZE),
				color: None,
			},
		)?;
		if coded.pixelformat != v4l2::H264 {
			return Err(Error::Codec(anyhow::anyhow!(
				"V4L2 decoder answered an H264 request with {}",
				v4l2::name(coded.pixelformat)
			)));
		}

		let mut coded = Queue::alloc(&device, Dir::Output, coded, CODED_BUFFERS)?;
		// Started immediately, unlike the encoder's: a stateful decoder has to be
		// consuming before it can parse a stream and report its size.
		coded.stream_on(&device)?;

		tracing::info!(
			decoder = NAME,
			device = %device.path().display(),
			"opened H.264 decoder"
		);

		Ok(Box::new(Self {
			device,
			coded,
			pictures: None,
			since: None,
			submitted: VecDeque::new(),
		}))
	}

	/// Hand one access unit to the driver, waiting for a free buffer.
	fn submit(&mut self, access_unit: &Bytes, timestamp: Timestamp) -> Result<(), Error> {
		let deadline = Instant::now() + BUFFER_TIMEOUT;
		let index = loop {
			self.reclaim_coded()?;
			if let Some(index) = self.coded.take_free() {
				break index;
			}
			if Instant::now() >= deadline {
				return Err(Error::Codec(anyhow::anyhow!(
					"V4L2 decoder held every input buffer for {BUFFER_TIMEOUT:?}"
				)));
			}
			self.device.wait(POLL_INTERVAL);
		};

		let capacity = self.coded.plane(index, 0).len();
		if access_unit.len() > capacity {
			// The buffer goes back on the free list: losing one to a single oversized
			// access unit would shrink the pool for the rest of the stream.
			self.coded.reclaim(index);
			return Err(Error::Codec(anyhow::anyhow!(
				"access unit of {} bytes exceeds the V4L2 decoder's {capacity} byte buffer",
				access_unit.len()
			)));
		}
		self.coded.plane_mut(index, 0)[..access_unit.len()].copy_from_slice(access_unit);

		let bytesused = [access_unit.len() as u32];
		let key = key(timestamp);
		self.coded.queue(&self.device, index, &bytesused, key)?;
		remember(&mut self.submitted, key, timestamp);
		Ok(())
	}

	/// Reclaim every access-unit buffer the driver has finished reading.
	fn reclaim_coded(&mut self) -> Result<usize, Error> {
		let mut reclaimed = 0;
		while let Some(buffer) = self.coded.dequeue(&self.device)?.buffer() {
			if buffer.failed() {
				tracing::warn!(
					decoder = NAME,
					buffer = buffer.index,
					"V4L2 decoder could not decode an access unit"
				);
			}
			self.coded.reclaim(buffer.index);
			reclaimed += 1;
		}
		Ok(reclaimed)
	}

	/// Negotiate the CAPTURE queue against the size the driver has just reported.
	fn negotiate(&mut self) -> Result<(), Error> {
		if let Some(pictures) = self.pictures.take() {
			pictures.queue.release(&self.device)?;
		}

		let format = self.capture_format()?;
		// Read after the format is settled, since `VIDIOC_S_FMT` on CAPTURE resets
		// the compose rectangle to the default for the format it just took.
		//
		// The coded size is rounded up to whole macroblocks, so the picture is the
		// compose rectangle inside it. A driver that reports none codes exactly the
		// picture.
		let visible = self
			.device
			.visible(Dir::Capture)
			.unwrap_or_else(|| Rect::whole(format.size));
		let planes = Planes::new(&format, visible)?;

		// The driver needs a minimum of its own to hold reference frames; anything
		// below it decodes wrong or not at all.
		let minimum = self
			.device
			.control(V4L2_CID_MIN_BUFFERS_FOR_CAPTURE)
			.map_or(DEFAULT_PICTURES, |minimum| minimum.max(1) as u32 + SPARE_PICTURES);

		tracing::info!(
			decoder = NAME,
			format = v4l2::name(format.pixelformat),
			coded = %format.size,
			visible = %visible.size,
			left = visible.left,
			top = visible.top,
			buffers = minimum,
			"V4L2 decoder negotiated its output"
		);

		let mut queue = Queue::alloc(&self.device, Dir::Capture, format, minimum)?;
		while let Some(index) = queue.take_free() {
			queue.queue(&self.device, index, &[], Duration::ZERO)?;
		}
		queue.stream_on(&self.device)?;

		self.pictures = Some(Pictures { queue, planes });
		Ok(())
	}

	/// Choose the raw format the CAPTURE queue produces.
	///
	/// `VIDIOC_G_FMT` reports the driver's own default, which need not be one this
	/// code can lay out: amphion defaults to the tiled `NV12_8L128` and mtk-vcodec
	/// to `MM21`. That the node offered NV12 at open does not settle it either,
	/// since the set narrows to what the driver supports for the stream it has now
	/// parsed. `VIDIOC_ENUM_FMT` re-reads that set and `VIDIOC_S_FMT` is the step
	/// where userspace picks from it. See
	/// `Documentation/userspace-api/media/v4l/dev-decoder.rst`, "Capture Setup",
	/// steps 3 and 4.
	fn capture_format(&self) -> Result<Format, Error> {
		let format = self.device.format(Dir::Capture)?;
		if v4l2::RAW.contains(&format.pixelformat) {
			return Ok(format);
		}

		let offered = self.device.formats(Dir::Capture)?;
		let Some(&pixelformat) = v4l2::RAW.iter().find(|code| offered.contains(code)) else {
			return Err(Error::Codec(anyhow::anyhow!(
				"V4L2 decoder defaults to {} and offers no 8-bit 4:2:0 format for this stream",
				v4l2::name(format.pixelformat)
			)));
		};

		tracing::debug!(
			decoder = NAME,
			default = v4l2::name(format.pixelformat),
			selected = v4l2::name(pixelformat),
			"V4L2 decoder defaulted to a format this cannot read"
		);
		self.device.set_format(
			Dir::Capture,
			&Request {
				pixelformat,
				// Unchanged from what the driver parsed out of the stream: this has no
				// compose or scaling to ask for.
				size: format.size,
				sizeimage: None,
				color: None,
			},
		)
	}

	/// Collect every picture the driver has finished, re-queueing each buffer as
	/// it is copied out.
	///
	/// Returns whether the sequence ended, which the driver says with
	/// `V4L2_BUF_FLAG_LAST` on its last picture and then with `EPIPE` on any
	/// dequeue past it.
	fn drain(&mut self, frames: &mut Vec<Frame>) -> Result<bool, Error> {
		let Some(pictures) = &self.pictures else {
			return Ok(false);
		};

		loop {
			let buffer = match pictures.queue.dequeue(&self.device)? {
				Dequeue::Buffer(buffer) => buffer,
				Dequeue::Empty => return Ok(false),
				Dequeue::Ended => return Ok(true),
			};

			// A zero-length picture is the driver marking the end of a sequence
			// rather than a frame, which is what arrives just before a source change.
			let decoded = match buffer.written(0) {
				0 => None,
				// A buffer flagged `V4L2_BUF_FLAG_ERROR` dequeues successfully and holds
				// a picture the driver could not decode, so reading it out would publish
				// garbage under a valid timestamp.
				_ if buffer.failed() => {
					tracing::warn!(
						decoder = NAME,
						buffer = buffer.index,
						"V4L2 decoder flagged a picture bad"
					);
					None
				}
				_ => Some(pictures.planes.read(&pictures.queue, &buffer)?),
			};
			// Back to the driver before anything can fail: a picture buffer left out
			// of the pool is one the decoder never gets to write again.
			pictures.queue.queue(&self.device, buffer.index, &[], Duration::ZERO)?;

			if let Some(decoded) = decoded {
				let timestamp = restore(&mut self.submitted, buffer.timestamp)?;
				frames.push(Frame::new(Surface::I420(decoded), timestamp));
			}
			if buffer.last() {
				return Ok(true);
			}
		}
	}

	/// Take the pictures still in the CAPTURE queue when a sequence ends.
	///
	/// A source change is an implicit drain: the driver decodes everything from
	/// before the change, marks the last of it with `V4L2_BUF_FLAG_LAST`, and only
	/// then is the CAPTURE queue free to be torn down. Releasing it as soon as the
	/// event arrives discards every picture the driver had already decoded, which
	/// is a visible gap at each mid-stream resolution change. See
	/// `Documentation/userspace-api/media/v4l/dev-decoder.rst`, "Dynamic Resolution
	/// Change".
	fn drain_tail(&mut self) -> Result<Vec<Frame>, Error> {
		let mut frames = Vec::new();
		let deadline = Instant::now() + DRAIN_TIMEOUT;
		loop {
			if self.drain(&mut frames)? {
				return Ok(frames);
			}
			if Instant::now() >= deadline {
				// Renegotiated anyway: a tail the driver never finished is worth less
				// than the rest of the stream, and holding the old queue open does not
				// make it arrive.
				tracing::warn!(
					decoder = NAME,
					frames = frames.len(),
					"V4L2 decoder did not end the sequence within {DRAIN_TIMEOUT:?}"
				);
				return Ok(frames);
			}
			self.device.wait(POLL_INTERVAL);
		}
	}

	/// Drive an explicit drain until CAPTURE reaches LAST and OUTPUT is reclaimed.
	///
	/// Returns whether a source change arrived during the drain. That event stops
	/// the decoder at the old format, so the caller must renegotiate before it can
	/// continue through any access units queued for the new format.
	fn drain_sequence(&mut self) -> Result<(Vec<Frame>, bool), Error> {
		let mut frames = Vec::new();
		let mut ended = false;
		let mut changed = false;
		let mut deadline = Instant::now() + DRAIN_TIMEOUT;
		loop {
			let before = frames.len();
			let reclaimed = self.reclaim_coded()?;
			changed |= self.device.take_source_change();
			if !ended {
				ended = self.drain(&mut frames)?;
			}

			// A source change stops the old sequence at LAST while access units for
			// the new format may remain on OUTPUT. Renegotiation is what lets those
			// continue, so do not wait here for buffers the stopped decoder cannot
			// return yet.
			if ended && (changed || self.coded.outstanding() == 0) {
				return Ok((frames, changed));
			}

			if reclaimed > 0 || frames.len() > before {
				deadline = Instant::now() + DRAIN_TIMEOUT;
			} else if Instant::now() >= deadline {
				return Err(Error::Codec(anyhow::anyhow!(
					"V4L2 decoder did not finish its drain within {DRAIN_TIMEOUT:?}, holding {} access unit(s)",
					self.coded.outstanding()
				)));
			}
			self.device.wait(POLL_INTERVAL);
		}
	}
}

/// The `struct timeval` an access unit rides the driver under, which the
/// driver copies onto the picture it becomes.
fn key(timestamp: Timestamp) -> Duration {
	Duration::from_micros(timestamp.as_micros() as u64)
}

/// Record an access unit handed to the driver, forgetting the oldest once
/// [`REMEMBERED`] are outstanding.
fn remember(submitted: &mut VecDeque<(Duration, Timestamp)>, key: Duration, timestamp: Timestamp) {
	if submitted.len() == REMEMBERED {
		submitted.pop_front();
	}
	submitted.push_back((key, timestamp));
}

/// The timestamp the access unit behind a picture went in with.
///
/// Falls back to the buffer's own when the driver stamped a picture with
/// nothing that was submitted, which is the best that can be said about it.
fn restore(submitted: &mut VecDeque<(Duration, Timestamp)>, key: Duration) -> Result<Timestamp, Error> {
	// Searched rather than popped: a decoder is free to hand pictures back in
	// presentation order, which is not the order they went in.
	let found = submitted.iter().position(|(at, _)| *at == key);
	match found.and_then(|at| submitted.remove(at)) {
		Some((_, timestamp)) => Ok(timestamp),
		None => Ok(Timestamp::from_micros(key.as_micros() as u64)?),
	}
}

impl Backend for V4l2 {
	fn decode(&mut self, access_unit: Bytes, timestamp: Timestamp, _keyframe: bool) -> Result<Vec<Frame>, Error> {
		self.submit(&access_unit, timestamp)?;

		// Before the first negotiation there is nowhere for a picture to go, so the
		// event is worth waiting for. After it, checking costs one ioctl and a
		// missed resolution change would decode the rest of the stream at the old
		// geometry.
		let mut frames = Vec::new();
		if self.pictures.is_none() {
			let since = *self.since.get_or_insert_with(Instant::now);
			let deadline = Instant::now() + SOURCE_CHANGE_TIMEOUT;
			while !self.device.take_source_change() {
				if Instant::now() >= deadline {
					let waited = since.elapsed();
					if waited >= SOURCE_CHANGE_LIMIT {
						return Err(Error::Codec(anyhow::anyhow!(
							"V4L2 decoder did not report the stream's size within {waited:?}"
						)));
					}
					tracing::debug!(
						decoder = NAME,
						?waited,
						"V4L2 decoder has not reported the stream's size"
					);
					return Ok(Vec::new());
				}
				self.device.wait(POLL_INTERVAL);
			}
			self.negotiate()?;
		} else if self.device.take_source_change() {
			// The pictures decoded before the change are still in the CAPTURE queue,
			// and the kernel wants them taken before the queue is released.
			frames = self.drain_tail()?;
			self.negotiate()?;
		}

		self.drain(&mut frames)?;
		Ok(frames)
	}

	fn flush(&mut self) -> Result<Vec<Frame>, Error> {
		// A drain only begins with both queues streaming. Before the driver has
		// reported a format there is no CAPTURE queue and therefore no picture it
		// can hand back.
		if self.pictures.is_none() {
			// OUTPUT may still hold undecodable access units. Restarting it triggers
			// the stateful decoder's seek/reset sequence and returns those buffers,
			// so none can surface after the next stream's keyframe.
			self.coded.restart(&self.device)?;
			self.submitted.clear();
			self.since = None;
			return Ok(Vec::new());
		}

		let mut frames = Vec::new();
		loop {
			self.device.decoder_cmd(V4L2_DEC_CMD_STOP)?;
			let (tail, changed) = self.drain_sequence()?;
			frames.extend(tail);

			if changed {
				self.negotiate()?;
			}
			self.device.decoder_cmd(V4L2_DEC_CMD_START)?;

			// A source change can leave work for the new format inside the decoder
			// even after every OUTPUT buffer was returned. Resume it, then always run
			// a fresh drain so the flush still covers everything submitted before it.
			if changed {
				continue;
			}
			break;
		}

		self.submitted.clear();
		self.since = None;
		Ok(frames)
	}

	fn name(&self) -> &str {
		NAME
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn micros(micros: u64) -> Timestamp {
		Timestamp::from_micros(micros).unwrap()
	}

	/// A picture comes back with the timestamp its access unit went in with, at
	/// its own scale: the driver carries whole microseconds, and a 90 kHz tick is
	/// not one, so anything rebuilt from the buffer would be a different instant.
	/// Pictures may also come back in presentation rather than submission order.
	#[test]
	fn a_picture_carries_its_access_unit_timestamp_unchanged() {
		let ninety_khz = moq_net::Timescale::new(90_000).unwrap();
		let first = Timestamp::new(3003, ninety_khz).unwrap();
		let second = Timestamp::new(6006, ninety_khz).unwrap();
		assert_ne!(Timestamp::from_micros(first.as_micros() as u64).unwrap(), first);

		let mut submitted = VecDeque::new();
		remember(&mut submitted, key(first), first);
		remember(&mut submitted, key(second), second);

		assert_eq!(restore(&mut submitted, key(second)).unwrap(), second);
		assert_eq!(restore(&mut submitted, key(first)).unwrap(), first);
		assert!(submitted.is_empty());
	}

	/// A picture stamped with nothing that was submitted still gets a timestamp,
	/// the buffer's own, rather than failing the stream.
	#[test]
	fn an_unknown_picture_keeps_the_buffer_timestamp() {
		let mut submitted = VecDeque::new();
		remember(&mut submitted, key(micros(10)), micros(10));
		assert_eq!(restore(&mut submitted, Duration::from_micros(7)).unwrap(), micros(7));
		// The one that was submitted is still waiting for its picture.
		assert_eq!(submitted.len(), 1);
	}

	/// What is remembered is bounded, and it is the oldest that is forgotten.
	#[test]
	fn remembered_access_units_are_bounded() {
		let mut submitted = VecDeque::new();
		for at in 0..=REMEMBERED as u64 {
			remember(&mut submitted, key(micros(at)), micros(at));
		}
		assert_eq!(submitted.len(), REMEMBERED);
		assert_eq!(restore(&mut submitted, key(micros(0))).unwrap(), micros(0));
		assert_eq!(submitted.len(), REMEMBERED);
		assert_eq!(restore(&mut submitted, key(micros(1))).unwrap(), micros(1));
		assert_eq!(submitted.len(), REMEMBERED - 1);
	}
}
