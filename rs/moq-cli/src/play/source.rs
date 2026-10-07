//! Watching the broadcast to play.

use hang::moq_net;

/// The announcements of the routes on `origin` that overlap `broadcast`.
///
/// Only those covering the path matter to the player; the scope just keeps the
/// rest of the origin from waking it. A path at the depth limit cannot be spelled
/// as a subtree, so it watches the whole origin instead.
pub(super) fn announced(
	origin: &moq_net::origin::Consumer,
	broadcast: &str,
) -> anyhow::Result<moq_net::announce::Consumer> {
	let origin = match moq_net::Pattern::subtree(broadcast) {
		Ok(subtree) => origin.scope("", &moq_net::Patterns::from(subtree))?,
		Err(moq_net::InvalidPattern::TooManySegments) => origin.clone(),
		Err(err) => return Err(err.into()),
	};
	Ok(origin.announced())
}
