//! Codec bridges from str0m media frames into moq-mux importers.
//!
//! str0m's Frame API does the RTP reassembly for us and hands back
//! whole codec frames. The job of this module is to convert each
//! frame into the codec-specific shape that the hang catalog and
//! moq-mux importers expect, and to publish the resulting tracks.

pub mod h264;
pub mod opus;
pub mod vp8;
pub mod vp9;

use bytes::Bytes;

use crate::Result;

/// One codec frame received from str0m, paired with a microsecond timestamp.
///
/// The session loop converts str0m's [`MediaTime`](str0m::media::MediaTime)
/// to microseconds so individual bridges don't need to repeat the math.
#[derive(Clone, Debug)]
pub struct Frame {
	pub timestamp_us: u64,
	pub payload: Bytes,
}

/// Bridges depacketized media frames from str0m to a hang broadcast track.
///
/// One bridge per `m=` line. The session loop calls [`Bridge::push`] once per
/// [`MediaData`](str0m::media::MediaData) event with the codec frame; the
/// bridge handles any codec-specific transformations (e.g. Annex-B to AVCC
/// for H.264) and forwards the frame into the matching moq-mux importer.
pub trait Bridge: Send {
	fn push(&mut self, frame: Frame) -> Result<()>;
}
