use std::{str::FromStr, sync::Arc};

use bytes::Buf;
use moq_mux::producer;

use crate::{Error, Id, NonZeroSlab};

#[derive(Default)]
pub struct Publish {
	/// Active broadcast producers for publishing.
	broadcasts: NonZeroSlab<(moq_lite::BroadcastProducer, moq_mux::CatalogProducer)>,

	/// Active media encoders/decoders for publishing.
	media: NonZeroSlab<producer::Framed>,
}

impl Publish {
	pub fn create(&mut self) -> Result<Id, Error> {
		let mut broadcast = moq_lite::Broadcast::new().produce();
		let catalog = moq_mux::CatalogProducer::new(&mut broadcast)?;

		let id = self.broadcasts.insert((broadcast, catalog))?;
		Ok(id)
	}

	pub fn get(&self, id: Id) -> Result<&moq_lite::BroadcastProducer, Error> {
		self.broadcasts
			.get(id)
			.ok_or(Error::BroadcastNotFound)
			.map(|(broadcast, _)| broadcast)
	}

	pub fn close(&mut self, broadcast: Id) -> Result<(), Error> {
		self.broadcasts.remove(broadcast).ok_or(Error::BroadcastNotFound)?;
		Ok(())
	}

	pub fn media_ordered(&mut self, broadcast: Id, format: &str, mut init: &[u8]) -> Result<Id, Error> {
		let (broadcast, catalog) = self.broadcasts.get(broadcast).ok_or(Error::BroadcastNotFound)?;

		let format = producer::FramedFormat::from_str(format).map_err(|_| Error::UnknownFormat(format.to_string()))?;
		let decoder = producer::Framed::new(broadcast.clone(), catalog.clone(), format, &mut init)
			.map_err(|err| Error::InitFailed(Arc::new(err)))?;

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

		media
			.decode_frame(&mut data, Some(timestamp))
			.map_err(|err| Error::DecodeFailed(Arc::new(err)))?;

		if data.has_remaining() {
			return Err(Error::DecodeFailed(Arc::new(anyhow::anyhow!(
				"buffer was not fully consumed"
			))));
		}

		Ok(())
	}

	pub fn media_close(&mut self, media: Id) -> Result<(), Error> {
		let mut decoder = self.media.remove(media).ok_or(Error::MediaNotFound)?;
		decoder.finish().map_err(|err| Error::DecodeFailed(Arc::new(err)))?;
		Ok(())
	}
}
