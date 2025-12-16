use std::sync::Arc;

use crate::{Error, Id, NonZeroSlab, RuntimeLock};

#[derive(Default)]
pub struct Publish {
	/// Active broadcast producers for publishing.
	broadcasts: NonZeroSlab<hang::BroadcastProducer>,

	/// Active media encoders/decoders for publishing.
	media: NonZeroSlab<hang::import::Decoder>,
}

impl Publish {
	pub fn create(&mut self) -> Result<Id, Error> {
		let broadcast = hang::BroadcastProducer::default();
		let id = self.broadcasts.insert(broadcast);
		Ok(id)
	}

	pub fn get(&self, id: Id) -> Result<&hang::BroadcastProducer, Error> {
		self.broadcasts.get(id).ok_or(Error::NotFound)
	}

	pub fn close(&mut self, broadcast: Id) -> Result<(), Error> {
		self.broadcasts.remove(broadcast).ok_or(Error::NotFound)?;
		Ok(())
	}

	pub fn media_init(&mut self, broadcast: Id, format: &str, init: &[u8]) -> Result<Id, Error> {
		let broadcast = self.broadcasts.get(broadcast).ok_or(Error::NotFound)?;
		let mut decoder = hang::import::Decoder::new(broadcast.clone(), format)
			.ok_or_else(|| Error::UnknownFormat(format.to_string()))?;

		let mut temp = init;
		decoder
			.initialize(&mut temp)
			.map_err(|err| Error::InitFailed(Arc::new(err)))?;
		assert!(init.is_empty(), "buffer was not fully consumed");

		let id = self.media.insert(decoder);
		Ok(id)
	}

	pub fn media_frame(&mut self, media: Id, mut data: &[u8], timestamp: hang::Timestamp) -> Result<(), Error> {
		let media = self.media.get_mut(media).ok_or(Error::NotFound)?;

		media
			.decode_frame(&mut data, Some(timestamp))
			.map_err(|err| Error::DecodeFailed(Arc::new(err)))?;
		assert!(data.is_empty(), "buffer was not fully consumed");

		Ok(())
	}

	pub fn media_close(&mut self, media: Id) -> Result<(), Error> {
		self.media.remove(media).ok_or(Error::NotFound)?;
		Ok(())
	}
}

/// Global shared state instance.
pub static PUBLISH: RuntimeLock<Publish> = RuntimeLock::new();
