//! Stream identities, mirroring [`moq_net::Identity`].

use std::sync::Arc;

use crate::error::MoqError;

/// Names one continuous stream at a path, as routes and requests carry it.
///
/// A session mints one for a route whose wire (moq-lite 06 or older, or moq-transport) carries
/// none; only a session can, so [`MoqIdentity::parse`] always names one. Two identities are equal
/// only when their text and whether a session minted them both match, so pass an identity a
/// route reported back as is, rather than its text, to pin that stream.
#[derive(Debug, Clone, PartialEq, Eq, Hash, uniffi::Object)]
#[uniffi::export(Debug, Display, Eq, Hash)]
pub struct MoqIdentity(pub(crate) moq_net::Identity);

impl std::fmt::Display for MoqIdentity {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		std::fmt::Display::fmt(&self.0, f)
	}
}

#[uniffi::export]
impl MoqIdentity {
	/// A named identity with `text`, refusing an empty one or one longer than 256 UTF-8 bytes.
	#[uniffi::constructor]
	pub fn parse(text: String) -> Result<Arc<Self>, MoqError> {
		Ok(Arc::new(Self(text.try_into()?)))
	}

	/// The identity's text, which alone does not tell a minted identity from a named one.
	pub fn text(&self) -> String {
		self.0.as_str().to_string()
	}

	/// Whether a session minted this identity for a route whose wire carries none. It names the
	/// announcement rather than the publisher's stream, so signing refuses it.
	pub fn is_minted(&self) -> bool {
		self.0.is_minted()
	}
}

impl MoqIdentity {
	pub(crate) fn wrap(identity: moq_net::Identity) -> Arc<Self> {
		Arc::new(Self(identity))
	}
}
