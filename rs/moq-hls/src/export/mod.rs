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
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use hang::catalog::{AudioConfig, VideoConfig};
use moq_mux::catalog::hang::Catalog;
use moq_mux::catalog::{CatalogFormat, Consumer, Select, Stream};
use moq_mux::container::fmp4::Export;
use moq_mux::select;

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
	/// Expected segment duration, and the seed for `EXT-X-TARGETDURATION`.
	///
	/// Audio renditions roll a segment once they reach this duration. Video rolls on GOP
	/// boundaries instead, so for video this is only the expected GOP length: if the
	/// encoder's actual GOPs run longer, `EXT-X-TARGETDURATION` latches up to match.
	pub segment_target: Duration,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			part_target: Duration::from_millis(500),
			window: Duration::from_secs(16),
			latency: Duration::from_secs(10),
			segment_target: Duration::from_secs(2),
		}
	}
}

/// Monotonically increasing rendition-set generation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Generation(u64);

impl Generation {
	/// Return the numeric generation, starting at zero before the first change.
	pub fn get(self) -> u64 {
		self.0
	}

	fn next(self) -> Self {
		Self(self.0.checked_add(1).expect("rendition generation overflow"))
	}
}

/// An atomic view of one rendition-set generation.
///
/// Doubles as the driver's own shared state: a reader's snapshot is a clone of it.
#[derive(Clone, Default)]
pub struct Snapshot {
	generation: Generation,
	renditions: BTreeMap<String, Arc<Rendition>>,
}

impl Snapshot {
	/// The generation represented by this snapshot.
	pub fn generation(&self) -> Generation {
		self.generation
	}

	/// Look up a rendition by name in this generation.
	pub fn rendition(&self, name: &str) -> Option<Arc<Rendition>> {
		self.renditions.get(name).cloned()
	}

	/// Whether this generation contains no renditions.
	pub fn is_empty(&self) -> bool {
		self.renditions.is_empty()
	}

	/// Every rendition in this generation, in name order.
	pub fn renditions(&self) -> Vec<Arc<Rendition>> {
		self.renditions.values().cloned().collect()
	}

	/// Render the multivariant playlist for exactly this generation.
	pub fn master_playlist(&self) -> String {
		let mut video = Vec::new();
		let mut audio = Vec::new();
		for rendition in self.renditions.values() {
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

#[derive(Clone, PartialEq)]
enum MediaConfig {
	Video(VideoConfig),
	Audio(AudioConfig),
}

impl MediaConfig {
	fn kind(&self) -> Kind {
		match self {
			Self::Video(_) => Kind::Video,
			Self::Audio(_) => Kind::Audio,
		}
	}

	fn rendition(&self, name: String, config: &Config) -> Rendition {
		match self {
			Self::Video(media) => Rendition::video(name, media, config),
			Self::Audio(media) => Rendition::audio(name, media, config),
		}
	}
}

/// A driver-private per-rendition unit: the shared metadata/store plus the exporter
/// that fills it. `export` is `None` once the rendition has finished -- dropping it
/// then RELEASES the source subscription immediately, instead of holding a live (but
/// no-longer-polled) track open until the whole `Broadcaster` drops. That matters on
/// the error path: moq-mux's exporter returns an error *before* it would internally
/// drop the track, so a rendition that errors while its publisher is still live would
/// otherwise pin that subscription for the rest of the recording (a scoped #2255).
struct Driver {
	config: MediaConfig,
	info: Arc<Rendition>,
	export: Option<RenditionExport>,
}

impl Driver {
	/// True once this rendition's exporter has finished (its subscription released).
	fn done(&self) -> bool {
		self.export.is_none()
	}

	/// Finalize the store (waking blocked readers with an ENDLIST) and drop the
	/// exporter, releasing its source track subscription.
	fn finish(&mut self) {
		if self.export.take().is_some() {
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
	state: kio::Producer<Snapshot>,
	/// While true, the exporters aren't polled: nothing is read, so the relay stops
	/// sending and the media produced during the pause is dropped from the recording.
	paused: bool,
	/// Set once a poll has observed the pause, so the next unpaused poll tags a
	/// `#EXT-X-DISCONTINUITY` at the seam. A pause toggled on and back off between polls
	/// is never observed, so it leaves this false -> no seam.
	///
	/// A paused poll can't tell whether the publisher produced anything during the pause,
	/// so an observed pause always seams. Erring toward a spurious discontinuity (a
	/// decoder reset) beats missing a real one, which would splice a gap into the media
	/// timeline without telling the player.
	paused_observed: bool,
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
		Ok(Self {
			broadcast,
			config,
			catalog,
			catalog_done: false,
			renditions: BTreeMap::new(),
			state: kio::Producer::new(Snapshot::default()),
			paused: false,
			paused_observed: false,
		})
	}

	/// A cheap read handle: the shared rendition set + stores. Cloneable; holds no
	/// subscription and can't keep the export alive past this `Broadcaster`.
	pub fn handle(&self) -> Handle {
		Handle {
			state: self.state.consume(),
		}
	}

	/// Pause or resume pulling media from the broadcast.
	///
	/// While paused the exporters aren't polled, so the relay stops sending and the
	/// live media produced during the pause is dropped from the recording (not
	/// buffered, and the publisher isn't kept ingesting). Resuming continues the SAME
	/// playlists from the next group still in the relay cache (the evicted span is
	/// skipped, then it reads forward -- it does NOT jump to live), marking the first
	/// post-resume segment `#EXT-X-DISCONTINUITY`. CMAF sequence numbers and the init
	/// segment persist, so it's one continuous recording with a gap, not a restart.
	///
	/// Takes `&mut self`: the owner applies pause between polls (e.g. in a
	/// `select!` alongside [`poll`](Self::poll)), so there's no shared pause flag and
	/// no separate forwarding task. Idempotent.
	pub fn set_paused(&mut self, paused: bool) {
		self.paused = paused;
	}

	/// Whether the export is currently paused.
	pub fn is_paused(&self) -> bool {
		self.paused
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
			// Not reading media, so nothing wakes us from the exporters. We must still
			// notice the broadcast closing, or a paused recording would hang forever.
			self.paused_observed = true;
			if self.broadcast.poll_closed(waiter).is_ready() {
				self.finish_all();
			}
		} else {
			// First unpaused poll after an observed pause: tag the seam on every rendition.
			if self.paused_observed {
				for driver in self.renditions.values() {
					driver.info.store.mark_discontinuity();
				}
				self.paused_observed = false;
			}

			for driver in self.renditions.values_mut() {
				// A finished rendition (`export` is `None`) is skipped; while draining, the
				// exporter stays `Some` until an arm below finishes it and breaks.
				if driver.export.is_none() {
					continue;
				}
				loop {
					// Poll into an owned outcome so the `driver.export` borrow is released
					// before the arms touch `driver` (e.g. `finish`, which drops the exporter).
					let outcome = driver.export.as_mut().unwrap().poll_next_fragment(waiter);
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

	/// Reconcile the active rendition drivers with one complete catalog snapshot.
	/// Removed or reconfigured drivers are finished before their replacements become
	/// visible, then the whole new rendition set is published as one generation.
	fn sync(&mut self, catalog: &Catalog) {
		let mut desired = BTreeMap::new();
		for (name, video) in &catalog.video.renditions {
			desired.insert(name.clone(), MediaConfig::Video(video.clone()));
		}
		for (name, audio) in &catalog.audio.renditions {
			// Video wins a same-name collision, matching the previous discovery order.
			desired
				.entry(name.clone())
				.or_insert_with(|| MediaConfig::Audio(audio.clone()));
		}

		let stale: Vec<_> = self
			.renditions
			.iter()
			.filter(|(name, driver)| desired.get(*name) != Some(&driver.config))
			.map(|(name, _)| name.clone())
			.collect();
		let mut changed = !stale.is_empty();
		for name in stale {
			let mut driver = self.renditions.remove(&name).expect("stale rendition exists");
			driver.finish();
		}

		for (name, media) in desired {
			if self.renditions.contains_key(&name) {
				continue;
			}
			changed |= self.insert_rendition(name, media);
		}

		if changed {
			let Ok(mut state) = self.state.write() else {
				unreachable!("broadcaster owns rendition state");
			};
			state.generation = state.generation.next();
			state.renditions = self
				.renditions
				.iter()
				.map(|(name, driver)| (name.clone(), driver.info.clone()))
				.collect();
		}
	}

	/// Register a discovered rendition: build its exporter, add it to the driver map,
	/// returning whether it became active.
	fn insert_rendition(&mut self, name: String, media: MediaConfig) -> bool {
		let kind = media.kind();
		let export = match build_export(&self.broadcast, &name, kind, &self.config) {
			Ok(export) => export,
			Err(err) => {
				// The catalog we're mid-read on lists this track, so subscribing its
				// catalog again can't legitimately fail; if it somehow does, skip the
				// rendition (it just won't be served) rather than abort discovery.
				tracing::warn!(%name, ?kind, %err, "failed to build rendition exporter; skipping");
				return false;
			}
		};
		let info = Arc::new(media.rendition(name.clone(), &self.config));
		self.renditions.insert(
			name,
			Driver {
				config: media,
				info,
				export: Some(export),
			},
		);
		true
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
) -> Result<RenditionExport> {
	let consumer = Consumer::<()>::new(broadcast, CatalogFormat::Hang)?;
	let selection = match kind {
		Kind::Video => select::Broadcast::default().video(select::Video::default().name(name)),
		Kind::Audio => select::Broadcast::default().audio(select::Audio::default().name(name)),
	};
	let filtered = consumer.select(selection);
	Ok(Export::new(broadcast.clone(), filtered)
		.with_fragment_duration(cfg.part_target)
		.with_latency(cfg.latency))
}

/// A cheap, cloneable read handle to a [`Broadcaster`]'s renditions.
///
/// Holds only the shared rendition set + stores, so it can't keep the export alive:
/// when the owning `Broadcaster` (and its driver) is dropped, the stores finish and
/// this handle's reads see the final state.
#[derive(Clone)]
pub struct Handle {
	state: kio::Consumer<Snapshot>,
}

impl Handle {
	/// Capture the current rendition set and generation atomically.
	pub fn snapshot(&self) -> Snapshot {
		self.state.read().clone()
	}

	/// Look up a rendition by name.
	pub fn rendition(&self, name: &str) -> Option<Arc<Rendition>> {
		self.state.read().renditions.get(name).cloned()
	}

	/// Every discovered rendition, in name order. Lets a caller enumerate the
	/// rendition set directly instead of re-parsing the master playlist.
	pub fn renditions(&self) -> Vec<Arc<Rendition>> {
		self.state.read().renditions.values().cloned().collect()
	}

	/// Wait until at least one rendition has been discovered, or `timeout` elapses.
	pub async fn wait_ready(&self, timeout: Duration) {
		let ready = kio::wait(|waiter| {
			match self.state.poll(waiter, |state| {
				if state.renditions.is_empty() {
					Poll::Pending
				} else {
					Poll::Ready(())
				}
			}) {
				Poll::Ready(_) => Poll::Ready(()),
				Poll::Pending => Poll::Pending,
			}
		});
		let _ = tokio::time::timeout(timeout, ready).await;
	}

	/// Subscribe to atomic rendition-set snapshots.
	///
	/// The first [`Changes::changed`] call returns immediately with the current
	/// snapshot. Later calls wait for a different generation.
	pub fn subscribe(&self) -> Changes {
		Changes {
			state: self.state.clone(),
			observed: None,
		}
	}

	/// Render the multivariant (master) playlist from the current renditions.
	pub fn master_playlist(&self) -> String {
		self.snapshot().master_playlist()
	}
}

/// A rendition-set change subscription.
pub struct Changes {
	state: kio::Consumer<Snapshot>,
	observed: Option<Generation>,
}

impl Changes {
	/// Wait for and return the next atomic rendition-set snapshot.
	///
	/// Returns `None` after the owning [`Broadcaster`] is dropped and the final
	/// generation has already been observed.
	pub async fn changed(&mut self) -> Option<Snapshot> {
		let observed = self.observed;
		let snapshot = kio::wait(|waiter| {
			match self.state.poll(waiter, |state| {
				if Some(state.generation) == observed {
					Poll::Pending
				} else {
					// Spelled out: `state` is a guard, so a bare `.clone()` would clone the
					// borrow rather than the snapshot.
					Poll::Ready(Snapshot::clone(state))
				}
			}) {
				Poll::Ready(Ok(snapshot)) => Poll::Ready(Some(snapshot)),
				Poll::Ready(Err(_)) => Poll::Ready(None),
				Poll::Pending => Poll::Pending,
			}
		})
		.await?;
		self.observed = Some(snapshot.generation());
		Some(snapshot)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn video(level: u8) -> VideoConfig {
		let mut video = VideoConfig::new(hang::catalog::H264 {
			profile: 0x42,
			constraints: 0xc0,
			level,
			inline: true,
		});
		video.coded_width = Some(1280);
		video.coded_height = Some(720);
		video.bitrate = Some(2_000_000);
		video
	}

	fn test_broadcaster() -> (moq_net::BroadcastProducer, moq_net::TrackProducer, Broadcaster) {
		let mut producer = moq_net::Broadcast::new().produce();
		let catalog = producer.create_track(moq_net::Track::new("catalog.json")).unwrap();
		let broadcaster = Broadcaster::new(producer.consume(), Config::default()).unwrap();
		(producer, catalog, broadcaster)
	}

	#[tokio::test]
	async fn removes_absent_rendition() {
		use moq_mux::catalog::Producer as CatalogProducer;

		let mut producer = moq_net::Broadcast::new().produce();
		let mut catalog = CatalogProducer::new(&mut producer).unwrap();
		let video_track = producer.create_track(moq_net::Track::new("video")).unwrap();
		let mut broadcaster = Broadcaster::new(producer.consume(), Config::default()).unwrap();
		let handle = broadcaster.handle();
		let mut changes = handle.subscribe();
		assert_eq!(changes.changed().await.unwrap().generation().get(), 0);
		let driver = tokio::spawn(async move { broadcaster.run().await });

		catalog.lock().video.renditions.insert("video".to_string(), video(0x1f));
		let first = tokio::time::timeout(Duration::from_secs(5), changes.changed())
			.await
			.expect("catalog addition should publish a generation")
			.unwrap();
		assert_eq!(first.generation().get(), 1);
		let removed = first.rendition("video").unwrap();
		tokio::time::timeout(Duration::from_secs(5), video_track.used())
			.await
			.expect("exporter should subscribe to the added track")
			.unwrap();

		catalog.lock().video.renditions.clear();
		let empty = tokio::time::timeout(Duration::from_secs(5), changes.changed())
			.await
			.expect("catalog removal should publish a generation")
			.unwrap();
		assert_eq!(empty.generation().get(), 2);
		assert!(empty.renditions().is_empty());
		assert!(removed.store.version().finished);
		assert!(handle.rendition("video").is_none());
		tokio::time::timeout(Duration::from_secs(5), video_track.unused())
			.await
			.expect("removing a rendition should release its media subscription")
			.unwrap();
		driver.abort();
	}

	#[test]
	fn replaces_same_name_when_media_config_changes() {
		let (_producer, _catalog_track, mut broadcaster) = test_broadcaster();
		let handle = broadcaster.handle();
		let mut catalog = Catalog::default();
		catalog.video.renditions.insert("video".to_string(), video(0x1f));
		broadcaster.sync(&catalog);
		let first = handle.rendition("video").unwrap();

		catalog.video.renditions.insert("video".to_string(), video(0x28));
		broadcaster.sync(&catalog);
		let replacement = handle.rendition("video").unwrap();

		assert!(!Arc::ptr_eq(&first, &replacement));
		assert_ne!(first.codec, replacement.codec);
		assert!(first.store.version().finished);
		assert_eq!(handle.snapshot().generation().get(), 2);
	}

	#[test]
	fn readds_rendition_with_a_fresh_store() {
		let (_producer, _catalog_track, mut broadcaster) = test_broadcaster();
		let handle = broadcaster.handle();
		let mut catalog = Catalog::default();
		catalog.video.renditions.insert("video".to_string(), video(0x1f));
		broadcaster.sync(&catalog);
		let first = handle.rendition("video").unwrap();

		catalog.video.renditions.clear();
		broadcaster.sync(&catalog);
		catalog.video.renditions.insert("video".to_string(), video(0x1f));
		broadcaster.sync(&catalog);
		let readded = handle.rendition("video").unwrap();

		assert!(!Arc::ptr_eq(&first, &readded));
		assert!(first.store.version().finished);
		assert!(!readded.store.version().finished);
		assert_eq!(handle.snapshot().generation().get(), 3);
	}

	/// Dropping a `Broadcaster` must release its source subscription, not pin it until
	/// the broadcast closes on its own. Regression for the VOD recorder leaving demo
	/// publishers "subscribed" (and, being subscription-driven, emulating + encoding)
	/// for hours after a recording was deleted (moq#2255): with the poll model, drop
	/// tears down the catalog consumer + exporters structurally, no guards needed.
	#[tokio::test]
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
	#[tokio::test]
	async fn dropping_broadcaster_releases_media_subscription() {
		use moq_mux::catalog::Producer as CatalogProducer;

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

	/// A broadcast that goes away drives the broadcaster to completion instead of
	/// hanging: the catalog stream ends and, with no renditions, `run()` returns.
	#[tokio::test]
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
