/// Whether a packet carried coded audio, or none at all.
///
/// Rides along with the audio it describes: on [`Frame`](crate::Frame) coming
/// out of [`decode`](crate::decode), and on
/// [`encode::Encoded`](crate::encode::Encoded) going in. Codecs without a
/// discontinuous mode (PCM, AAC) are always [`Active`](Self::Active).
///
/// Read off the packet, so a consumer gets the same answer as the publisher and
/// gets one for senders that are not us. Two things follow from that, and both
/// are why this reports what arrived rather than what the speaker was doing:
///
/// - Opus marks withheld audio but not silence itself. A run of
///   [`Dtx`](Self::Dtx) is interrupted every few hundred milliseconds by an
///   ordinarily coded frame of the silence, which reads [`Active`](Self::Active)
///   because nothing distinguishes it from speech resuming. So audio is never
///   reported as silence, but silence is reported as audio a frame at a time.
///   Hold a talking indicator across the gap rather than following it frame by
///   frame.
/// - A frame that codes nothing usually means the sender withheld it, but RFC
///   6716 section 3.2.1 lets one stand for a frame that went missing on the way
///   instead. A relay that repacketizes loss that way reads as
///   [`Dtx`](Self::Dtx).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Activity {
	/// The packet coded audio for this frame.
	#[default]
	Active,
	/// The packet coded no audio for this frame, which the sender does while its
	/// input is silent.
	Dtx,
}

impl Activity {
	/// Whether the packet coded audio for this frame.
	pub fn is_active(self) -> bool {
		matches!(self, Self::Active)
	}

	/// Whether the packet coded no audio for this frame.
	pub fn is_dtx(self) -> bool {
		matches!(self, Self::Dtx)
	}
}
