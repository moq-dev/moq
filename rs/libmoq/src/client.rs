use crate::{Error, Id, NonZeroSlab};

/// Client configurations built up by the `moq_client_*` setters.
///
/// A handle is a plain [`moq_native::ClientConfig`] the caller mutates one knob at a
/// time, then dials with. Dialing clones it, so one handle can open any number of
/// sessions and stays editable in between.
#[derive(Default)]
pub struct Client {
	configs: NonZeroSlab<moq_native::ClientConfig>,
}

impl Client {
	pub fn create(&mut self) -> Result<Id, Error> {
		self.configs.insert(moq_native::ClientConfig::default())
	}

	pub fn close(&mut self, id: Id) -> Result<(), Error> {
		self.configs.remove(id).ok_or(Error::ClientNotFound)?;
		Ok(())
	}

	/// The config a setter should mutate.
	pub fn get_mut(&mut self, id: Id) -> Result<&mut moq_native::ClientConfig, Error> {
		self.configs.get_mut(id).ok_or(Error::ClientNotFound)
	}

	/// The config to dial with. A missing handle means "no config given", which is the
	/// defaults, so [`crate::moq_session_connect`] is just this with `None`.
	pub fn config(&self, id: Option<Id>) -> Result<moq_native::ClientConfig, Error> {
		match id {
			Some(id) => self.configs.get(id).cloned().ok_or(Error::ClientNotFound),
			None => Ok(moq_native::ClientConfig::default()),
		}
	}
}
