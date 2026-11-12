//! Stream instances, mirroring [`moq_net::Instance`].

use std::sync::Arc;

use crate::error::MoqError;

/// Names one continuous stream at a path, as routes and requests carry it.
///
/// A session mints one for a route whose wire (moq-lite 06 or older, or moq-transport) carries
/// none; only a session can, so [`MoqInstance::parse`] always names one. Two instances are equal
/// only when their text and whether a session minted them both match, so pass an instance a
/// route reported back as is, rather than its text, to pin that stream.
#[derive(Debug, Clone, PartialEq, Eq, Hash, uniffi::Object)]
#[uniffi::export(Debug, Display, Eq, Hash)]
pub struct MoqInstance(pub(crate) moq_net::Instance);

impl std::fmt::Display for MoqInstance {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		std::fmt::Display::fmt(&self.0, f)
	}
}

#[uniffi::export]
impl MoqInstance {
	/// A named instance with `text`, refusing an empty one.
	#[uniffi::constructor]
	pub fn parse(text: String) -> Result<Arc<Self>, MoqError> {
		Ok(Arc::new(Self(text.try_into()?)))
	}

	/// The instance's text, which alone does not tell a minted instance from a named one.
	pub fn text(&self) -> String {
		self.0.as_str().to_string()
	}

	/// Whether a session minted this instance for a route whose wire carries none. It names the
	/// announcement rather than the publisher's stream, so signing refuses it and a catalog's
	/// instance check skips it.
	pub fn is_minted(&self) -> bool {
		self.0.is_minted()
	}
}

impl MoqInstance {
	pub(crate) fn wrap(instance: moq_net::Instance) -> Arc<Self> {
		Arc::new(Self(instance))
	}
}
