use crate::Error;

/// Speaker meaning and interleaving order for PCM channels.
///
/// Named layouts interleave in the SMPTE/WAVE order (front left, front right,
/// center, LFE, back left, back right, back center, side left, side right),
/// keeping only the speakers the layout has. Every decoder reorders its codec's
/// native order into this one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Layout {
	/// One center channel.
	Mono,
	/// Left then right channels.
	#[default]
	Stereo,
	/// Channels with no declared speaker positions, in source order.
	///
	/// Passes through unchanged but can't be remixed into another layout.
	Discrete(u32),
	/// 2.1: left, right, LFE.
	TwoPointOne,
	/// 3.0: left, right, center.
	ThreePointZero,
	/// Quad: left, right, back left, back right.
	Quad,
	/// 4.0: left, right, center, back center.
	FourPointZero,
	/// 5.0: left, right, center, side left, side right.
	FivePointZero,
	/// 5.1: left, right, center, LFE, side left, side right.
	FivePointOne,
	/// 6.1: left, right, center, LFE, back center, side left, side right.
	SixPointOne,
	/// 7.1: left, right, center, LFE, back left, back right, side left, side right.
	SevenPointOne,
}

/// One speaker position, declared in the canonical interleaving order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Speaker {
	FrontLeft,
	FrontRight,
	FrontCenter,
	Lfe,
	BackLeft,
	BackRight,
	BackCenter,
	SideLeft,
	SideRight,
}

impl Layout {
	/// The default layout for a channel count, by the WAVE convention: 1 is mono,
	/// 2 stereo, 3 2.1, 4 quad, 5 5.0, 6 5.1, 7 6.1, and 8 7.1.
	///
	/// Any other count is [`Discrete`](Self::Discrete), since there is no
	/// convention to take speaker positions from.
	pub fn from_channels(channels: u32) -> Result<Self, Error> {
		match channels {
			0 => Err(Error::Unsupported(
				"audio layout must contain at least one channel".into(),
			)),
			1 => Ok(Self::Mono),
			2 => Ok(Self::Stereo),
			3 => Ok(Self::TwoPointOne),
			4 => Ok(Self::Quad),
			5 => Ok(Self::FivePointZero),
			6 => Ok(Self::FivePointOne),
			7 => Ok(Self::SixPointOne),
			8 => Ok(Self::SevenPointOne),
			channels => Ok(Self::Discrete(channels)),
		}
	}

	/// Number of interleaved channels in this layout.
	pub fn channels(self) -> u32 {
		match self {
			Self::Discrete(channels) => channels,
			layout => layout.speakers().map_or(0, <[_]>::len) as u32,
		}
	}

	/// The speaker each channel feeds, in interleaved order, or `None` for a
	/// discrete layout.
	pub(crate) fn speakers(self) -> Option<&'static [Speaker]> {
		use Speaker::*;

		Some(match self {
			Self::Mono => &[FrontCenter],
			Self::Stereo => &[FrontLeft, FrontRight],
			Self::Discrete(_) => return None,
			Self::TwoPointOne => &[FrontLeft, FrontRight, Lfe],
			Self::ThreePointZero => &[FrontLeft, FrontRight, FrontCenter],
			Self::Quad => &[FrontLeft, FrontRight, BackLeft, BackRight],
			Self::FourPointZero => &[FrontLeft, FrontRight, FrontCenter, BackCenter],
			Self::FivePointZero => &[FrontLeft, FrontRight, FrontCenter, SideLeft, SideRight],
			Self::FivePointOne => &[FrontLeft, FrontRight, FrontCenter, Lfe, SideLeft, SideRight],
			Self::SixPointOne => &[FrontLeft, FrontRight, FrontCenter, Lfe, BackCenter, SideLeft, SideRight],
			Self::SevenPointOne => &[
				FrontLeft,
				FrontRight,
				FrontCenter,
				Lfe,
				BackLeft,
				BackRight,
				SideLeft,
				SideRight,
			],
		})
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn counts_map_to_the_wave_defaults() {
		let expected = [
			Layout::Mono,
			Layout::Stereo,
			Layout::TwoPointOne,
			Layout::Quad,
			Layout::FivePointZero,
			Layout::FivePointOne,
			Layout::SixPointOne,
			Layout::SevenPointOne,
		];
		for (count, layout) in (1..).zip(expected) {
			assert_eq!(Layout::from_channels(count).unwrap(), layout);
			assert_eq!(layout.channels(), count);
		}

		assert_eq!(Layout::from_channels(9).unwrap(), Layout::Discrete(9));
		assert!(Layout::from_channels(0).is_err());
	}

	/// Canonical order is the WAVE bit order, so every layout's speakers must
	/// ascend through [`Speaker`]'s declaration order without repeating.
	#[test]
	fn speakers_follow_the_canonical_order() {
		let layouts = [
			Layout::Mono,
			Layout::Stereo,
			Layout::TwoPointOne,
			Layout::ThreePointZero,
			Layout::Quad,
			Layout::FourPointZero,
			Layout::FivePointZero,
			Layout::FivePointOne,
			Layout::SixPointOne,
			Layout::SevenPointOne,
		];
		for layout in layouts {
			let speakers = layout.speakers().unwrap();
			assert!(
				speakers.windows(2).all(|pair| (pair[0] as u8) < (pair[1] as u8)),
				"{layout:?} is out of order"
			);
		}
	}
}
