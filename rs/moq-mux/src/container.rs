use std::task::Poll;

use bytes::Bytes;

pub type Timestamp = moq_lite::Timescale<1_000_000>;

/// A media frame with a timestamp, payload, and keyframe flag.
#[derive(Clone, Debug)]
pub struct Frame {
	/// The presentation timestamp for this frame.
	pub timestamp: Timestamp,

	/// The encoded media data for this frame.
	pub payload: Bytes,

	/// Whether this frame is a keyframe.
	pub keyframe: bool,
}

/// Trait for reading/writing media frames from/to moq-lite groups.
///
/// A single moq-lite frame may contain multiple media frames (e.g. CMAF fragments
/// contain multiple samples). The write/read methods work with slices accordingly.
///
/// Different container formats encode timestamps and payloads differently:
/// - Legacy (hang): VarInt timestamp prefix + raw codec bitstream, one media frame per moq-lite frame
/// - CMAF: moof+mdat atoms, potentially multiple samples per fragment
pub trait Container {
	type Error: std::error::Error + Send + Sync + Unpin + From<moq_lite::Error>;

	/// Write one or more frames as a single moq-lite frame in the group.
	fn write(&self, group: &mut moq_lite::GroupProducer, frames: &[Frame]) -> Result<(), Self::Error>;

	/// Poll-read the next moq-lite frame, returning decoded media frames.
	/// Returns `Ok(None)` when the group is finished.
	fn poll_read(
		&self,
		group: &mut moq_lite::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Vec<Frame>>, Self::Error>>;

	/// Read the next moq-lite frame, returning decoded media frames.
	/// Returns `None` when the group is finished.
	fn read(
		&self,
		group: &mut moq_lite::GroupConsumer,
	) -> impl std::future::Future<Output = Result<Option<Vec<Frame>>, Self::Error>>
	where
		Self: Sync,
	{
		async { conducer::wait(|waiter| self.poll_read(group, waiter)).await }
	}
}
