//! Encoded playback retention in media time.

use hang::moq_net;
use moq_mux::container::Frame;
use std::collections::VecDeque;
use std::time::Duration;

/// Encoded access units stay in decode order, even when their PTS reorders.
#[derive(Default)]
pub(super) struct Buffer {
	pub frames: VecDeque<Frame>,
	pub bytes: usize,
	edge: Option<moq_net::Timestamp>,
	// Monotone minimum: PTS can reorder behind the front of decode order.
	oldest: VecDeque<moq_net::Timestamp>,
	pub generation: u64,
	pub floor: Option<moq_net::Timestamp>,
	waiting_keyframe: bool,
	pub ended: bool,
}

impl Buffer {
	pub fn clear(&mut self) {
		self.frames.clear();
		self.oldest.clear();
		self.bytes = 0;
		self.edge = None;
		self.generation += 1;
		self.waiting_keyframe = true;
	}

	pub fn pop(&mut self) -> Option<Frame> {
		let frame = self.frames.pop_front()?;
		self.bytes -= frame.payload.len();
		if self.oldest.front() == Some(&frame.timestamp) {
			self.oldest.pop_front();
		}
		Some(frame)
	}

	pub fn push(&mut self, frame: Frame, max_age: Duration) {
		let edge = *self.edge.get_or_insert(frame.timestamp);
		self.edge = Some(edge.max(frame.timestamp));
		if frame.keyframe {
			if self.waiting_keyframe {
				self.floor = Some(frame.timestamp);
			}
			self.waiting_keyframe = false;
		}
		if self.waiting_keyframe {
			return;
		}
		while self.oldest.back().is_some_and(|at| *at > frame.timestamp) {
			self.oldest.pop_back();
		}
		self.oldest.push_back(frame.timestamp);
		self.bytes += frame.payload.len();
		self.frames.push_back(frame);
		let edge = self.edge.unwrap();
		let stale =
			|oldest: &moq_net::Timestamp| edge.as_micros().saturating_sub(oldest.as_micros()) > max_age.as_micros();
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
		}
	}
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
		buffer.push(frame(0, true), age);
		buffer.push(frame(50, false), age);
		buffer.push(frame(100, true), age);
		buffer.push(frame(150, false), age);
		assert_eq!(buffer.frames.front().unwrap().timestamp.as_millis(), 100);
		assert_eq!(buffer.bytes, 14);
		buffer.pop();
		assert_eq!(buffer.bytes, 7);
		buffer.push(frame(260, false), age);
		assert!(buffer.frames.is_empty(), "no retained keyframe");
		assert_eq!(buffer.bytes, 0);
		buffer.push(frame(280, false), age);
		assert!(buffer.frames.is_empty(), "cannot resume on a dependent picture");
		buffer.push(frame(300, true), age);
		assert_eq!(buffer.bytes, 7);
	}

	#[test]
	fn reordered_timestamps_keep_decode_order_and_the_newest_edge() {
		let mut buffer = Buffer::default();
		for (ms, keyframe) in [(0, true), (99, false), (33, false), (66, false)] {
			buffer.push(frame(ms, keyframe), Duration::from_millis(100));
		}
		assert_eq!(
			buffer
				.frames
				.iter()
				.map(|f| f.timestamp.as_millis())
				.collect::<Vec<_>>(),
			[0, 99, 33, 66]
		);
		assert_eq!(buffer.edge.unwrap().as_millis(), 99);
		buffer.clear();
		assert_eq!(buffer.bytes, 0);
		assert!(buffer.frames.is_empty());
		buffer.push(frame(1, true), Duration::ZERO);
		assert_eq!(buffer.edge.unwrap().as_millis(), 1);
	}
	#[test]
	fn age_expiry_finds_a_reordered_picture_behind_the_front() {
		let mut buffer = Buffer::default();
		let age = Duration::from_millis(100);
		for (ms, keyframe) in [(0, true), (99, false), (33, false)] {
			buffer.push(frame(ms, keyframe), age);
		}
		buffer.pop();
		buffer.push(frame(150, false), age);
		assert!(
			buffer.frames.is_empty(),
			"the 33ms picture expired behind the 99ms reference"
		);
		assert_eq!(buffer.bytes, 0);
		buffer.push(frame(160, true), age);
		assert_eq!(buffer.frames.len(), 1);
	}
}
