/// Whether an encoded audio frame carries active codec data or Opus
/// discontinuous-transmission comfort noise.
///
/// Rides along with the audio it describes: on [`Frame`](crate::Frame) coming
/// out of [`decode`](crate::decode), and on
/// [`encode::Encoded`](crate::encode::Encoded) going in. Codecs without a
/// comfort-noise mode (PCM, AAC) are always [`Active`](Self::Active).
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
