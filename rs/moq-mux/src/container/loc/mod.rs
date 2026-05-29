//! Low Overhead Container.
//!
//! The IETF draft replacement for hang's Legacy format. Each moq frame
//! holds a small property block (timestamp, optional per-frame
//! timescale) followed by the codec bitstream. Defaults to microsecond
//! timestamps. See [draft-ietf-moq-loc](https://www.ietf.org/archive/id/draft-ietf-moq-loc-00.html).

use std::task::Poll;

use crate::container::{Container, Frame, Timestamp};

/// LOC wire format. Each moq frame holds one LOC frame.
#[derive(Default)]
pub struct Wire;

const DEFAULT_TIMESCALE: u64 = 1_000_000;

impl Container for Wire {
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
		waiter: &kio::Waiter,
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
			// LOC doesn't carry the keyframe bit on the wire; the
			// wrapping Consumer fills it in from group position.
			keyframe: false,
		}])))
	}
}
