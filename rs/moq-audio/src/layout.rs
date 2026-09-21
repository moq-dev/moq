use crate::Error;

/// Speaker meaning and interleaving order for PCM channels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Layout {
	/// One center channel.
	Mono,
	/// Left then right channels.
	#[default]
	Stereo,
	/// Channels with no declared speaker positions, in source order.
	Discrete(u32),
}

impl Layout {
	/// Infer today's conventional layout from a channel count.
	pub fn from_channels(channels: u32) -> Result<Self, Error> {
		match channels {
			0 => Err(Error::Unsupported(
				"audio layout must contain at least one channel".into(),
			)),
			1 => Ok(Self::Mono),
			2 => Ok(Self::Stereo),
			channels => Ok(Self::Discrete(channels)),
		}
	}

	/// Number of interleaved channels in this layout.
	pub fn channels(self) -> u32 {
		match self {
			Self::Mono => 1,
			Self::Stereo => 2,
			Self::Discrete(channels) => channels,
		}
	}

	pub(crate) fn validate(self) -> Result<(), Error> {
		if self.channels() == 0 {
			return Err(Error::Unsupported(
				"audio layout must contain at least one channel".into(),
			));
		}
		Ok(())
	}
}
