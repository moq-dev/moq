//! Low Overhead Container.
//!
//! The IETF draft replacement for hang's Legacy format. Each moq frame
//! holds a small property block (timestamp, optional per-frame
//! timescale) followed by the codec bitstream. Defaults to microsecond
//! timestamps. See [draft-ietf-moq-loc](https://www.ietf.org/archive/id/draft-ietf-moq-loc-00.html).

use std::task::Poll;

use moq_net::{Timescale, Timestamp};

use crate::container::{Container, Frame};

/// LOC's catalog convention: timestamps are in microseconds when no per-frame
/// 0x08 timescale property is present.
const DEFAULT_TIMESCALE: Timescale = Timescale::MICRO;

/// LOC wire format. Each moq frame holds one LOC frame.
#[derive(Default)]
pub struct Wire;

impl Container for Wire {
	type Error = crate::Error;

	fn write(&self, group: &mut moq_net::GroupProducer, frames: &[Frame]) -> Result<(), Self::Error> {
		for frame in frames {
			// LOC's wire format omits per-frame timescale by convention; the catalog
			// default is microseconds, so convert at the boundary.
			let timestamp = frame.timestamp.convert(DEFAULT_TIMESCALE).map_err(hang::Error::from)?;
			let data = moq_loc::encode(timestamp.value(), &frame.payload)?;

			let mut chunked = group.create_frame(data.len())?;
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
		// `loc.timescale == Some(0)` is a malformed wire (caught by moq_loc::decode itself),
		// so any Some(_) we see here is non-zero. Falling back to the catalog default
		// keeps this code path infallible.
		let scale = loc
			.timescale
			.and_then(|s| Timescale::new(s).ok())
			.unwrap_or(DEFAULT_TIMESCALE);
		let timestamp = Timestamp::new(loc.timestamp, scale).map_err(hang::Error::from)?;

		Poll::Ready(Ok(Some(vec![Frame {
			timestamp,
			payload: loc.payload,
			// LOC doesn't carry the keyframe bit on the wire; the
			// wrapping Consumer fills it in from group position.
			keyframe: false,
			// LOC carries no per-frame duration.
			duration: None,
		}])))
	}
}
