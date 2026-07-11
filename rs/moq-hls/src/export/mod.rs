//! Export: serve a MoQ broadcast as HLS, fetching media on demand.
//!
//! A [`Broadcaster`] subscribes to one broadcast's catalog and, per rendition, to its
//! timeline track (see [`hang::timeline`]). That is the *only* standing traffic: playlists
//! are rendered from timeline records (each record maps a group to its start timestamp), and
//! media bytes move only when an HTTP client requests a segment, which FETCHes exactly the
//! groups that segment covers from the relay cache and transmuxes them to CMAF. Renditions
//! whose catalog entry advertises no timeline can't be served this way and are skipped.

mod master;
mod playlist;
mod rendition;
mod timeline;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use moq_mux::catalog::hang::Catalog;
use moq_mux::catalog::{self, CatalogFormat, Stream};
use tokio::sync::watch;

pub use playlist::{Segment, Snapshot, render_media};
pub use rendition::{Kind, Rendition};

/// How long to wait before retrying the initial catalog subscription.
const CATALOG_RETRY: Duration = Duration::from_millis(250);

/// Export tuning shared across renditions.
///
/// Construct via [`Config::default`] and set the fields you need, so new options
/// stay additive.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// Minimum duration of media listed in each rendition's playlist window. Older timeline
	/// records are evicted once the remaining segments still cover this span; keep it within
	/// the relay's group-cache retention, since segments are fetched from there on request.
	pub window: Duration,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			window: Duration::from_secs(16),
		}
	}
}

/// All renditions of one broadcast, kept in sync with its catalog.
pub struct Broadcaster {
	broadcast: moq_net::broadcast::Consumer,
	/// Keyed by `(kind, name)` so a video and an audio rendition can share a name
	/// without one silently evicting the other.
	renditions: Mutex<BTreeMap<(Kind, String), Arc<Rendition>>>,
	/// Current rendition count, bumped on every catalog sync so handlers can wait
	/// for the catalog to populate before rendering a playlist.
	ready: watch::Sender<usize>,
	/// Aborts the catalog watcher when the broadcaster is dropped (and, with it,
	/// its renditions, whose own `Drop` aborts their timeline watchers). Set once,
	/// right after construction.
	watcher: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Broadcaster {
	/// Resolve `source`'s catalog broadcast and start tracking its renditions.
	pub async fn new(source: moq_mux::Source, config: Config) -> Result<Arc<Self>, moq_mux::Error> {
		let broadcast = source.broadcast().await?;
		let (ready, _) = watch::channel(0);
		let broadcaster = Arc::new(Self {
			broadcast: broadcast.clone(),
			renditions: Mutex::new(BTreeMap::new()),
			ready,
			watcher: Mutex::new(None),
		});
		// The watcher holds a Weak so it can't keep the Broadcaster alive; the
		// Broadcaster owns the watcher's handle and aborts it on Drop.
		let watcher = tokio::spawn(watch_catalog(source, broadcast, config, Arc::downgrade(&broadcaster)));
		*broadcaster.watcher.lock().unwrap() = Some(watcher);
		Ok(broadcaster)
	}

	pub(crate) fn is_closed(&self) -> bool {
		self.broadcast.is_closed()
	}

	pub(crate) async fn closed(&self) {
		self.broadcast.closed().await;
	}

	/// Look up a rendition by kind and name. Video and audio are separate axes, so a
	/// video and an audio rendition may share a name without colliding.
	pub fn rendition(&self, kind: Kind, name: &str) -> Option<Arc<Rendition>> {
		self.renditions.lock().unwrap().get(&(kind, name.to_string())).cloned()
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
	/// they disappear; their timelines simply end (rare for a live broadcast).
	fn sync(&self, source: &moq_mux::Source, config: &Config, catalog: &Catalog) {
		let mut renditions = self.renditions.lock().unwrap();
		for (name, video) in &catalog.video.renditions {
			let key = (Kind::Video, name.clone());
			if renditions.contains_key(&key) {
				continue;
			}
			match Rendition::video(name.clone(), video, source, config.window) {
				Some(rendition) => {
					renditions.insert(key, Arc::new(rendition));
				}
				None => tracing::warn!(%name, "skipping video rendition without a timeline track"),
			}
		}
		for (name, audio) in &catalog.audio.renditions {
			let key = (Kind::Audio, name.clone());
			if renditions.contains_key(&key) {
				continue;
			}
			match Rendition::audio(name.clone(), audio, source, config.window) {
				Some(rendition) => {
					renditions.insert(key, Arc::new(rendition));
				}
				None => tracing::warn!(%name, "skipping audio rendition without a timeline track"),
			}
		}
		let _ = self.ready.send(renditions.len());
	}
}

impl Drop for Broadcaster {
	fn drop(&mut self) {
		if let Some(watcher) = self.watcher.lock().unwrap().take() {
			watcher.abort();
		}
	}
}

async fn watch_catalog(
	source: moq_mux::Source,
	broadcast: moq_net::broadcast::Consumer,
	config: Config,
	broadcaster: Weak<Broadcaster>,
) {
	let mut consumer = loop {
		match catalog::Consumer::<()>::new(&broadcast, CatalogFormat::Hang).await {
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
			Ok(Some(catalog)) => match broadcaster.upgrade() {
				Some(broadcaster) => broadcaster.sync(&source, &config, &catalog),
				// The Broadcaster was dropped; nothing left to sync into.
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

	fn frame(micros: u64, keyframe: bool) -> moq_mux::container::Frame {
		moq_mux::container::Frame {
			timestamp: moq_net::Timestamp::from_micros(micros).unwrap(),
			payload: bytes::Bytes::from_static(&[0xDE, 0xAD, 0xBE, 0xEF]),
			keyframe,
			duration: None,
		}
	}

	async fn wait_playable(rendition: &Rendition) {
		let mut rx = rendition.updated();
		tokio::time::timeout(Duration::from_secs(5), async {
			while !rendition.playable() {
				rx.changed().await.expect("rendition watcher alive");
			}
		})
		.await
		.expect("rendition never became playable");
	}

	// The whole fetch-on-demand path in process: a broadcast publishes media through the
	// catalog (which records the timeline), the Broadcaster renders playlists from the
	// timeline alone, and a segment request fetches and transmuxes exactly its groups.
	#[tokio::test]
	async fn serves_playlist_and_segments_from_the_timeline() {
		let origin = moq_net::Origin::random().produce();
		let mut broadcast = origin.create_broadcast("live").expect("publish allowed");
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();

		let reserved = catalog.reserve();
		let mut registration = reserved.video("video0");
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.framerate = Some(30.0);
		registration.set(config);
		drop(reserved);

		// Three GOPs, 2s apart: groups 0 and 1 are complete, group 2 is the live edge.
		let track = broadcast.create_track("video0", None).unwrap();
		let mut media = catalog.media_producer(track, moq_mux::catalog::hang::Container::Legacy);
		media.write(frame(0, true)).unwrap();
		media.write(frame(1_000_000, false)).unwrap();
		media.write(frame(2_000_000, true)).unwrap();
		media.write(frame(3_000_000, false)).unwrap();
		media.write(frame(4_000_000, true)).unwrap();

		let source = moq_mux::Source::new(origin.consume(), "live");
		let broadcaster = Broadcaster::new(source, Config::default()).await.unwrap();
		broadcaster.wait_ready(Duration::from_secs(5)).await;
		let rendition = broadcaster
			.rendition(Kind::Video, "video0")
			.expect("rendition discovered from the catalog");
		wait_playable(&rendition).await;

		let master = broadcaster.master_playlist();
		assert!(master.contains("video/video0/media.m3u8"), "master lists the rendition");

		let playlist = rendition.playlist();
		assert_eq!(playlist.segments.len(), 2, "the live-edge group is not listed");
		assert_eq!(playlist.segments[0].group, 0);
		assert_eq!(playlist.segments[0].duration, 2.0);
		assert_eq!(playlist.segments[1].group, 1);
		assert_eq!(playlist.target_duration, 2, "ceil of the longest timeline gap");
		assert!(!playlist.finished);

		let rendered = render_media(&playlist);
		assert!(rendered.contains("#EXT-X-MAP:URI=\"init.mp4\"\n"));
		assert!(rendered.contains("seg/0.m4s\n"));
		assert!(rendered.contains("seg/1.m4s\n"));

		let init = rendition.init().await.unwrap().expect("init segment");
		assert_eq!(&init[4..8], b"ftyp");

		let segment = rendition.segment(0).await.unwrap().expect("segment fetched on demand");
		assert_eq!(&segment[4..8], b"moof", "a fetched group transmuxes to moof+mdat");

		// The live-edge group isn't a segment yet, and unknown groups miss.
		assert!(rendition.segment(2).await.unwrap().is_none());
		assert!(rendition.segment(99).await.unwrap().is_none());

		// Keep the publisher alive for the whole test.
		drop((media, registration, broadcast));
	}
}
