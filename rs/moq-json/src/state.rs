/// Whether a producer still publishes, or how it ended its track.
///
/// Another handle may end the track first, so the track alone can't say what this producer did:
/// its later calls report its own outcome.
#[derive(Default)]
pub(crate) enum State {
	#[default]
	Open,
	Finished,
	/// The first cause wins.
	Aborted(crate::Error),
}

impl State {
	/// Refuse the call once this producer ended the track.
	pub(crate) fn check(&self) -> crate::Result<()> {
		match self {
			Self::Open => Ok(()),
			Self::Finished => Err(moq_net::Error::Closed.into()),
			Self::Aborted(err) => Err(err.clone()),
		}
	}

	/// Record an abort, unless an earlier one already gave the cause.
	pub(crate) fn abort(&mut self, err: crate::Error) {
		if !matches!(self, Self::Aborted(_)) {
			*self = Self::Aborted(err);
		}
	}
}
