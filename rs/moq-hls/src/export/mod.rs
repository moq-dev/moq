//! Export: subscribe to a MoQ broadcast and turn it into HLS / LL-HLS.
//!
//! A [`Broadcaster`] watches one broadcast's catalog and, per rendition, runs a
//! [`moq_mux::container::fmp4::Export`] narrowed to that single track (via
//! [`moq_mux::catalog::Select`]) feeding a [`store::SegmentStore`]. The HTTP
//! [`server`](crate::server) reads the stores to answer playlist and segment
//! requests.

mod master;
mod playlist;
mod rendition;
pub mod store;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use moq_mux::catalog::hang::Catalog;
use moq_mux::catalog::{self, CatalogFormat, Stream};
use tokio::sync::watch;

pub use playlist::render_media;
pub use rendition::{Kind, Rendition};

/// How long to wait before retrying the initial catalog subscription.
const CATALOG_RETRY: Duration = Duration::from_millis(250);

/// Aborts a spawned task when dropped, tying the task's lifetime to the value that
/// owns this guard. Used to stop the catalog watcher + rendition pumps the moment a
/// [`Broadcaster`] is dropped, so its source subscriptions are released instead of
/// lingering until the broadcast itself closes (which a subscription-driven
/// publisher would never do while we hold it open -- a self-sustaining leak).
pub(super) struct AbortOnDrop(pub(super) tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
	fn drop(&mut self) {
		self.0.abort();
	}
}

/// Export tuning shared across renditions.
#[derive(Clone, Debug)]
pub struct Config {
	/// LL-HLS part target duration (also the exporter's fragment cap).
	pub part_target: Duration,
	/// Minimum duration of media retained in each rendition's sliding window.
	/// Older segments are evicted once the remaining ones still cover this span.
	pub window: Duration,
	/// Exporter latency budget. Generous so live GOPs aren't skipped; see the
	/// group-skip note in the crate plan.
	pub latency: Duration,
	/// Target segment duration for audio renditions (video rolls on GOPs).
	pub audio_segment_target: Duration,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			part_target: Duration::from_millis(500),
			window: Duration::from_secs(16),
			latency: Duration::from_secs(10),
			audio_segment_target: Duration::from_secs(2),
		}
	}
}

/// All renditions of one broadcast, kept in sync with its catalog.
pub struct Broadcaster {
	broadcast: moq_net::BroadcastConsumer,
	renditions: Mutex<BTreeMap<String, Arc<Rendition>>>,
	/// Current rendition count, bumped on every catalog sync so handlers can wait
	/// for the catalog to populate before rendering a playlist.
	ready: watch::Sender<usize>,
	/// Pause flag shared with every rendition pump. While true the pumps stop
	/// reading; renditions discovered later inherit the current value (they
	/// `subscribe()` to this sender).
	paused: watch::Sender<bool>,
	/// Aborts the [`watch_catalog`] task when this `Broadcaster` is dropped. The task
	/// holds only a `Weak<Self>`, so dropping the last external `Arc` runs this `Drop`
	/// (and, transitively, every rendition's [`AbortOnDrop`]), releasing the catalog
	/// and per-track subscriptions instead of leaking them until the broadcast closes.
	_catalog: AbortOnDrop,
}

impl Broadcaster {
	/// Subscribe to `broadcast` and start tracking its renditions.
	pub fn new(broadcast: moq_net::BroadcastConsumer, config: Config) -> Arc<Self> {
		let (ready, _) = watch::channel(0);
		let (paused, _) = watch::channel(false);
		// `new_cyclic` hands the catalog watcher a `Weak<Self>` so it can't keep the
		// `Broadcaster` alive; the `AbortOnDrop` we store then stops that (possibly
		// parked) task the instant the last external `Arc` drops. Without both halves the
		// task's `Arc` (old code) pinned the `Broadcaster` -- and thus every source
		// subscription -- until the broadcast closed on its own.
		Arc::new_cyclic(|weak: &Weak<Self>| {
			let catalog = tokio::spawn(watch_catalog(broadcast.clone(), config, weak.clone()));
			Self {
				broadcast,
				renditions: Mutex::new(BTreeMap::new()),
				ready,
				paused,
				_catalog: AbortOnDrop(catalog.abort_handle()),
			}
		})
	}

	/// Pause or resume pulling media from the broadcast.
	///
	/// While paused, every rendition's pump stops reading its track, so the relay
	/// stops sending and the live media produced during the pause is dropped from the
	/// recording (not buffered, and the publisher isn't kept ingesting). Resuming
	/// continues the SAME playlists from the next group still in the relay cache (the
	/// evicted span is skipped, then it reads forward -- it does NOT jump to live),
	/// marking the first post-resume segment `#EXT-X-DISCONTINUITY`. CMAF sequence
	/// numbers and the init segment persist, so it's one continuous recording with a
	/// gap, not a restart. Idempotent.
	pub fn set_paused(&self, paused: bool) {
		let _ = self.paused.send(paused);
	}

	/// Whether the export is currently paused.
	pub fn is_paused(&self) -> bool {
		*self.paused.borrow()
	}

	pub(crate) fn is_closed(&self) -> bool {
		self.broadcast.is_closed()
	}

	pub(crate) async fn closed(&self) {
		self.broadcast.closed().await;
	}

	/// Look up a rendition by name.
	pub fn rendition(&self, name: &str) -> Option<Arc<Rendition>> {
		self.renditions.lock().unwrap().get(name).cloned()
	}

	/// Wait until at least one rendition has been discovered, or `timeout` elapses.
	pub async fn wait_ready(&self, timeout: Duration) {
		let mut rx = self.ready.subscribe();
		if *rx.borrow() > 0 {
			return;
		}
		let _ = tokio::time::timeout(timeout, async {
			while rx.changed().await.is_ok() {
				if *rx.borrow() > 0 {
					break;
				}
			}
		})
		.await;
	}

	/// Render the multivariant (master) playlist from the current renditions.
	pub fn master_playlist(&self) -> String {
		let renditions = self.renditions.lock().unwrap();
		let mut video = Vec::new();
		let mut audio = Vec::new();
		for rendition in renditions.values() {
			match rendition.kind {
				Kind::Video => video.push(master::VideoVariant {
					name: rendition.name.clone(),
					bandwidth: rendition.bandwidth,
					width: rendition.width,
					height: rendition.height,
					codec: rendition.codec.clone(),
				}),
				Kind::Audio => audio.push(master::AudioVariant {
					name: rendition.name.clone(),
					bandwidth: rendition.bandwidth,
					codec: rendition.codec.clone(),
				}),
			}
		}
		master::render_master(&video, &audio)
	}

	/// Add renditions newly present in `catalog`. Renditions are not removed when
	/// they disappear; their stores simply go stale (rare for a live broadcast).
	fn sync(&self, broadcast: &moq_net::BroadcastConsumer, config: &Config, catalog: &Catalog) {
		let mut renditions = self.renditions.lock().unwrap();
		for (name, video) in &catalog.video.renditions {
			renditions.entry(name.clone()).or_insert_with(|| {
				Arc::new(Rendition::video(
					name.clone(),
					video,
					broadcast.clone(),
					config,
					self.paused.subscribe(),
				))
			});
		}
		for (name, audio) in &catalog.audio.renditions {
			renditions.entry(name.clone()).or_insert_with(|| {
				Arc::new(Rendition::audio(
					name.clone(),
					audio,
					broadcast.clone(),
					config,
					self.paused.subscribe(),
				))
			});
		}
		let _ = self.ready.send(renditions.len());
	}
}

async fn watch_catalog(broadcast: moq_net::BroadcastConsumer, config: Config, broadcaster: Weak<Broadcaster>) {
	let mut consumer = loop {
		match catalog::Consumer::<()>::new(&broadcast, CatalogFormat::Hang) {
			Ok(consumer) => break consumer,
			Err(err) => {
				tracing::warn!(%err, "failed to subscribe to broadcast catalog, retrying");
				tokio::select! {
					_ = tokio::time::sleep(CATALOG_RETRY) => {}
					_ = kio::wait(|waiter| broadcast.poll_closed(waiter)) => return,
				}
			}
		}
	};

	loop {
		match kio::wait(|waiter| consumer.poll_next(waiter)).await {
			// Upgrade per catalog rather than holding an `Arc`: if the owner has dropped
			// the `Broadcaster`, stop (our `AbortOnDrop` will also have aborted us, but a
			// clean exit is fine if that race hasn't run yet).
			Ok(Some(catalog)) => match broadcaster.upgrade() {
				Some(broadcaster) => broadcaster.sync(&broadcast, &config, &catalog),
				None => break,
			},
			Ok(None) => break,
			Err(err) => {
				tracing::warn!(%err, "broadcast catalog stream ended with error");
				break;
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Dropping a `Broadcaster` must release its source subscription, not pin it until
	/// the broadcast closes on its own. Regression for the VOD recorder leaving demo
	/// publishers "subscribed" (and, being subscription-driven, emulating + encoding)
	/// for hours after a recording was deleted: the catalog watcher held an
	/// `Arc<Broadcaster>` and the rendition pumps only exited on broadcast close, so
	/// nothing dropped when the recorder task ended.
	#[tokio::test]
	async fn dropping_broadcaster_releases_subscription() {
		let mut producer = moq_net::Broadcast::new().produce();
		let catalog = producer
			.create_track(moq_net::Track {
				name: "catalog.json".to_string(),
				priority: 0,
			})
			.unwrap();

		let broadcaster = Broadcaster::new(producer.consume(), Config::default());

		// The catalog watcher subscribes to `catalog.json`; wait until it actually has.
		tokio::time::timeout(Duration::from_secs(5), catalog.used())
			.await
			.expect("export should subscribe to the catalog track")
			.unwrap();

		// Dropping the export must release that subscription so the producer sees no
		// consumers. Before the fix this timed out: the watcher's `Arc` kept the
		// `Broadcaster` (and its subscription) alive until the broadcast closed.
		drop(broadcaster);
		tokio::time::timeout(Duration::from_secs(5), catalog.unused())
			.await
			.expect("dropping the Broadcaster must release the catalog subscription")
			.unwrap();
	}
}
