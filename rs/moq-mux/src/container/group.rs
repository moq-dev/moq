use std::collections::VecDeque;
use std::task::{Poll, ready};

use super::{Container, Frame};

/// Decode a single [`moq_net::group::Consumer`] into a finite stream of media [`Frame`]s.
///
/// This is the group-scoped counterpart to [`Consumer`](super::Consumer). Where that one
/// subscribes to a track and juggles group ordering, age skipping, and rewinds, this one
/// reads exactly the group it was handed, in arrival order, and ends. That is what a caller
/// wants after a FETCH: a group already chosen by sequence, with no live subscription and no
/// max age budget that could skip the very group being asked for.
///
/// A batch of frames decoded from one wire frame (a CMAF fragment carrying several samples) is
/// handed back one frame at a time.
pub struct GroupConsumer<F: Container> {
	group: moq_net::group::Consumer,
	format: F,

	// Frames decoded from the last wire frame but not yet returned.
	pending: VecDeque<Frame>,

	// How many frames we have returned, so the first one can be marked a keyframe.
	index: u64,
}

impl<F: Container> GroupConsumer<F> {
	/// Decode `group` with the given container format.
	pub fn new(group: moq_net::group::Consumer, format: F) -> Self {
		Self {
			group,
			format,
			pending: VecDeque::new(),
			index: 0,
		}
	}

	/// The sequence number of this group within its track.
	pub fn sequence(&self) -> u64 {
		self.group.sequence
	}

	/// Read the next frame, or `None` once the group ends.
	pub async fn read(&mut self) -> Result<Option<Frame>, F::Error> {
		kio::wait(|waiter| self.poll_read(waiter)).await
	}

	/// Poll for the next frame, without blocking.
	pub fn poll_read(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Frame>, F::Error>> {
		loop {
			if let Some(mut frame) = self.pending.pop_front() {
				// First frame of a group is always a keyframe by protocol invariant; trust
				// the container's flag otherwise so CMAF mid-group keyframes survive.
				frame.keyframe = frame.keyframe || self.index == 0;
				self.index += 1;
				return Poll::Ready(Ok(Some(frame)));
			}

			// An empty batch is not end-of-group, so keep looping until the format says None.
			let Some(frames) = ready!(self.format.poll_read(&mut self.group, waiter))? else {
				return Poll::Ready(Ok(None));
			};
			self.pending.extend(frames);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::catalog::hang::Container as Hang;

	fn frame(timestamp_us: u64, payload: &'static [u8], keyframe: bool) -> Frame {
		Frame {
			timestamp: moq_net::Timestamp::from_micros(timestamp_us).unwrap(),
			payload: bytes::Bytes::from_static(payload),
			keyframe,
			duration: None,
		}
	}

	/// Read one retained group end to end, without a subscription.
	#[tokio::test]
	async fn reads_a_group_to_completion() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast.create_track("media", None).unwrap();
		let consumer = broadcast.consume();

		let mut media = crate::container::Producer::new(track, Hang::Legacy);
		media.write(frame(1_000_000, b"keyframe", true)).unwrap();
		media.write(frame(1_020_000, b"delta", false)).unwrap();
		media.finish().unwrap();

		let group = consumer.track("media").unwrap().fetch_group(0, None).await.unwrap();
		let mut group = GroupConsumer::new(group, Hang::Legacy);
		assert_eq!(group.sequence(), 0);

		let first = group.read().await.unwrap().unwrap();
		assert_eq!(first.payload, b"keyframe".as_slice());
		assert!(first.keyframe);

		let second = group.read().await.unwrap().unwrap();
		assert_eq!(second.payload, b"delta".as_slice());
		assert!(!second.keyframe);

		assert!(group.read().await.unwrap().is_none());
	}

	/// One CMAF fragment decodes to several samples, which are handed back one at a time.
	#[tokio::test]
	async fn hands_back_a_cmaf_batch_one_frame_at_a_time() {
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.coded_width = Some(320);
		config.coded_height = Some(240);
		let muxer = crate::container::fmp4::Muxer::video(&config).unwrap();
		let init = muxer.init().unwrap().expect("VP8 init should be available");
		let cmaf = hang::catalog::Container::Cmaf { init };
		// The format is not Clone, so decode with a second instance built from the same init.
		let format = Hang::try_from(&cmaf).unwrap();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast.create_track("video", None).unwrap();
		let consumer = broadcast.consume();

		// Buffer both samples into one moof+mdat, which is what this decodes.
		let mut media = crate::container::Producer::new(track, format).with_buffer(std::time::Duration::from_secs(1));
		for (timestamp_us, payload, keyframe) in [
			(2_000_000, b"keyframe".as_slice(), true),
			(2_020_000, b"delta".as_slice(), false),
		] {
			media
				.write(Frame {
					timestamp: moq_net::Timestamp::from_micros(timestamp_us).unwrap(),
					payload: bytes::Bytes::from_static(payload),
					keyframe,
					duration: Some(moq_net::Timestamp::from_micros(20_000).unwrap()),
				})
				.unwrap();
		}
		media.finish().unwrap();

		let group = consumer.track("video").unwrap().fetch_group(0, None).await.unwrap();
		let mut group = GroupConsumer::new(group, Hang::try_from(&cmaf).unwrap());

		let first = group.read().await.unwrap().unwrap();
		assert_eq!(first.payload, b"keyframe".as_slice());
		assert!(first.keyframe);

		let second = group.read().await.unwrap().unwrap();
		assert_eq!(second.payload, b"delta".as_slice());
		assert!(!second.keyframe);

		assert!(group.read().await.unwrap().is_none());
	}
}
