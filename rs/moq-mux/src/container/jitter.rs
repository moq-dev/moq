use crate::container::Timestamp;

/// Tracks the catalog `jitter` for a video/audio track: the maximum delay before a frame can
/// be emitted, so a player sizes its buffer to at least this much.
///
/// It reports whichever is larger of two contributions:
/// - the minimum frame duration (the steady inter-frame spacing), and
/// - the reorder delay (`max(PTS - DTS)`), which is non-zero only for reordered (B-frame)
///   streams and which a transmuxer also reuses as the decode-clock reserve.
///
/// A non-reordered stream reports the frame duration; a B-frame stream reports the deeper
/// reorder delay (e.g. up to 3 consecutive B-frames is 3x the frame duration).
#[derive(Default)]
pub struct Jitter {
	last_timestamp: Option<Timestamp>,
	min_duration: Option<Timestamp>,
	max_reorder: Timestamp,
	/// Last value handed back from [`observe`](Self::observe) /
	/// [`observe_reorder`](Self::observe_reorder), so they only report on a change.
	reported: Option<Timestamp>,
}

impl Jitter {
	pub fn new() -> Self {
		Self::default()
	}

	/// Record a frame's presentation timestamp (decode order), updating the minimum frame
	/// duration. Returns the new jitter as a [`moq_net::Time`] if it changed, else `None`.
	/// The first observation and non-monotonic timestamps (B-frames) only update state.
	pub fn observe(&mut self, ts: Timestamp) -> Option<moq_net::Time> {
		if let Some(last) = self.last_timestamp.replace(ts)
			&& let Ok(duration) = ts.checked_sub(last)
			&& !duration.is_zero()
			&& duration < self.min_duration.unwrap_or(Timestamp::MAX)
		{
			self.min_duration = Some(duration);
		}
		self.report()
	}

	/// Record a frame's reorder delay (`PTS - DTS`), updating the maximum. Returns the new
	/// jitter as a [`moq_net::Time`] if it changed, else `None`.
	pub fn observe_reorder(&mut self, reorder: Timestamp) -> Option<moq_net::Time> {
		self.max_reorder = self.max_reorder.max(reorder);
		self.report()
	}

	/// The current jitter (the larger of the frame duration and the reorder delay), without
	/// the change-detection of [`observe`](Self::observe). Used to seed a freshly created
	/// catalog rendition with whatever has accumulated, since per-frame updates before the
	/// rendition exists would otherwise be lost.
	pub fn current(&self) -> Option<moq_net::Time> {
		let jitter = self.combined();
		(!jitter.is_zero()).then(|| jitter.convert().ok()).flatten()
	}

	fn combined(&self) -> Timestamp {
		self.min_duration.unwrap_or(Timestamp::ZERO).max(self.max_reorder)
	}

	/// Report the current jitter only when it changes.
	fn report(&mut self) -> Option<moq_net::Time> {
		let jitter = self.combined();
		if jitter.is_zero() || self.reported == Some(jitter) {
			return None;
		}
		self.reported = Some(jitter);
		jitter.convert().ok()
	}
}
