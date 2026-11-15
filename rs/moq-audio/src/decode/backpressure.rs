use std::time::Duration;

use kio::clock::Instant;

/// Decides when [`Consumer::read`](super::Consumer::read) decodes a packet, which is how a
/// player splits its playout delay between decoded PCM in its playback ring and encoded
/// media held in the consumer.
///
/// The ring holds the floor as PCM. The rest of the delay, the hold, stays encoded, so
/// memory follows the bytes actually received rather than the delay. The consumer keeps
/// reading packets as they arrive, so its delay estimate still times each one, and
/// decodes a packet at `timestamp`:
///
///   - once the media queued behind it covers the hold, or nothing more continues it
///     (a gap, a declared end, or the track's end is queued after it). A backlog past
///     the delay still reaches the ring this way, to be caught up;
///   - while the playhead is known, also once it is within the floor of where the
///     packet's output ends, keeping the ring on its floor when the network runs late.
///
/// With no hold every packet decodes at once and the ring bounds itself. The playhead is
/// unknown until the first [`observe`](Self::observe), and again after each gap or end
/// the consumer hands over, until an observation lands past it.
///
/// Dropping the handle stops the gating, decoding every packet as it arrives.
pub struct Backpressure {
	state: kio::Producer<State>,
}

#[derive(Debug, Default)]
pub(super) struct State {
	pub floor: Duration,
	pub hold: Duration,
	/// Media at the first value sounds at the second, advancing in real time from there.
	pub playhead: Option<(Duration, Instant)>,
	/// Bumped on every update, so a waiting read notices one.
	pub version: u64,
}

impl Backpressure {
	pub(super) fn new() -> (Self, kio::Consumer<State>) {
		let state = kio::Producer::new(State::default());
		let consumer = state.consume();
		(Self { state }, consumer)
	}

	/// Split the playout delay: up to `floor` decoded in the ring, and `hold` past it encoded.
	pub fn set_latency(&self, floor: Duration, hold: Duration) {
		self.update(|state| {
			state.floor = floor;
			state.hold = hold;
		});
	}

	/// Note that media at `timestamp`, on the consumer's timeline, sounds at `at`.
	/// An unscaled timestamp carries no time, so it is ignored.
	pub fn observe(&self, timestamp: moq_net::Timestamp, at: Instant) {
		if let Ok(timestamp) = Duration::try_from(timestamp) {
			self.update(|state| state.playhead = Some((timestamp, at)));
		}
	}

	fn update(&self, f: impl FnOnce(&mut State)) {
		if let Ok(mut state) = self.state.write() {
			f(&mut state);
			state.version += 1;
		}
	}
}

impl std::fmt::Debug for Backpressure {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Backpressure").finish_non_exhaustive()
	}
}
