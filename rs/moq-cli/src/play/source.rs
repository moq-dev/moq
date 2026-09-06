//! Resolving the broadcast to play.

use anyhow::Context;
use hang::moq_net;

/// Wait for `broadcast` to be announced on `origin`, then subscribe to it.
///
/// The wait is the whole point. Subscribing goes through
/// `origin::Consumer::request_broadcast`, which resolves `Unroutable` on the
/// spot when no session has registered a handler yet rather than waiting for
/// one, and the media task starts well before the first handshake lands. The
/// window is already up, so this shows as a black frame rather than as a hang.
pub(super) async fn subscribe(origin: moq_net::origin::Consumer, broadcast: &str) -> anyhow::Result<moq_mux::Source> {
	origin
		.announced_broadcast(broadcast)
		.await
		.with_context(|| format!("origin closed before broadcast `{broadcast}` was announced"))?;

	Ok(moq_mux::Source::new(origin, broadcast))
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::time::Duration;

	/// Subscribing before the announcement lands doesn't wait, it fails: with no
	/// session yet there is no handler registered on the origin, and
	/// `request_broadcast` resolves `Unroutable` immediately. The media task is
	/// spawned right after the reconnect loop starts, so it gets there first.
	#[tokio::test]
	async fn subscribe_waits_for_the_announcement() {
		tokio::time::pause();

		let origin = moq_net::Origin::random().produce();
		let consumer = origin.consume();

		// Direct resolution has no route before the announcement.
		let unannounced = moq_mux::Source::new(consumer.clone(), "room.hang").broadcast().await;
		assert!(unannounced.is_err(), "expected an unroutable broadcast");

		// Waiting first parks instead, for as long as it takes.
		let mut waiting = std::pin::pin!(subscribe(consumer, "room.hang"));
		let parked = tokio::time::timeout(Duration::from_secs(60), &mut waiting).await;
		assert!(parked.is_err(), "expected to still be waiting on the announcement");

		let _broadcast = origin
			.create_broadcast("room.hang", moq_net::broadcast::Route::new().with_announce(true))
			.unwrap();
		waiting.await.unwrap();
	}
}
