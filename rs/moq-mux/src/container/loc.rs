use std::task::Poll;

use crate::container::{Container, Frame, Timestamp};

/// LOC (Low Overhead Container) frame format.
///
/// Each moq-net frame holds one LOC frame: a small property block (timestamp
/// and optional per-frame timescale) followed by the codec bitstream. See
/// [draft-ietf-moq-loc](https://www.ietf.org/archive/id/draft-ietf-moq-loc-00.html).
///
/// Frames without a 0x08 timescale property are interpreted as microseconds.
#[derive(Default)]
pub struct Loc;

impl Loc {
	pub fn new() -> Self {
		Self
	}
}

const DEFAULT_TIMESCALE: u64 = 1_000_000;

impl Container for Loc {
	type Error = crate::Error;

	fn write(&self, group: &mut moq_net::GroupProducer, frames: &[Frame]) -> Result<(), Self::Error> {
		for frame in frames {
			let data = moq_loc::encode(frame.timestamp.as_micros() as u64, &frame.payload)?;

			let mut chunked = group.create_frame(data.len().into())?;
			chunked.write(data)?;
			chunked.finish()?;
		}
		Ok(())
	}

	fn poll_read(
		&self,
		group: &mut moq_net::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Vec<Frame>>, Self::Error>> {
		use std::task::ready;

		let Some(data) = ready!(group.poll_read_frame(waiter)?) else {
			return Poll::Ready(Ok(None));
		};

		let loc = moq_loc::decode(data)?;
		let timescale = loc.timescale.unwrap_or(DEFAULT_TIMESCALE);
		let timestamp = Timestamp::from_scale(loc.timestamp, timescale).map_err(hang::Error::from)?;

		Poll::Ready(Ok(Some(vec![Frame {
			timestamp,
			payload: loc.payload,
			// LOC keyframes are inferred from group position by the wrapping Consumer.
			keyframe: false,
		}])))
	}
}
