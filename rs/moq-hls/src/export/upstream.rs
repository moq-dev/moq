//! The broadcast an export is bound to, held rather than looked up again by path.

/// The one broadcast an export is bound to: the already-resolved catalog consumer, plus the
/// [`moq_mux::Source`] it came from so a rendition can still follow a cross-broadcast reference.
///
/// The two travel together because resolving the catalog path by name is not idempotent. A
/// same-path republish is a takeover (`Hop::UNKNOWN` never counts as the same publisher, so
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
	/// Bind a rendition whose catalog `broadcast` field is `rel` to the broadcast serving it.
	///
	/// An absent or empty reference means the catalog broadcast, which is already in hand; a
	/// named sibling is requested here, when the rendition is created. Both are therefore fixed
	/// before the rendition can list a segment, so a republish afterwards never answers for
	/// media the timeline has already described.
	///
	/// Fails when the reference escapes above the origin root and so names no broadcast at all.
	pub fn bind(&self, rel: Option<&moq_net::PathRelativeOwned>) -> moq_mux::Result<moq_mux::Binding> {
		match rel.filter(|rel| !rel.is_empty()) {
			None => Ok(moq_mux::Binding::new(self.broadcast.clone())),
			Some(rel) => self.source.bind(Some(rel)),
		}
	}
}
