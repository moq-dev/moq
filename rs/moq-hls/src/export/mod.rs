//! Export: subscribe to a MoQ broadcast and turn it into HLS / LL-HLS.
//!
//! A [`Broadcaster`] watches one broadcast's catalog and, per rendition, runs a
//! [`moq_mux::container::fmp4::Export`] narrowed to that single track (via
//! [`moq_mux::catalog::Filter`]) feeding a [`store::SegmentStore`]. The HTTP
//! [`server`](crate::server) reads the stores to answer playlist and segment
//! requests.

mod master;
mod playlist;
mod rendition;
pub mod store;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use moq_mux::catalog::hang::Catalog;
use moq_mux::catalog::{self, CatalogFormat, Stream};
use tokio::sync::watch;

pub use playlist::render_media;
pub use rendition::{Kind, Rendition};

/// The query string to append to every playlist URI, so token-gated sub-requests
/// (media playlists, segments, parts, init) carry the auth token: `?{query}` when
/// present, else empty. Export URIs never carry a query of their own, so it's
/// always a fresh `?`. Pass `None` for public broadcasts (and the auth-less
/// standalone binary).
fn query_suffix(query: Option<&str>) -> String {
	query.map(|q| format!("?{q}")).unwrap_or_default()
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
	renditions: Mutex<BTreeMap<String, Arc<Rendition>>>,
	/// Current rendition count, bumped on every catalog sync so handlers can wait
	/// for the catalog to populate before rendering a playlist.
	ready: watch::Sender<usize>,
	/// Shared segment-boundary timeline: the primary video publishes boundaries
	/// and the others align to them (see [`store::SegmentClock`]).
	clock: Arc<store::SegmentClock>,
	/// Name of the rendition designated the alignment leader (the primary video),
	/// assigned once so it stays stable as the catalog updates.
	leader: Mutex<Option<String>>,
}

impl Broadcaster {
	/// Subscribe to `broadcast` and start tracking its renditions.
	pub fn new(broadcast: moq_net::BroadcastConsumer, config: Config) -> Arc<Self> {
		let (ready, _) = watch::channel(0);
		let broadcaster = Arc::new(Self {
			renditions: Mutex::new(BTreeMap::new()),
			ready,
			clock: store::SegmentClock::new(),
			leader: Mutex::new(None),
		});
		tokio::spawn(watch_catalog(broadcast, config, broadcaster.clone()));
		broadcaster
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
	///
	/// `query` is appended to each rendition's media-playlist URI so a token-gated
	/// player carries the token onto its sub-requests; pass `None` for public
	/// broadcasts.
	pub fn master_playlist(&self, query: Option<&str>) -> String {
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
		master::render_master(&video, &audio, query)
	}

	/// Add renditions newly present in `catalog`. Renditions are not removed when
	/// they disappear; their stores simply go stale (rare for a live broadcast).
	fn sync(&self, broadcast: &moq_net::BroadcastConsumer, config: &Config, catalog: &Catalog) {
		let mut renditions = self.renditions.lock().unwrap();
		// Designate one primary video as the alignment leader (deterministically the
		// first by name), assigned once so it stays stable across catalog updates.
		let primary = catalog.video.renditions.keys().min().cloned();
		let mut leader = self.leader.lock().unwrap();
		for (name, video) in &catalog.video.renditions {
			renditions.entry(name.clone()).or_insert_with(|| {
				let role = if leader.is_none() && Some(name) == primary.as_ref() {
					*leader = Some(name.clone());
					store::Role::Leader
				} else {
					store::Role::Follower
				};
				Arc::new(Rendition::video(
					name.clone(),
					video,
					broadcast.clone(),
					config,
					role,
					self.clock.clone(),
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
					self.clock.clone(),
				))
			});
		}
		let _ = self.ready.send(renditions.len());
	}
}

async fn watch_catalog(broadcast: moq_net::BroadcastConsumer, config: Config, broadcaster: Arc<Broadcaster>) {
	let mut consumer = match catalog::Consumer::<()>::new(&broadcast, CatalogFormat::Hang).await {
		Ok(consumer) => consumer,
		Err(err) => {
			tracing::warn!(%err, "failed to subscribe to broadcast catalog");
			return;
		}
	};

	loop {
		match kio::wait(|waiter| consumer.poll_next(waiter)).await {
			Ok(Some(catalog)) => broadcaster.sync(&broadcast, &config, &catalog),
			Ok(None) => break,
			Err(err) => {
				tracing::warn!(%err, "broadcast catalog stream ended with error");
				break;
			}
		}
	}
}
