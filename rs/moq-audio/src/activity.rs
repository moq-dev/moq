/// Whether the sender coded audio for a frame, or withheld it because the input
/// was silent (Opus discontinuous transmission).
///
/// Rides along with the audio it describes: on [`Frame`](crate::Frame) coming
/// out of [`decode`](crate::decode), and on
/// [`encode::Encoded`](crate::encode::Encoded) going in. Codecs without a
/// discontinuous mode (PCM, AAC) are always [`Active`](Self::Active).
///
/// Read off the packet, so a consumer gets the same answer as the publisher and
/// gets one for senders that are not us. Opus marks withheld audio but not
/// silence itself: a run of [`Dtx`](Self::Dtx) is interrupted every few hundred
/// milliseconds by an ordinarily coded frame of the silence, which reads
/// [`Active`](Self::Active) because nothing distinguishes it from speech
/// resuming. So this never reports audio as silence, but it does report silence
/// as audio for one frame at a time. Hold a talking indicator across the gap
/// rather than following it frame by frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Activity {
	/// The sender coded audio for this frame.
	#[default]
	Active,
	/// The sender withheld audio for this frame because its input was silent.
	Dtx,
}

impl Activity {
	/// Whether the sender coded audio for this frame.
	pub fn is_active(self) -> bool {
		matches!(self, Self::Active)
	}

	/// Whether the sender withheld audio for this frame.
	pub fn is_dtx(self) -> bool {
		matches!(self, Self::Dtx)
	}
}
