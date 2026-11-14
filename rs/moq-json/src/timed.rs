use moq_net::Timestamp;

/// A value to publish or read, and its frame's timestamp.
///
/// On a timed track `at` is when the value was captured. A scale-less track has no time, so
/// there it is [`Timestamp::unscaled`] at a group key: a write that opens a group starts it
/// at that key, which must pass every group the track holds, as
/// [`append_group`](moq_net::track::Producer::append_group) requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timed<P> {
	/// The value.
	pub value: P,

	/// The frame's timestamp: its capture time, or its group key on a scale-less track.
	pub at: Timestamp,
}

impl<P> Timed<P> {
	/// `value` at `at`.
	pub fn new(value: P, at: Timestamp) -> Self {
		Self { value, at }
	}
}
