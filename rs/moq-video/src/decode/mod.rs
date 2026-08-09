//! Subscribe to an H.264, H.265, or AV1 track and decode it to raw frames.
//!
//! The decode counterpart to [`encode`](crate::encode), and the mirror of
//! `moq_audio::decode::Consumer`. [`Consumer`] subscribes to a moq-mux video
//! track and hands back decoded [`Frame`](crate::Frame)s; a native backend does the work
//! (VideoToolbox on macOS, Media Foundation / DXVA on Windows, NVDEC on Linux,
//! openh264 everywhere as the software fallback for H.264).
//!
//! H.264 and H.265 are supported, symmetric with what [`encode`](crate::encode)
//! produces. AV1 is decode-only on NVDEC. H.265 and AV1 are hardware-only (no
//! software fallback). Any other codec yields
//! [`Error::UnsupportedCodec`](crate::Error).

// Crate-visible so the NVENC encode backend's round-trip test can decode its
// output with the software decoder (an in-crate, ffmpeg-free encode->decode
// check that catches input-pitch corruption).
pub(crate) mod backend;
mod consumer;
mod decoder;
mod sink;

pub use consumer::Consumer;
pub use decoder::{Config, Decoder, Kind};
pub use sink::Sink;

#[cfg(test)]
mod tests {
	/// Callers (libmoq, moq-transcode) hold these across `.await`s in spawned
	/// tasks and share frames via `Arc` (the transcode fanout), so both must
	/// stay `Send` and `Frame` also `Sync` even when a platform's frame wraps
	/// a GPU handle. Compile-time check; fails per-platform if a variant
	/// regresses.
	#[test]
	fn frame_and_consumer_are_thread_safe() {
		fn assert_send<T: Send>() {}
		fn assert_sync<T: Sync>() {}
		assert_send::<crate::Frame>();
		assert_sync::<crate::Frame>();
		assert_send::<super::Consumer>();
	}
}
