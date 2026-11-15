//! Encoded playback retention in media time.

use moq_mux::container::Frame;
use std::collections::VecDeque;
use std::time::Duration;

/// Encoded access units stay in decode order, even when their PTS reorders.
pub(super) struct Buffer {
	pub frames: VecDeque<Frame>,
	pub bytes: usize,
	edge: Option<moq_net::Timestamp>,
	// Monotone minimum: PTS can reorder behind the front of decode order.
	oldest: VecDeque<moq_net::Timestamp>,
	pub generation: u64,
	/// The keyframe the buffer last resumed at. A picture presenting before it references
	/// media the buffer never had, so it is neither decoded nor shown.
	pub floor: Option<moq_net::Timestamp>,
	/// Pictures presenting before this are still warming up after a join or a skip:
	/// decoded for their references and never shown. They age out the warmup later
	/// than other media, since they trail the live edge by it on purpose.
	pub present: Option<moq_net::Timestamp>,
	/// Whether the next keyframe resumes after a join or a loss, so a catalog `warmup`
	/// applies. Cleared for an encoder's first picture, which has nothing before it.
	pub joined: bool,
	waiting_keyframe: bool,
	pub ended: bool,
	/// Exclusive endpoint declared for this encoder segment.
	pub end: Option<moq_net::Timestamp>,
}

impl Default for Buffer {
	fn default() -> Self {
		Self {
			frames: VecDeque::new(),
			bytes: 0,
			edge: None,
			oldest: VecDeque::new(),
			generation: 0,
			floor: None,
			present: None,
			joined: true,
			// A join resumes at its first keyframe, like any other break.
			waiting_keyframe: true,
			ended: false,
			end: None,
		}
	}
}

impl Buffer {
	pub fn clear(&mut self) {
		self.frames.clear();
		self.oldest.clear();
		self.bytes = 0;
		self.edge = None;
		self.generation += 1;
		self.waiting_keyframe = true;
		self.joined = true;
	}

	/// The earliest presentation time still buffered, wherever it sits in
	/// decode order.
	pub fn oldest(&self) -> Option<moq_net::Timestamp> {
		self.oldest.front().copied()
	}

	pub fn pop(&mut self) -> Option<Frame> {
		let frame = self.frames.pop_front()?;
		self.bytes -= frame.payload.len();
		if self.oldest.front() == Some(&frame.timestamp) {
			self.oldest.pop_front();
		}
		Some(frame)
	}

	/// Append `frame`, aging out what trails the newest by more than `max_delay`. After a
	/// join or a loss, the pictures before the resuming keyframe plus `warmup` are warming.
	pub fn push(&mut self, frame: Frame, max_delay: Duration, warmup: Duration) {
		let edge = *self.edge.get_or_insert(frame.timestamp);
		// Timestamp has no `Ord`; one that doesn't compare keeps the edge.
		self.edge = Some(if frame.timestamp > edge { frame.timestamp } else { edge });
		if frame.keyframe {
			if self.waiting_keyframe {
				self.floor = Some(frame.timestamp);
				self.present = self
					.floor
					.filter(|_| self.joined)
					.and_then(|floor| warmed(floor, warmup));
			}
			self.waiting_keyframe = false;
		}
		if self.waiting_keyframe || self.below_floor(&frame) {
			return;
		}
		self.index(frame);
		let edge = self.edge.unwrap();
		let present = self.present;
		// An unscaled timestamp has no age, so it never goes stale. A warming one may
		// trail by the warmup too.
		let stale = |oldest: &moq_net::Timestamp| {
			let warming = present.is_some_and(|present| *oldest < present);
			let budget = match warming {
				true => max_delay.saturating_add(warmup),
				false => max_delay,
			};
			match (edge.as_micros(), oldest.as_micros()) {
				(Ok(edge), Ok(oldest)) => edge.saturating_sub(oldest) > budget.as_micros(),
				_ => false,
			}
		};
		if self.oldest.front().is_some_and(stale) {
			// Dropping a reference picture invalidates the rest of its GOP. Resume
			// only at a retained keyframe, never feed a broken chain to the codec.
			self.pop();
			while self.frames.front().is_some_and(|frame| !frame.keyframe) || self.oldest.front().is_some_and(stale) {
				self.pop();
			}
			self.generation += 1;
			self.waiting_keyframe = self.frames.is_empty();
			self.floor = self.frames.front().map(|frame| frame.timestamp);
			// Resuming past a loss warms up again, even at a keyframe that has yet to arrive.
			self.joined = true;
			self.present = self.floor.and_then(|floor| warmed(floor, warmup));
			// The retained keyframe's leading pictures referenced what was just dropped.
			let mut frames = std::mem::take(&mut self.frames);
			frames.retain(|frame| !self.below_floor(frame));
			self.bytes = 0;
			self.oldest.clear();
			for frame in frames {
				self.index(frame);
			}
		}
	}

	/// Append a frame, tracking its bytes and the earliest presentation time.
	fn index(&mut self, frame: Frame) {
		while self.oldest.back().is_some_and(|at| *at > frame.timestamp) {
			self.oldest.pop_back();
		}
		self.oldest.push_back(frame.timestamp);
		self.bytes += frame.payload.len();
		self.frames.push_back(frame);
	}

	fn below_floor(&self, frame: &Frame) -> bool {
		self.floor.is_some_and(|floor| frame.timestamp < floor)
	}
}

/// Where a `warmup` from `floor` ends, at its scale, or `None` when there is none. Rounds
/// up to the next tick, so a coarse scale never hides less than the whole warmup.
fn warmed(floor: moq_net::Timestamp, warmup: Duration) -> Option<moq_net::Timestamp> {
	if warmup.is_zero() {
		return None;
	}
	let scale = floor.scale()?;
	let ticks = warmup
		.as_nanos()
		.checked_mul(u128::from(scale.as_u64()))?
		.div_ceil(1_000_000_000);
	let warmup = moq_net::Timestamp::new(u64::try_from(ticks).ok()?, scale).ok()?;
	floor.checked_add(warmup).ok()
}

#[cfg(test)]
mod tests {
	use super::*;

	fn frame(ms: u64, keyframe: bool) -> Frame {
		Frame {
			timestamp: moq_net::Timestamp::from_millis(ms).unwrap(),
			duration: None,
			payload: bytes::Bytes::from_static(b"encoded"),
			keyframe,
		}
	}

	#[test]
	fn eviction_keeps_only_decodable_media_inside_the_age_window() {
		let mut buffer = Buffer::default();
		let age = Duration::from_millis(100);
		buffer.push(frame(0, true), age, Duration::ZERO);
		buffer.push(frame(50, false), age, Duration::ZERO);
		buffer.push(frame(100, true), age, Duration::ZERO);
		buffer.push(frame(150, false), age, Duration::ZERO);
		assert_eq!(buffer.frames.front().unwrap().timestamp.as_millis().unwrap(), 100);
		assert_eq!(buffer.bytes, 14);
		buffer.pop();
		assert_eq!(buffer.bytes, 7);
		buffer.push(frame(260, false), age, Duration::ZERO);
		assert!(buffer.frames.is_empty(), "no retained keyframe");
		assert_eq!(buffer.bytes, 0);
		buffer.push(frame(280, false), age, Duration::ZERO);
		assert!(buffer.frames.is_empty(), "cannot resume on a dependent picture");
		buffer.push(frame(300, true), age, Duration::ZERO);
		assert_eq!(buffer.bytes, 7);
	}

	#[test]
	fn eviction_drops_the_retained_keyframes_leading_pictures() {
		let mut buffer = Buffer::default();
		let age = Duration::from_millis(100);
		for (ms, keyframe) in [
			(0, true),
			(50, false),
			(100, true),
			(80, false),
			(90, false),
			(110, false),
		] {
			buffer.push(frame(ms, keyframe), age, Duration::ZERO);
		}
		buffer.push(frame(95, false), age, Duration::ZERO);
		let kept: Vec<_> = buffer.frames.iter().map(|f| f.timestamp.as_millis().unwrap()).collect();
		assert_eq!(kept, [100, 110], "80, 90 and 95 reference the evicted group");
		assert_eq!(buffer.bytes, 14);
		assert_eq!(buffer.oldest().unwrap().as_millis().unwrap(), 100);
		buffer.push(frame(200, true), age, Duration::ZERO);
		buffer.push(frame(180, false), age, Duration::ZERO);
		assert_eq!(buffer.frames.len(), 4, "a continuous group keeps its leading pictures");
	}

	#[test]
	fn reordered_timestamps_keep_decode_order_and_the_newest_edge() {
		let mut buffer = Buffer::default();
		for (ms, keyframe) in [(0, true), (99, false), (33, false), (66, false)] {
			buffer.push(frame(ms, keyframe), Duration::from_millis(100), Duration::ZERO);
		}
		assert_eq!(
			buffer
				.frames
				.iter()
				.map(|f| f.timestamp.as_millis().unwrap())
				.collect::<Vec<_>>(),
			[0, 99, 33, 66]
		);
		assert_eq!(buffer.edge.unwrap().as_millis().unwrap(), 99);
		buffer.clear();
		assert_eq!(buffer.bytes, 0);
		assert!(buffer.frames.is_empty());
		buffer.push(frame(1, true), Duration::ZERO, Duration::ZERO);
		assert_eq!(buffer.edge.unwrap().as_millis().unwrap(), 1);
	}
	#[test]
	fn age_expiry_finds_a_reordered_picture_behind_the_front() {
		let mut buffer = Buffer::default();
		let age = Duration::from_millis(100);
		for (ms, keyframe) in [(0, true), (99, false), (33, false)] {
			buffer.push(frame(ms, keyframe), age, Duration::ZERO);
		}
		buffer.pop();
		buffer.push(frame(150, false), age, Duration::ZERO);
		assert!(
			buffer.frames.is_empty(),
			"the 33ms picture expired behind the 99ms reference"
		);
		assert_eq!(buffer.bytes, 0);
		buffer.push(frame(160, true), age, Duration::ZERO);
		assert_eq!(buffer.frames.len(), 1);
	}

	/// After a join the warmup's pictures trail the live edge on purpose, so they age out the
	/// warmup later than other media, and an eviction past a loss warms up again from the
	/// keyframe it resumes at.
	#[test]
	fn a_join_ages_out_the_warmup_a_warmup_later() {
		let mut buffer = Buffer::default();
		let (age, warmup) = (Duration::from_millis(100), Duration::from_millis(100));
		for ms in [0, 50, 100, 150, 200] {
			buffer.push(frame(ms, ms % 100 == 0), age, warmup);
		}
		let kept: Vec<_> = buffer.frames.iter().map(|f| f.timestamp.as_millis().unwrap()).collect();
		assert_eq!(
			kept,
			[0, 50, 100, 150, 200],
			"the warmup trails by the age plus the warmup"
		);
		assert_eq!(buffer.present.unwrap().as_millis().unwrap(), 100);

		buffer.push(frame(250, false), age, warmup);
		let kept: Vec<_> = buffer.frames.iter().map(|f| f.timestamp.as_millis().unwrap()).collect();
		assert_eq!(
			kept,
			[200, 250],
			"the warmup past its budget was evicted, then the stale group"
		);
		assert_eq!(
			buffer.present.unwrap().as_millis().unwrap(),
			300,
			"resuming past a loss warms up"
		);
	}

	/// An eviction that empties a fresh encoder's segment is a loss: the keyframe it resumes at
	/// warms up.
	#[test]
	fn an_emptied_fresh_segment_warms_up_at_its_next_keyframe() {
		let mut buffer = Buffer {
			joined: false,
			..Buffer::default()
		};
		let warmup = Duration::from_millis(100);
		buffer.push(frame(0, true), Duration::ZERO, warmup);
		assert!(buffer.present.is_none());
		buffer.push(frame(50, false), Duration::ZERO, warmup);
		assert!(buffer.frames.is_empty(), "aged out with no keyframe to resume at");
		buffer.push(frame(300, true), Duration::ZERO, warmup);
		assert_eq!(buffer.present.unwrap().as_millis().unwrap(), 400);
	}

	/// A coarse scale rounds the warmup up a tick rather than hiding less than all of it.
	#[test]
	fn a_coarse_scale_rounds_the_warmup_up() {
		let floor = moq_net::Timestamp::from_scale(10, 10).unwrap();
		let present = warmed(floor, Duration::from_millis(150)).unwrap();
		assert_eq!(present.value(), 12, "150 ms at 10 Hz ends at the second tick");
		let present = warmed(floor, Duration::from_millis(200)).unwrap();
		assert_eq!(present.value(), 12, "an exact warmup does not round");
		assert!(warmed(moq_net::Timestamp::unscaled(3), Duration::from_millis(150)).is_none());
	}

	/// An encoder's first picture has nothing before it to warm up from.
	#[test]
	fn a_fresh_encoder_needs_no_warmup() {
		let mut buffer = Buffer {
			joined: false,
			..Buffer::default()
		};
		buffer.push(frame(0, true), Duration::ZERO, Duration::from_millis(100));
		assert!(buffer.present.is_none());
		buffer.clear();
		buffer.push(frame(500, true), Duration::ZERO, Duration::from_millis(100));
		assert_eq!(buffer.present.unwrap().as_millis().unwrap(), 600, "a loss warms up");
	}
}
