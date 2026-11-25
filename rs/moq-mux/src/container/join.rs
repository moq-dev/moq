use std::time::Duration;

/// Rolls an audio track's groups by duration.
///
/// Every audio packet decodes on its own, so nothing in the media says where a group ends. A group
/// per packet costs a QUIC stream per packet, 50 a second at 20 ms, and those tiny streams race each
/// other and spend the peer's stream credit. This says when a group carries its span of audio, on
/// the packet (or container unit) that fills it, so the caller cuts right away and the relay sees the
/// group finish without waiting for the next packet. Packets still forward as they are written.
#[derive(Clone, Copy, Debug)]
pub struct Join {
	span: Duration,
	/// Where the open group starts, or `None` when the next media opens one.
	start: Option<Duration>,
}

impl Join {
	/// How much audio a group carries by default: a new group (one QUIC stream) every 100 ms, about
	/// as much as a player conceals, so a lost group is concealed rather than half silent.
	pub const DEFAULT: Duration = Duration::from_millis(100);

	/// Roll groups once they carry `span` of audio. A zero span fills every group with its first packet.
	pub fn new(span: Duration) -> Self {
		Self { span, start: None }
	}

	/// Count media from `start` to `end` into the open group, opening one when none is, and return
	/// whether the group is now full, so the caller cuts it.
	///
	/// Media running backwards past the group's start also fills it, since the timeline broke. An
	/// unscaled timestamp can't be measured, so it fills its group alone.
	pub fn fill(&mut self, start: moq_net::Timestamp, end: moq_net::Timestamp) -> bool {
		let (Ok(start), Ok(end)) = (Duration::try_from(start), Duration::try_from(end)) else {
			self.start = None;
			return true;
		};
		let opened = *self.start.get_or_insert(start);
		let full = self.spans(opened, end);
		if full {
			self.start = None;
		}
		full
	}

	/// Whether media from `start` can't continue the open group, since it starts a whole span past
	/// the group's start (a gap) or before it (a rewind), so the caller cuts before writing it.
	pub fn opens(&self, start: moq_net::Timestamp) -> bool {
		match (self.start, Duration::try_from(start)) {
			(Some(opened), Ok(start)) => self.spans(opened, start),
			(Some(_), Err(_)) => true,
			(None, _) => false,
		}
	}

	/// Whether a group from `start` to `end` is full, for a caller that tracks its own group.
	///
	/// Ending before it starts is full, since the timeline broke, and so is an unscaled timestamp.
	pub fn full(&self, start: moq_net::Timestamp, end: moq_net::Timestamp) -> bool {
		match (Duration::try_from(start), Duration::try_from(end)) {
			(Ok(start), Ok(end)) => self.spans(start, end),
			_ => true,
		}
	}

	fn spans(&self, start: Duration, end: Duration) -> bool {
		end < start || end - start >= self.span
	}

	/// Forget the open group, which the caller closed some other way, such as a discontinuity.
	pub fn reset(&mut self) {
		self.start = None;
	}
}

impl Default for Join {
	fn default() -> Self {
		Self::new(Self::DEFAULT)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn micros(micros: u64) -> moq_net::Timestamp {
		moq_net::Timestamp::from_micros(micros).unwrap()
	}

	/// Feed back-to-back packets of `frame` µs and return how many each group took.
	fn groups(join: &mut Join, frame: u64, count: u64) -> Vec<u64> {
		let mut sizes = vec![0];
		for index in 0..count {
			*sizes.last_mut().unwrap() += 1;
			if join.fill(micros(index * frame), micros((index + 1) * frame)) {
				sizes.push(0);
			}
		}
		sizes.pop_if(|last| *last == 0);
		sizes
	}

	#[test]
	fn the_packet_that_reaches_the_span_fills_the_group() {
		// 20 ms Opus fills a 100 ms group exactly.
		assert_eq!(groups(&mut Join::default(), 20_000, 15), [5, 5, 5]);
		// 1024 samples at 48 kHz never land on 100 ms, so a group takes the packet past it.
		assert_eq!(groups(&mut Join::default(), 21_333, 10), [5, 5]);
		// A unit longer than the span is a group of its own.
		assert_eq!(groups(&mut Join::default(), 500_000, 3), [1, 1, 1]);
		assert_eq!(groups(&mut Join::new(Duration::ZERO), 20_000, 3), [1, 1, 1]);
	}

	#[test]
	fn a_reset_restarts_the_span() {
		let mut join = Join::default();
		assert!(!join.fill(micros(0), micros(80_000)));
		join.reset();
		// Measured from the media after the reset, not the group the caller closed.
		assert!(!join.fill(micros(1_000_000), micros(1_050_000)));
		assert!(join.fill(micros(1_050_000), micros(1_100_000)));
	}

	#[test]
	fn a_gap_or_rewind_opens_a_group() {
		let mut join = Join::default();
		assert!(!join.opens(micros(1_000_000)), "no group is open");
		assert!(!join.fill(micros(1_000_000), micros(1_020_000)));
		assert!(!join.opens(micros(1_020_000)));
		assert!(join.opens(micros(1_100_000)));
		assert!(join.opens(micros(999_999)));
	}

	#[test]
	fn a_gap_or_rewind_fills_the_group() {
		let mut join = Join::default();
		assert!(!join.fill(micros(1_000_000), micros(1_020_000)));
		// A jump ahead reaches the span at once.
		assert!(join.fill(micros(5_000_000), micros(5_020_000)));
		assert!(!join.fill(micros(5_020_000), micros(5_040_000)));
		// A rewind closes the group it can't continue.
		assert!(join.fill(micros(0), micros(20_000)));
		assert!(join.fill(moq_net::Timestamp::unscaled(3), moq_net::Timestamp::unscaled(4)));
	}
}
