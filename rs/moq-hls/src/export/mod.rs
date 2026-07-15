//! Export: subscribe to a MoQ broadcast and turn it into HLS / LL-HLS.
//!
//! A [`Broadcaster`] watches one broadcast's catalog and, per rendition, runs a
//! [`moq_mux::container::fmp4::Export`] narrowed to that single track (via
//! [`moq_mux::catalog::Select`]) feeding a [`store::SegmentStore`].
//!
//! It is a plain owned value the caller drives by polling: [`poll`](Broadcaster::poll)
//! (or the [`run`](Broadcaster::run) convenience) advances the catalog and every
//! rendition's exporter in one pass, with **no** background tasks. Dropping the
//! `Broadcaster` drops its catalog consumer and every exporter, which releases the
//! source subscriptions immediately -- so an owner that stops recording a still-live
//! broadcast tears its subscriptions down instead of leaking them (moq#2255).
//!
//! Readers (the HTTP [`server`](crate::server), the VOD uploader) hold a cheap
//! [`Handle`] instead: the shared rendition set + stores, with no control over the
//! subscriptions and no ability to keep them alive past the driver.

mod master;
mod playlist;
mod rendition;
pub mod store;

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::task::Poll;
use std::time::Duration;

use moq_mux::catalog::hang::Catalog;
use moq_mux::catalog::{CatalogFormat, Consumer, Select, Stream};
use moq_mux::container::fmp4::Export;
use moq_mux::select;
use tokio::sync::watch;

pub use playlist::render_media;
pub use rendition::{Kind, Rendition};

use crate::Result;

/// The per-rendition exporter: a catalog consumer narrowed (via [`Select`]) to one
/// track, wrapped in the fMP4 [`Export`] that emits CMAF fragments.
type RenditionExport = Export<Select<Consumer<()>>>;

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

/// Shared read side, handed out via [`Handle`]. Written only by the driver's
/// [`sync`](Broadcaster::sync); read by playlist renderers and segment handlers.
struct Shared {
	/// name -> rendition metadata + store. Grows as the catalog advertises tracks.
	renditions: RwLock<BTreeMap<String, Arc<Rendition>>>,
	/// Rendition count, bumped on every catalog sync so a handler can wait for the
	/// catalog to populate before rendering a playlist.
	ready: watch::Sender<usize>,
}

/// The lifecycle of one rendition's media subscription.
enum DriverState {
	/// Pulling fragments from an active source subscription.
	Active(Box<RenditionExport>),
	/// Paused with no source subscription. Resume rebuilds at the live edge.
	Paused,
	/// Permanently ended with its store finalized.
	Finished,
}

/// A driver-private per-rendition unit: the shared metadata/store plus the state
/// of the exporter that fills it.
struct Driver {
	info: Arc<Rendition>,
	state: DriverState,
}

impl Driver {
	/// True once this rendition's exporter has finished (its subscription released).
	fn done(&self) -> bool {
		matches!(self.state, DriverState::Finished)
	}

	/// The active exporter, if this rendition is currently pulling media.
	fn active(&mut self) -> Option<&mut RenditionExport> {
		match &mut self.state {
			DriverState::Active(export) => Some(export),
			DriverState::Paused | DriverState::Finished => None,
		}
	}

	/// Drop the active exporter without finalizing the store, synchronously
	/// acknowledging that the media subscription has been released.
	fn pause(&mut self) {
		if matches!(self.state, DriverState::Active(_)) {
			self.state = DriverState::Paused;
		}
	}

	/// Rebuild a paused exporter at the live edge and mark the recording seam.
	fn resume(&mut self, broadcast: &moq_net::BroadcastConsumer, config: &Config) {
		if !matches!(self.state, DriverState::Paused) {
			return;
		}

		let export = match build_export(broadcast, &self.info.name, self.info.kind, config, true) {
			Ok(export) => export,
			Err(err) => {
				tracing::warn!(name = %self.info.name, kind = ?self.info.kind, %err, "failed to resume hls rendition exporter");
				self.finish();
				return;
			}
		};

		if self.info.store.has_media() {
			self.info.store.mark_discontinuity();
		}
		self.state = DriverState::Active(Box::new(export));
	}

	/// Finalize the store (waking blocked readers with an ENDLIST) and drop the
	/// exporter, releasing its source track subscription.
	fn finish(&mut self) {
		if !matches!(self.state, DriverState::Finished) {
			self.state = DriverState::Finished;
			self.info.store.finish();
		}
	}
}

/// All renditions of one broadcast, kept in sync with its catalog and driven by
/// polling. Owns every source subscription; drop it to release them.
pub struct Broadcaster {
	broadcast: moq_net::BroadcastConsumer,
	config: Config,
	/// Catalog consumer used to DISCOVER renditions (each exporter runs its own,
	/// narrowed, catalog consumer for track (un)subscription -- all deduped by
	/// moq-net to one wire subscription).
	catalog: Consumer<()>,
	/// The discovery catalog has ended (broadcast closed) or errored.
	catalog_done: bool,
	renditions: BTreeMap<String, Driver>,
	shared: Arc<Shared>,
	/// While true, rendition drivers hold no exporters or media subscriptions.
	paused: bool,
}

impl Broadcaster {
	/// Subscribe to `broadcast`'s catalog and start tracking its renditions.
	///
	/// Fails loud if the catalog can't be subscribed: a broadcast must publish its
	/// catalog before it is announced (the relay guarantees this; a local publisher
	/// must create the catalog before `publish_broadcast`). There is no retry -- a
	/// failure here is a real publish-ordering bug, not a transient.
	pub fn new(broadcast: moq_net::BroadcastConsumer, config: Config) -> Result<Self> {
		let catalog = Consumer::<()>::new(&broadcast, CatalogFormat::Hang)?;
		let (ready, _) = watch::channel(0);
		Ok(Self {
			broadcast,
			config,
			catalog,
			catalog_done: false,
			renditions: BTreeMap::new(),
			shared: Arc::new(Shared {
				renditions: RwLock::new(BTreeMap::new()),
				ready,
			}),
			paused: false,
		})
	}

	/// A cheap read handle: the shared rendition set + stores. Cloneable; holds no
	/// subscription and can't keep the export alive past this `Broadcaster`.
	pub fn handle(&self) -> Handle {
		Handle {
			shared: self.shared.clone(),
		}
	}

	/// Pause or resume pulling media from the broadcast.
	///
	/// Entering pause synchronously drops every rendition exporter, releasing its media
	/// subscription before this method returns. The discovery catalog stays subscribed
	/// so newly advertised renditions are known, but no media is pulled.
	///
	/// Resuming creates fresh exporters at each track's latest available group. The
	/// playlists and their sequence numbers persist, and the first post-resume segment
	/// is tagged `#EXT-X-DISCONTINUITY` when the rendition already contained media.
	///
	/// Takes `&mut self`: the owner applies pause between polls (e.g. in a
	/// `select!` alongside [`poll`](Self::poll)), so there's no shared pause flag and
	/// no separate forwarding task. Idempotent.
	pub fn set_paused(&mut self, paused: bool) {
		if self.paused == paused {
			return;
		}

		self.paused = paused;
		if paused {
			for driver in self.renditions.values_mut() {
				driver.pause();
			}
		} else {
			for driver in self.renditions.values_mut() {
				driver.resume(&self.broadcast, &self.config);
			}
		}
	}

	/// Whether the export is currently paused.
	pub fn is_paused(&self) -> bool {
		self.paused
	}

	/// Stop the export and establish a synchronous no-more-writes barrier.
	///
	/// This consumes the broadcaster, releases every source subscription, and
	/// finalizes retained stores before returning. Dropping a broadcaster performs
	/// the same cleanup, while this method makes the barrier explicit to callers.
	pub fn shutdown(mut self) {
		self.finish_all();
	}

	/// Advance the catalog and every rendition's exporter one pass.
	///
	/// - Drains catalog snapshots (even while paused, so the rendition set / a
	///   reader's `wait_ready` still resolve), adding newly advertised renditions.
	/// - Unless paused, drains each exporter into its store.
	/// - Returns `Ready(())` once the catalog has ended and every rendition has
	///   finished; `Pending` otherwise.
	///
	/// A source ending -- whether cleanly (`finish()`) or abruptly (the publisher
	/// disconnecting, the common live case) -- finishes that rendition's store and
	/// completes it; an abrupt end is logged, not propagated, since for a live
	/// broadcast it is the normal termination, not a fault.
	///
	/// Cancel-safe: every underlying poll is cancel-safe and all cursor state lives on
	/// `self`, so dropping the future mid-poll and re-entering resumes cleanly.
	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		// 1. Discover renditions from the catalog. Runs regardless of pause.
		while !self.catalog_done {
			match self.catalog.poll_next(waiter) {
				Poll::Ready(Ok(Some(catalog))) => self.sync(&catalog),
				Poll::Ready(Ok(None)) => self.catalog_done = true,
				Poll::Ready(Err(err)) => {
					// The catalog track ended abruptly (publisher gone): stop discovering
					// and let the media tracks drain to completion on their own.
					tracing::warn!(%err, "broadcast catalog stream ended");
					self.catalog_done = true;
				}
				Poll::Pending => break,
			}
		}

		if self.paused {
			// No media subscriptions remain, so only the catalog or broadcast can wake us.
			// We must still notice the broadcast closing, or a paused recording would
			// hang forever.
			if self.broadcast.poll_closed(waiter).is_ready() {
				self.finish_all();
			}
		} else {
			for driver in self.renditions.values_mut() {
				while let Some(export) = driver.active() {
					// Poll into an owned outcome so the active-state borrow is released
					// before the arms touch `driver` (e.g. `finish`, which drops the exporter).
					let outcome = export.poll_next_fragment(waiter);
					match outcome {
						Poll::Ready(Ok(Some(fragment))) => driver.info.store.push(fragment),
						Poll::Ready(Ok(None)) => {
							driver.finish();
							break;
						}
						Poll::Ready(Err(err)) => {
							tracing::warn!(name = %driver.info.name, ?driver.info.kind, %err, "hls rendition exporter ended");
							driver.finish();
							break;
						}
						Poll::Pending => break,
					}
				}
			}
		}

		// Done once the catalog has ended and every rendition has finished.
		if self.catalog_done && self.renditions.values().all(Driver::done) {
			return Poll::Ready(());
		}

		Poll::Pending
	}

	/// Drive the broadcaster to completion. Convenience for owners with no pause
	/// signal (the HTTP server); a pausing owner writes its own `select!` over
	/// [`poll`](Self::poll) instead.
	pub async fn run(&mut self) {
		kio::wait(|waiter| self.poll(waiter)).await
	}

	/// Finish every rendition's store (used when the broadcast closes while paused,
	/// so a paused recording terminates instead of hanging).
	fn finish_all(&mut self) {
		self.catalog_done = true;
		for driver in self.renditions.values_mut() {
			driver.finish();
		}
	}

	/// Add renditions newly present in `catalog`. Renditions are add-only: one that
	/// disappears from the catalog keeps its store (rare for a live broadcast, and
	/// dropping it would break a player mid-stream). Removal-on-drop is now possible
	/// (drop the `Driver` = release its subscription) but left as a follow-up.
	fn sync(&mut self, catalog: &Catalog) {
		for (name, video) in &catalog.video.renditions {
			if self.renditions.contains_key(name) {
				continue;
			}
			let info = Arc::new(Rendition::video(name.clone(), video, &self.config));
			self.insert_rendition(name.clone(), info, Kind::Video);
		}
		for (name, audio) in &catalog.audio.renditions {
			if self.renditions.contains_key(name) {
				continue;
			}
			let info = Arc::new(Rendition::audio(name.clone(), audio, &self.config));
			self.insert_rendition(name.clone(), info, Kind::Audio);
		}
		let _ = self.shared.ready.send(self.renditions.len());
	}

	/// Register a discovered rendition: build its exporter, add it to the driver map,
	/// and publish its metadata/store to the shared read side.
	fn insert_rendition(&mut self, name: String, info: Arc<Rendition>, kind: Kind) {
		let state = if self.paused {
			DriverState::Paused
		} else {
			let export = match build_export(&self.broadcast, &name, kind, &self.config, false) {
				Ok(export) => export,
				Err(err) => {
					// The catalog we're mid-read on lists this track, so subscribing its
					// catalog again can't legitimately fail; if it somehow does, skip the
					// rendition (it just won't be served) rather than abort discovery.
					tracing::warn!(%name, ?kind, %err, "failed to build rendition exporter; skipping");
					return;
				}
			};
			DriverState::Active(Box::new(export))
		};
		self.renditions.insert(
			name.clone(),
			Driver {
				info: info.clone(),
				state,
			},
		);
		self.shared.renditions.write().unwrap().insert(name, info);
	}
}

/// Build a per-track exporter: subscribe the catalog, narrow it to `name` on the
/// `kind` axis so the exporter sees exactly one track, and cap fragment duration +
/// latency from the config.
fn build_export(
	broadcast: &moq_net::BroadcastConsumer,
	name: &str,
	kind: Kind,
	cfg: &Config,
	start_at_live: bool,
) -> Result<RenditionExport> {
	let consumer = Consumer::<()>::new(broadcast, CatalogFormat::Hang)?;
	let selection = match kind {
		Kind::Video => select::Broadcast::default().video(select::Video::default().name(name)),
		Kind::Audio => select::Broadcast::default().audio(select::Audio::default().name(name)),
	};
	let filtered = consumer.select(selection);
	let export = Export::new(broadcast.clone(), filtered)
		.with_fragment_duration(cfg.part_target)
		.with_latency(cfg.latency);
	Ok(if start_at_live {
		export.with_start_at_live()
	} else {
		export
	})
}

impl Drop for Broadcaster {
	fn drop(&mut self) {
		self.finish_all();
	}
}

/// A cheap, cloneable read handle to a [`Broadcaster`]'s renditions.
///
/// Holds only the shared rendition set + stores, so it can't keep the export alive:
/// when the owning `Broadcaster` (and its driver) is dropped, the stores finish and
/// this handle's reads see the final state.
#[derive(Clone)]
pub struct Handle {
	shared: Arc<Shared>,
}

impl Handle {
	/// Look up a rendition by name.
	pub fn rendition(&self, name: &str) -> Option<Arc<Rendition>> {
		self.shared.renditions.read().unwrap().get(name).cloned()
	}

	/// Every discovered rendition, in name order. Lets a caller enumerate the
	/// rendition set directly instead of re-parsing the master playlist.
	pub fn renditions(&self) -> Vec<Arc<Rendition>> {
		self.shared.renditions.read().unwrap().values().cloned().collect()
	}

	/// Wait until at least one rendition has been discovered, or `timeout` elapses.
	pub async fn wait_ready(&self, timeout: Duration) {
		let mut rx = self.shared.ready.subscribe();
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
		let renditions = self.shared.renditions.read().unwrap();
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
}

#[cfg(test)]
mod tests {
	use bytes::Bytes;
	use hang::catalog::{Container, VideoCodec, VideoConfig};
	use moq_mux::catalog::Producer as CatalogProducer;
	use moq_mux::catalog::hang::Container as WireContainer;
	use moq_mux::container::{Frame, Producer as MediaProducer, Timestamp};

	use super::*;

	type VideoProducer = MediaProducer<WireContainer>;

	/// Build one live VP8 rendition with an HLS broadcaster over the same broadcast.
	fn video_fixture() -> (moq_net::BroadcastProducer, CatalogProducer, VideoProducer, Broadcaster) {
		let mut producer = moq_net::Broadcast::new().produce();
		let mut catalog = CatalogProducer::new(&mut producer).unwrap();
		let track = producer.create_track(moq_net::Track::new("video")).unwrap();
		let mut config = VideoConfig::new(VideoCodec::VP8);
		config.container = Container::Legacy;
		config.coded_width = Some(320);
		config.coded_height = Some(240);
		catalog.lock().video.renditions.insert("video".to_string(), config);
		let video = MediaProducer::new(track, WireContainer::Legacy);
		let broadcaster = Broadcaster::new(producer.consume(), Config::default()).unwrap();
		(producer, catalog, video, broadcaster)
	}

	fn frame(seconds: u64) -> Frame {
		Frame {
			timestamp: Timestamp::from_micros(seconds * 1_000_000).unwrap(),
			duration: None,
			payload: Bytes::from_static(&[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a]),
			keyframe: true,
		}
	}

	fn drive(broadcaster: &mut Broadcaster) {
		let _ = broadcaster.poll(&kio::Waiter::noop());
	}

	/// Dropping a `Broadcaster` must release its source subscription, not pin it until
	/// the broadcast closes on its own. Regression for the VOD recorder leaving demo
	/// publishers "subscribed" (and, being subscription-driven, emulating + encoding)
	/// for hours after a recording was deleted (moq#2255): with the poll model, drop
	/// tears down the catalog consumer + exporters structurally, no guards needed.
	#[tokio::test(start_paused = true)]
	async fn dropping_broadcaster_releases_subscription() {
		let mut producer = moq_net::Broadcast::new().produce();
		let catalog = producer
			.create_track(moq_net::Track {
				name: "catalog.json".to_string(),
				priority: 0,
			})
			.unwrap();

		let mut broadcaster = Broadcaster::new(producer.consume(), Config::default()).unwrap();

		// Drive the broadcaster so it actually subscribes to the catalog track, then
		// wait until the producer sees that consumer.
		let driver = tokio::spawn(async move { broadcaster.run().await });
		tokio::time::timeout(Duration::from_secs(5), catalog.used())
			.await
			.expect("export should subscribe to the catalog track")
			.unwrap();

		// Dropping the driver (which owns the Broadcaster) must release that
		// subscription so the producer sees no consumers.
		driver.abort();
		tokio::time::timeout(Duration::from_secs(5), catalog.unused())
			.await
			.expect("dropping the Broadcaster must release the catalog subscription")
			.unwrap();
	}

	/// The real #2255 scenario: a rendition's MEDIA subscription (not just the
	/// catalog) must be released when the driver is dropped. A live media track held
	/// open is what kept the demo's subscription-driven publishers emulating.
	#[tokio::test(start_paused = true)]
	async fn dropping_broadcaster_releases_media_subscription() {
		let mut producer = moq_net::Broadcast::new().produce();
		let mut catalog = CatalogProducer::new(&mut producer).unwrap();
		let video = producer.create_track(moq_net::Track::new("video")).unwrap();
		// List the "video" rendition so the exporter subscribes to that media track.
		catalog.lock().video.renditions.insert(
			"video".to_string(),
			hang::catalog::VideoConfig::new(hang::catalog::H264 {
				profile: 0x42,
				constraints: 0xc0,
				level: 0x1f,
				inline: true,
			}),
		);

		let mut broadcaster = Broadcaster::new(producer.consume(), Config::default()).unwrap();
		let driver = tokio::spawn(async move { broadcaster.run().await });

		// The exporter subscribes to the (still-live) "video" track once it sees the
		// catalog; the track is never finished, so the subscription stays open.
		tokio::time::timeout(Duration::from_secs(5), video.used())
			.await
			.expect("exporter should subscribe to the video track")
			.unwrap();

		// Dropping the driver (owning the Broadcaster -> renditions -> exporters) must
		// release that media subscription, not leave the publisher "subscribed".
		driver.abort();
		tokio::time::timeout(Duration::from_secs(5), video.unused())
			.await
			.expect("dropping the Broadcaster must release the media subscription")
			.unwrap();
	}

	/// Pausing a fragment stalled in an open group drops the exporter immediately,
	/// releasing the media subscription without waiting for another frame or group.
	#[tokio::test(start_paused = true)]
	async fn pause_releases_stalled_media_subscription() {
		let (_producer, _catalog, mut video, mut broadcaster) = video_fixture();
		video.write(frame(0)).unwrap();

		// The exporter consumes the only frame but cannot flush a media fragment until
		// another keyframe or end-of-group supplies its boundary.
		drive(&mut broadcaster);
		video.used().await.unwrap();

		broadcaster.set_paused(true);
		assert!(broadcaster.is_paused());
		tokio::time::timeout(Duration::from_secs(1), video.unused())
			.await
			.expect("pause should release a stalled media subscription")
			.unwrap();

		let rendition = broadcaster.handle().rendition("video").unwrap();
		assert!(
			!rendition.store.snapshot().finished,
			"pause must keep the playlist resumable"
		);
	}

	/// Resuming after cache eviction rebuilds at the latest retained group instead of
	/// replaying the cache, while preserving the playlist and marking its seam.
	#[tokio::test(start_paused = true)]
	async fn resume_after_cache_eviction_starts_at_live_edge() {
		let (_producer, _catalog, mut video, mut broadcaster) = video_fixture();
		let handle = broadcaster.handle();

		video.write(frame(0)).unwrap();
		video.write(frame(1)).unwrap();
		drive(&mut broadcaster);
		let rendition = handle.rendition("video").unwrap();
		assert_eq!(rendition.store.snapshot().segments.len(), 1, "initial segment");

		broadcaster.set_paused(true);
		video.unused().await.unwrap();

		// Age the pre-pause groups out, then leave two fresh groups cached. A consumer
		// starting at the cache head would record group 2; a live-edge consumer starts
		// at group 3 and waits for group 4 to close it.
		tokio::time::advance(Duration::from_secs(6)).await;
		video.write(frame(2)).unwrap();
		video.write(frame(3)).unwrap();

		broadcaster.set_paused(false);
		drive(&mut broadcaster);
		assert_eq!(
			rendition.store.snapshot().segments.len(),
			1,
			"cached group was not replayed"
		);

		video.write(frame(4)).unwrap();
		drive(&mut broadcaster);
		let snapshot = rendition.store.snapshot();
		assert_eq!(snapshot.segments.len(), 2, "exactly one post-resume segment");
		assert!(snapshot.segments[1].discontinuity, "post-resume segment marks the gap");
	}

	/// Explicit shutdown is a no-more-writes barrier: it releases an active source
	/// subscription and finalizes retained stores before returning.
	#[tokio::test(start_paused = true)]
	async fn shutdown_active_source_finishes_store() {
		let (_producer, _catalog, mut video, mut broadcaster) = video_fixture();
		video.write(frame(0)).unwrap();
		drive(&mut broadcaster);
		video.used().await.unwrap();
		let rendition = broadcaster.handle().rendition("video").unwrap();

		broadcaster.shutdown();

		video.unused().await.unwrap();
		assert!(
			rendition.store.snapshot().finished,
			"shutdown must finalize retained stores"
		);
	}

	/// A broadcast that goes away drives the broadcaster to completion instead of
	/// hanging: the catalog stream ends and, with no renditions, `run()` returns.
	#[tokio::test(start_paused = true)]
	async fn broadcast_gone_completes() {
		let mut producer = moq_net::Broadcast::new().produce();
		let catalog = producer
			.create_track(moq_net::Track {
				name: "catalog.json".to_string(),
				priority: 0,
			})
			.unwrap();
		let mut broadcaster = Broadcaster::new(producer.consume(), Config::default()).unwrap();

		// No renditions ever appear; dropping the catalog track + producer ends the
		// discovery catalog stream, so the broadcaster completes.
		drop(catalog);
		drop(producer);
		tokio::time::timeout(Duration::from_secs(5), broadcaster.run())
			.await
			.expect("broadcaster should complete when the broadcast goes away");
	}
}
