//! Integration test: an unannounced broadcast still releases a track's demand
//! once its last subscriber leaves.

use std::time::Duration;

use moq_tokio::moq_net;

/// Subscribe to a published track, optionally unannounce (and let the origin
/// process the retraction), then drop the subscriber and wait for the
/// publisher's demand to go unused.
async fn release(unannounce: bool, settle: bool) {
	let origin = moq_tokio::origin::spawn();
	let broadcast = origin.publish("a/b", moq_net::origin::Route::default()).unwrap();
	let track = broadcast.create_track("t", None).unwrap();
	let consumer = origin.consume().request_broadcast("a/b").await.unwrap();
	let sub = consumer.track("t").unwrap().subscribe(None).await.unwrap();
	track.demand().used().await.unwrap();

	if unannounce {
		broadcast.unannounce();
		if settle {
			tokio::time::sleep(Duration::from_secs(1)).await;
		}
	}

	drop(sub);
	drop(consumer);
	tokio::time::timeout(Duration::from_secs(600), track.demand().unused())
		.await
		.expect("demand released")
		.unwrap();
}

#[tokio::test(start_paused = true)]
async fn announced_track_releases_demand() {
	release(false, false).await
}

#[tokio::test(start_paused = true)]
async fn unannounced_track_releases_demand() {
	release(true, false).await
}

#[tokio::test(start_paused = true)]
async fn settled_unannounce_releases_demand() {
	release(true, true).await
}
