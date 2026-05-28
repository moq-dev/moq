use std::str::FromStr;

use bytes::Buf;
use moq_mux::import;

use crate::{Error, Id, NonZeroSlab};

#[derive(Default)]
pub struct Publish {
	/// Active broadcast producers for publishing.
	broadcasts: NonZeroSlab<(moq_net::BroadcastProducer, moq_mux::catalog::hang::Producer)>,

	/// Active media encoders/decoders for publishing.
	media: NonZeroSlab<import::Framed>,
}

impl Publish {
	pub fn create(&mut self) -> Result<Id, Error> {
		let mut broadcast = moq_net::Broadcast::new().produce();
		let catalog = moq_mux::catalog::hang::Producer::new(&mut broadcast)?;

		let id = self.broadcasts.insert((broadcast, catalog))?;
		Ok(id)
	}

	pub fn get(&self, id: Id) -> Result<&moq_net::BroadcastProducer, Error> {
		self.broadcasts
			.get(id)
			.ok_or(Error::BroadcastNotFound)
			.map(|(broadcast, _)| broadcast)
	}

	/// Mutable access to both the broadcast and its catalog producer.
	/// Used by sibling modules (e.g. `audio`) that need to attach a new
	/// track to an existing publish.
	pub fn pair_mut(
		&mut self,
		id: Id,
	) -> Result<(&mut moq_net::BroadcastProducer, &mut moq_mux::catalog::hang::Producer), Error> {
		let (broadcast, catalog) = self.broadcasts.get_mut(id).ok_or(Error::BroadcastNotFound)?;
		Ok((broadcast, catalog))
	}

	pub fn close(&mut self, broadcast: Id) -> Result<(), Error> {
		self.broadcasts.remove(broadcast).ok_or(Error::BroadcastNotFound)?;
		Ok(())
	}

	pub fn media_ordered(&mut self, broadcast: Id, format: &str, mut init: &[u8]) -> Result<Id, Error> {
		let (broadcast, catalog) = self.broadcasts.get(broadcast).ok_or(Error::BroadcastNotFound)?;

		let format = import::FramedFormat::from_str(format).map_err(|_| Error::UnknownFormat(format.to_string()))?;
		let decoder = import::Framed::new(broadcast.clone(), catalog.clone(), format, &mut init)?;

		let id = self.media.insert(decoder)?;
		Ok(id)
	}

	pub fn media_frame(
		&mut self,
		media: Id,
		mut data: &[u8],
		timestamp: hang::container::Timestamp,
	) -> Result<(), Error> {
		let media = self.media.get_mut(media).ok_or(Error::MediaNotFound)?;

		media.decode_frame(&mut data, Some(timestamp))?;

		if data.has_remaining() {
			return Err(Error::BufferNotConsumed);
		}

		Ok(())
	}

	pub fn media_close(&mut self, media: Id) -> Result<(), Error> {
		let mut decoder = self.media.remove(media).ok_or(Error::MediaNotFound)?;
		decoder.finish()?;
		Ok(())
	}
}
