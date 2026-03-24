use std::task::Poll;

use bytes::Bytes;

pub type Timestamp = moq_lite::Timescale<1_000_000>;

/// A media frame with a timestamp and codec-specific payload.
#[derive(Clone, Debug)]
pub struct Frame {
	/// The presentation timestamp for this frame.
	pub timestamp: Timestamp,

	/// The encoded media data for this frame.
	pub payload: Bytes,
}

/// Trait for reading/writing media frames from/to moq-lite groups.
///
/// Different container formats encode timestamps and payloads differently:
/// - Legacy (hang): VarInt timestamp prefix + raw codec bitstream
/// - CMAF: moof+mdat atoms with timestamp in tfdt
pub trait Container {
	type Error: std::error::Error + Send + Sync + Unpin + From<moq_lite::Error>;

	/// Write a frame to a group.
	fn write(&self, group: &mut moq_lite::GroupProducer, frame: &Frame) -> Result<(), Self::Error>;

	/// Poll-read the next frame from a group. Returns None when the group is finished.
	fn poll_read(
		&self,
		group: &mut moq_lite::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Frame>, Self::Error>>;

	/// Read the next frame from a group. Returns None when the group is finished.
	fn read(
		&self,
		group: &mut moq_lite::GroupConsumer,
	) -> impl std::future::Future<Output = Result<Option<Frame>, Self::Error>>
	where
		Self: Sync,
	{
		async { conducer::wait(|waiter| self.poll_read(group, waiter)).await }
	}
}
