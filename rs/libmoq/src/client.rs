use crate::{Error, Id, NonZeroSlab};

/// One client handle: the dial config plus the QUIC tuning it dials with.
///
/// They are separate types in `moq-native` (the tuning is shared by the dial and
/// accept sides of an endpoint), but a C caller configures one client, so the
/// handle carries both and `moq_client_set_*` writes into whichever owns the knob.
#[derive(Clone, Default)]
pub struct Config {
	pub connect: moq_native::connect::Config,
	pub quic: moq_native::quic::Config,
}

/// Client configurations built up by the `moq_client_*` setters.
///
/// A handle is a plain [`Config`] the caller mutates one knob at a time, then
/// dials with. Dialing clones it, so one handle can open any number of sessions
/// and stays editable in between.
#[derive(Default)]
pub struct Client {
	configs: NonZeroSlab<Config>,
}

impl Client {
	pub fn create(&mut self) -> Result<Id, Error> {
		self.configs.insert(Config::default())
	}

	pub fn close(&mut self, id: Id) -> Result<(), Error> {
		self.configs.remove(id).ok_or(Error::ClientNotFound)?;
		Ok(())
	}

	/// The config a setter should mutate.
	pub fn get_mut(&mut self, id: Id) -> Result<&mut Config, Error> {
		self.configs.get_mut(id).ok_or(Error::ClientNotFound)
	}

	/// The config to dial with. A missing handle means "no config given", which is the
	/// defaults, so [`crate::moq_session_connect`] is just this with `None`.
	pub fn config(&self, id: Option<Id>) -> Result<Config, Error> {
		match id {
			Some(id) => self.configs.get(id).cloned().ok_or(Error::ClientNotFound),
			None => Ok(Config::default()),
		}
	}
}
