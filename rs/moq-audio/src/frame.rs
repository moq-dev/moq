use bytes::Bytes;

/// One unit of audio passed across the codec boundary.
///
/// Just a payload and a presentation timestamp. PCM layout (format /
/// sample rate / channel count) is fixed by the producer or consumer
/// at construction time, never per frame, so callers can't accidentally
/// drift the format mid-stream.
#[derive(Clone, Debug)]
pub struct Frame {
	/// Presentation timestamp of the first sample, in microseconds.
	pub timestamp_us: u64,
	/// Encoded packet (post-Encoder) or raw PCM bytes (post-Decoder).
	pub data: Bytes,
}
