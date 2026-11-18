/// Where an importer's input stands, checked before any call touches a track.
#[derive(Debug, Default)]
pub(crate) enum Input {
	/// Accepting input.
	#[default]
	Open,
	/// A chunk was refused part way, so no later chunk can continue it. `finish` still ends the
	/// tracks with what was accepted.
	Refused(crate::Error),
	/// A chunk was refused for going back on a track's timeline, which no later chunk to this
	/// importer can fix: a restarted source is a new broadcast. Otherwise as [`Refused`](Self::Refused).
	Rewound(crate::Error),
	/// Finishing failed, ending every track: every later call reports this cause.
	Failed(crate::Error),
	/// `finish` ended every track.
	Finished,
}

impl Input {
	/// Refuse the call unless the input is still open.
	pub(crate) fn check(&self) -> crate::Result<()> {
		match self {
			Self::Open => Ok(()),
			Self::Refused(err) | Self::Rewound(err) | Self::Failed(err) => Err(err.clone()),
			Self::Finished => Err(crate::Error::Finished),
		}
	}
}
