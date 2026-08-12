//! The broadcast an export is bound to, held rather than looked up again by path.

/// The one broadcast an export is bound to: the already-resolved catalog consumer, plus the
/// [`moq_mux::Source`] it came from so a rendition can still follow a cross-broadcast reference.
///
/// The two travel together because resolving the catalog path by name is not idempotent. A
/// same-path republish is a takeover (`Origin::UNKNOWN` never counts as the same publisher, so
/// every ordinary publisher reconnect qualifies), and that installs a brand new broadcast at the
/// leaf. A rendition that looked its media up by path would then serve the replacement's groups
/// under the manifest, segment numbering, and `PROGRAM-DATE-TIME` of the broadcast it replaced.
#[derive(Clone)]
pub(crate) struct Upstream {
	/// Origin plus catalog path, for resolving a rendition's sibling `broadcast` reference.
	pub source: moq_mux::Source,
	/// The catalog broadcast, resolved once when the export started.
	pub broadcast: moq_net::broadcast::Consumer,
}

impl Upstream {
	/// The broadcast serving a rendition whose catalog `broadcast` field is `rel`, when that can
	/// be answered without a round trip.
	///
	/// An absent or empty reference means the catalog broadcast itself, which is already in hand.
	/// A named sibling returns `None` and has to go through [`moq_mux::Source::resolve`].
	pub fn local(&self, rel: Option<&moq_net::PathRelativeOwned>) -> Option<moq_net::broadcast::Consumer> {
		rel.filter(|rel| !rel.is_empty())
			.is_none()
			.then(|| self.broadcast.clone())
	}
}
