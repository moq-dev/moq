//! [`Decoded`]: one packet's worth of PCM on its way out of a codec.

use crate::Activity;

/// One decoded Opus/PCM/AAC packet: interleaved `f32` samples at the codec's own
/// rate and channel count, plus what the codec was doing when it produced them.
///
/// The low-level counterpart to [`encode::Encoded`](crate::encode::Encoded).
/// [`Consumer`](super::Consumer) turns these into [`Frame`](crate::Frame)s in
/// the layout its [`Config`](super::Config) asks for.
#[derive(Clone, Debug)]
pub struct Decoded {
	/// Interleaved samples, at [`Decoder::sample_rate`](super::Decoder::sample_rate).
	pub samples: Vec<f32>,
	/// Whether this packet coded any audio. Always [`Activity::Active`] for
	/// codecs without a discontinuous mode, and for the coded frames that
	/// punctuate a silent run.
	pub activity: Activity,
}
