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
	/// Every reference resolving to the catalog path reuses the broadcast already in hand.
	/// A sibling request starts when the rendition is created, before it can list a segment,
	/// and subsequent media fetches reuse that request's result.
	///
	/// Fails when the reference escapes above the origin root and so names no broadcast at all.
	pub fn bind(&self, rel: Option<&moq_net::PathRelativeOwned>) -> moq_mux::Result<moq_mux::Binding> {
		if self.source.resolve_reference(rel) == self.source.resolve_reference(None) {
			Ok(moq_mux::Binding::new(self.broadcast.clone()))
		} else {
			self.source.bind(rel)
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn self_references_keep_the_catalog_broadcast_after_replacement() {
		let (origin, driver) = moq_net::origin::Producer::new(moq_net::Hop::random().into());
		let driver = tokio::spawn(driver.run(moq_tokio::runtime::Runtime::<()>::new()));
		let old = origin.create_broadcast("a/live").unwrap();
		let source = moq_mux::Source::new(origin.consume(), "a/live");
		let upstream = Upstream {
			source,
			broadcast: old.consume(),
		};
		drop(old);
		let _replacement = origin.create_broadcast("a/live").unwrap();
		for _ in 0..10 {
			tokio::task::yield_now().await;
		}
		assert!(!upstream.source.broadcast().await.unwrap().is_closed());

		for reference in [None, Some(""), Some("live"), Some("./live"), Some("../a/live")] {
			let rel = reference.map(|value| moq_net::PathRelative::new(value).to_owned());
			let bound = upstream.bind(rel.as_ref()).unwrap().broadcast().await.unwrap();
			assert!(
				bound.is_closed(),
				"{reference:?} must keep the original catalog broadcast"
			);
		}
		driver.abort();
	}
}
