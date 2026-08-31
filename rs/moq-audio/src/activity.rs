use std::ops::Deref;

/// Whether an encoded audio frame carries active codec data or Opus
/// discontinuous-transmission comfort noise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Activity {
	/// A normally encoded frame.
	#[default]
	Active,
	/// An Opus DTX or comfort-noise frame.
	Dtx,
}

impl Activity {
	/// Whether this frame carries normally encoded audio.
	pub fn is_active(self) -> bool {
		matches!(self, Self::Active)
	}

	/// Whether this frame is Opus DTX or comfort noise.
	pub fn is_dtx(self) -> bool {
		matches!(self, Self::Dtx)
	}
}

/// A produced or consumed audio item and its codec activity classification.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Classified<T> {
	/// The encoded payload, decoded PCM, or published timestamp.
	pub value: T,
	/// The codec activity represented by this item.
	pub activity: Activity,
}

impl<T> Classified<T> {
	pub(crate) fn new(value: T, activity: Activity) -> Self {
		Self { value, activity }
	}

	/// Transform the contained item while preserving its activity.
	pub fn map<U>(self, map: impl FnOnce(T) -> U) -> Classified<U> {
		Classified {
			value: map(self.value),
			activity: self.activity,
		}
	}

	/// Consume the classification and return the contained item.
	pub fn into_inner(self) -> T {
		self.value
	}
}

impl<T> Deref for Classified<T> {
	type Target = T;

	fn deref(&self) -> &Self::Target {
		&self.value
	}
}
