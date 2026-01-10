use buf_list::BufList;
use bytes::BytesMut;
use moq_lite::coding::{Decode, Encode};

use crate::Error;

/// A timestamp representing the presentation time in microseconds.
///
/// [moq_lite::Time] is in milliseconds, but that's not quiiite precise enough for audio.
/// (technically you could make it work, but it's a pain in the butt)
/// We encode microseconds instead to match the WebCodecs API.
pub type Timestamp = moq_lite::Timescale<1_000_000>;

pub struct Container {
	pub timestamp: Timestamp,
	pub payload: BufList,
}

impl Container {
	pub async fn decode(group: &mut moq_lite::GroupConsumer) -> Result<Option<Self>, Error> {
		let Some(mut frame) = group.next_frame().await? else {
			return Ok(None);
		};

		// NOTE: We currently don't use `frame.timestamp` for backwards compatibility with versions < draft03.
		// TODO: Remove legacy support and stop double encoding the timestamp.

		// TODO: Ideally we don't buffer the entire payload in memory; some decoders can handle it.
		let payload = frame.read_chunks().await?;

		let mut payload = BufList::from_iter(payload);
		let timestamp = Timestamp::decode(&mut payload, moq_lite::lite::Version::Draft03)?;
		let container = Self { timestamp, payload };

		Ok(Some(container))
	}

	pub fn encode(&self, group: &mut moq_lite::GroupProducer) -> Result<(), Error> {
		let mut header = BytesMut::new();
		self.timestamp.encode(&mut header, moq_lite::lite::Version::Draft03);

		let size = self.payload.num_bytes() + header.len();
		let frame = moq_lite::Frame {
			size,
			// NOTE: We encode the timestamp into the MoQ layer as well.
			// The MoQ layer uses milliseconds, so we convert from our microsecond timestamp.
			instant: self.timestamp.convert().expect("timestamp conversion overflow"),
		};

		let mut chunked = group.create_frame(frame)?;
		chunked.write_chunk(header.freeze())?;
		for chunk in &self.payload {
			chunked.write_chunk(chunk.clone())?;
		}
		chunked.close()?;

		Ok(())
	}
}
