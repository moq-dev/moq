use std::task::Poll;

use crate::container::{Container, Frame, Timestamp};

/// LOC (Low Overhead Container) frame format.
///
/// Each moq-lite frame holds one LOC frame: a small property block (timestamp
/// and optional per-frame timescale) followed by the codec bitstream. See
/// [draft-ietf-moq-loc](https://www.ietf.org/archive/id/draft-ietf-moq-loc-00.html).
///
/// `catalog_timescale` is the units/sec used when a frame omits its own 0x08
/// timescale property. The catalog default is microseconds (`1_000_000`).
pub struct Loc {
	catalog_timescale: u64,
}

impl Loc {
	pub fn new(catalog_timescale: u64) -> Self {
		Self { catalog_timescale }
	}
}

impl Container for Loc {
	type Error = crate::Error;

	fn write(&self, group: &mut moq_lite::GroupProducer, frames: &[Frame]) -> Result<(), Self::Error> {
		for frame in frames {
			// Rescale the microsecond timestamp into the catalog's timescale.
			let scaled = (frame.timestamp.as_micros() * self.catalog_timescale as u128 / 1_000_000) as u64;

			let data = moq_loc::encode(scaled, &frame.payload)?;

			let mut chunked = group.create_frame(data.len().into())?;
			chunked.write(data)?;
			chunked.finish()?;
		}
		Ok(())
	}

	fn poll_read(
		&self,
		group: &mut moq_lite::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Vec<Frame>>, Self::Error>> {
		use std::task::ready;

		let Some(data) = ready!(group.poll_read_frame(waiter)?) else {
			return Poll::Ready(Ok(None));
		};

		let loc = moq_loc::decode(data)?;
		let timescale = loc.timescale.unwrap_or(self.catalog_timescale);
		let timestamp = Timestamp::from_scale(loc.timestamp, timescale).map_err(hang::Error::from)?;

		Poll::Ready(Ok(Some(vec![Frame {
			timestamp,
			payload: loc.payload,
			// LOC keyframes are inferred from group position by the wrapping Consumer.
			keyframe: false,
		}])))
	}
}
