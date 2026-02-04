use std::{str::FromStr, sync::Arc};

use moq_lite::coding::Buf;
use moq_mux::import;

use crate::{Error, Id, NonZeroSlab};

#[derive(Default)]
pub struct Publish {
	/// Active broadcast producers for publishing.
	broadcasts: NonZeroSlab<(moq_lite::BroadcastProducer, hang::CatalogProducer)>,

	/// Active media encoders/decoders for publishing.
	media: NonZeroSlab<import::Decoder>,
}

impl Publish {
	pub fn create(&mut self) -> Result<Id, Error> {
		let mut broadcast = moq_lite::BroadcastProducer::new();
		let catalog = broadcast.create_track(hang::Catalog::default_track());
		let catalog = hang::CatalogProducer::new(catalog, hang::Catalog::default());

		let id = self.broadcasts.insert((broadcast, catalog));
		Ok(id)
	}

	pub fn get(&self, id: Id) -> Result<&moq_lite::BroadcastProducer, Error> {
		self.broadcasts
			.get(id)
			.ok_or(Error::NotFound)
			.map(|(broadcast, _)| broadcast)
	}

	pub fn close(&mut self, broadcast: Id) -> Result<(), Error> {
		self.broadcasts.remove(broadcast).ok_or(Error::NotFound)?;
		Ok(())
	}

	pub fn media_ordered(&mut self, broadcast: Id, format: &str, mut init: &[u8]) -> Result<Id, Error> {
		let (broadcast, catalog) = self.broadcasts.get(broadcast).ok_or(Error::NotFound)?;

		let format = import::DecoderFormat::from_str(format).map_err(|_| Error::UnknownFormat(format.to_string()))?;
		let mut decoder = import::Decoder::new(broadcast.clone(), catalog.clone(), format);

		decoder
			.initialize(&mut init)
			.map_err(|err| Error::InitFailed(Arc::new(err)))?;
		if init.has_remaining() {
			return Err(Error::InitFailed(Arc::new(anyhow::anyhow!(
				"buffer was not fully consumed"
			))));
		}

		let id = self.media.insert(decoder);
		Ok(id)
	}

	pub fn media_frame(
		&mut self,
		media: Id,
		mut data: &[u8],
		timestamp: hang::container::Timestamp,
	) -> Result<(), Error> {
		let media = self.media.get_mut(media).ok_or(Error::NotFound)?;

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
		self.media.remove(media).ok_or(Error::NotFound)?;
		Ok(())
	}
}
