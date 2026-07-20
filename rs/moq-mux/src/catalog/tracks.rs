use std::marker::PhantomData;
use std::time::Duration;

use moq_net::Timestamp;

use super::Producer;
use super::hang::{Catalog, CatalogExt};
use crate::container::jitter::Metrics;

/// The catalog fields a [`Rendition`] can measure from the frames fed to it.
///
/// An absent field is one to measure: whatever the config already carries when it reaches
/// [`Rendition::set`] is authoritative and left alone, and the rest is detected and kept current.
#[derive(Clone, Default, Debug, PartialEq)]
#[non_exhaustive]
pub struct Estimate {
	/// The maximum jitter before the next frame is emitted.
	pub jitter: Option<Duration>,
	/// The maximum bitrate in bits per second.
	pub bitrate: Option<u64>,
}

impl Estimate {
	/// Set the jitter (or clear it with `None`).
	pub fn with_jitter(mut self, jitter: impl Into<Option<Duration>>) -> Self {
		self.jitter = jitter.into();
		self
	}

	/// Set the bitrate in bits per second (or clear it with `None`).
	pub fn with_bitrate(mut self, bitrate: impl Into<Option<u64>>) -> Self {
		self.bitrate = bitrate.into();
		self
	}
}

/// A catalog config that can be published as a named rendition.
///
/// Implement it on your own config type to get the full [`Rendition`] lifecycle for a custom
/// track: reservation gating, removal on drop, and optional jitter/bitrate detection.
/// [`VideoConfig`](hang::catalog::VideoConfig) and [`AudioConfig`](hang::catalog::AudioConfig)
/// implement it for every extension; a custom config implements it for the one [`CatalogExt`] that
/// holds it:
///
/// ```
/// # use moq_mux::catalog::{Estimate, RenditionConfig};
/// # use moq_mux::catalog::hang::{Catalog, CatalogExt};
/// # use serde::{Deserialize, Serialize};
/// # use std::collections::BTreeMap;
/// #[derive(Serialize, Deserialize, Clone, Default)]
/// struct MyExt {
///     telemetry: BTreeMap<String, Telemetry>,
/// }
/// impl CatalogExt for MyExt {}
///
/// #[derive(Serialize, Deserialize, Clone, Default)]
/// struct Telemetry {
///     schema: String,
///     bitrate: Option<u64>,
/// }
///
/// impl RenditionConfig<MyExt> for Telemetry {
///     fn insert(self, catalog: &mut Catalog<MyExt>, name: &str) {
///         catalog.telemetry.insert(name.to_string(), self);
///     }
///     fn get_mut<'a>(catalog: &'a mut Catalog<MyExt>, name: &str) -> Option<&'a mut Self> {
///         catalog.telemetry.get_mut(name)
///     }
///     fn remove(catalog: &mut Catalog<MyExt>, name: &str) {
///         catalog.telemetry.remove(name);
///     }
///
///     // Opt into bitrate detection; jitter is left undetected.
///     fn estimate(&self) -> Estimate {
///         Estimate::default().with_bitrate(self.bitrate)
///     }
///     fn set_estimate(&mut self, estimate: Estimate) {
///         self.bitrate = estimate.bitrate;
///     }
/// }
/// ```
///
/// Note that `insert` takes the whole [`Catalog`], not just the extension, so the built-in media
/// configs use the same trait. Writing to `catalog.video` / `catalog.audio` from a custom config
/// fights the media pipeline for those sections; stay in your own.
///
/// To advertise a timeline for a custom track, set your config's timeline field from
/// [`catalog::Producer::timeline`](crate::catalog::Producer::timeline) before [`set`](Rendition::set)
/// (the same way an importer does), and record group opens through its recorder.
pub trait RenditionConfig<E: CatalogExt>: Sized + 'static {
	/// Insert or replace this config under `name`.
	fn insert(self, catalog: &mut Catalog<E>, name: &str);

	/// Borrow the config stored under `name`, if it's present.
	fn get_mut<'a>(catalog: &'a mut Catalog<E>, name: &str) -> Option<&'a mut Self>;

	/// Remove the config stored under `name`.
	fn remove(catalog: &mut Catalog<E>, name: &str);

	/// The config's current [`Estimate`] fields. Defaults to none, disabling detection.
	fn estimate(&self) -> Estimate {
		Estimate::default()
	}

	/// Replace the config's [`Estimate`] fields. Defaults to discarding them.
	fn set_estimate(&mut self, _estimate: Estimate) {}
}

/// Caller-provided catalog fields for a video track: a starting point for what the importer detects.
///
/// Every field is optional and fills only a gap the stream leaves; a value the stream reveals (the
/// dimensions from an SPS, ...) always wins, so a detected change just updates the catalog. A
/// [`codec`](Self::codec) alone is enough to publish the catalog before the first keyframe (the
/// decoder config then arrives in band). Use it for fields the stream can't reveal (bitrate) or to
/// skip that wait.
///
/// Video importers take one because they build the [`VideoConfig`](hang::catalog::VideoConfig)
/// themselves, out of the bitstream, so the caller never holds the config to set these on. An audio
/// importer publishes a complete config from its init bytes, so audio has no equivalent.
#[derive(Clone, Default, Debug, PartialEq)]
#[non_exhaustive]
pub struct VideoHint {
	/// The video codec.
	pub codec: Option<hang::catalog::VideoCodec>,
	/// The encoded width in pixels.
	pub coded_width: Option<u32>,
	/// The encoded height in pixels.
	pub coded_height: Option<u32>,
	/// The display aspect ratio width.
	pub display_aspect_width: Option<u32>,
	/// The display aspect ratio height.
	pub display_aspect_height: Option<u32>,
	/// The maximum bitrate in bits per second.
	pub bitrate: Option<u64>,
	/// The frame rate in frames per second.
	pub framerate: Option<f64>,
	/// If true, the decoder optimizes for latency.
	pub optimize_for_latency: Option<bool>,
	/// The maximum jitter before the next frame is emitted.
	pub jitter: Option<Duration>,
}

/// Fill `slot` from `value` only when the slot is still empty, so a value the stream detected always
/// wins over the caller's hint.
fn fill<T>(slot: &mut Option<T>, value: Option<T>) {
	if slot.is_none() {
		*slot = value;
	}
}

impl VideoHint {
	/// Fill a detected video config's absent optional fields from these hints.
	///
	/// Only the gaps: a value the stream detects (e.g. the dimensions from an SPS) is left untouched,
	/// so a resolution change updates the catalog instead of conflicting with the hint. An importer
	/// calls this on every config it publishes, before handing it to [`Rendition::set`], so a hinted
	/// field counts as supplied and is never overwritten by detection.
	pub fn apply(&self, config: &mut hang::catalog::VideoConfig) {
		fill(&mut config.coded_width, self.coded_width);
		fill(&mut config.coded_height, self.coded_height);
		fill(&mut config.display_aspect_width, self.display_aspect_width);
		fill(&mut config.display_aspect_height, self.display_aspect_height);
		fill(&mut config.bitrate, self.bitrate);
		fill(&mut config.framerate, self.framerate);
		fill(&mut config.optimize_for_latency, self.optimize_for_latency);
		fill(&mut config.jitter, self.jitter);
	}

	/// Build a config from these fields alone, or `None` if the codec is missing. Used to publish
	/// the catalog before the stream is parsed.
	pub fn to_config(&self) -> Option<hang::catalog::VideoConfig> {
		let codec = self.codec.clone()?;
		let mut config = hang::catalog::VideoConfig::new(codec);
		config.container = hang::catalog::Container::Legacy;
		self.apply(&mut config);
		Some(config)
	}
}

impl<E: CatalogExt> RenditionConfig<E> for hang::catalog::VideoConfig {
	fn insert(self, catalog: &mut Catalog<E>, name: &str) {
		catalog.video.renditions.insert(name.to_string(), self);
	}
	fn get_mut<'a>(catalog: &'a mut Catalog<E>, name: &str) -> Option<&'a mut Self> {
		catalog.video.renditions.get_mut(name)
	}
	fn remove(catalog: &mut Catalog<E>, name: &str) {
		catalog.video.renditions.remove(name);
	}

	fn estimate(&self) -> Estimate {
		Estimate::default().with_jitter(self.jitter).with_bitrate(self.bitrate)
	}
	fn set_estimate(&mut self, estimate: Estimate) {
		self.jitter = estimate.jitter;
		self.bitrate = estimate.bitrate;
	}
}

impl<E: CatalogExt> RenditionConfig<E> for hang::catalog::AudioConfig {
	fn insert(self, catalog: &mut Catalog<E>, name: &str) {
		catalog.audio.renditions.insert(name.to_string(), self);
	}
	fn get_mut<'a>(catalog: &'a mut Catalog<E>, name: &str) -> Option<&'a mut Self> {
		catalog.audio.renditions.get_mut(name)
	}
	fn remove(catalog: &mut Catalog<E>, name: &str) {
		catalog.audio.renditions.remove(name);
	}

	fn estimate(&self) -> Estimate {
		Estimate::default().with_jitter(self.jitter).with_bitrate(self.bitrate)
	}
	fn set_estimate(&mut self, estimate: Estimate) {
		self.jitter = estimate.jitter;
		self.bitrate = estimate.bitrate;
	}
}

/// A clonable reservation context handed to importers so they declare their tracks up front.
///
/// Made via [`Producer::reserve`]. While any `Reserved` clone is alive the track set may still
/// grow, so the catalog is withheld from the broadcast. Each [`init`](Self::init) reserves a
/// rendition by name (config filled in later via the returned [`Rendition`] guard) and counts as
/// outstanding until that guard is fulfilled or dropped. Once every clone is dropped *and* every
/// reservation resolves, the first catalog snapshot is published atomically with the complete
/// track list, so a one-shot muxer (fMP4, MPEG-TS) never sees a half-converged catalog.
pub struct Reserved<E: CatalogExt = ()> {
	catalog: Producer<E>,
}

impl<E: CatalogExt> Reserved<E> {
	pub(super) fn new(catalog: Producer<E>) -> Self {
		catalog.add_reserver();
		Self { catalog }
	}

	/// Reserve a rendition of config type `C` under `name`, returning a guard to fill it in.
	///
	/// The guard holds its own `Reserved` clone, so the catalog stays withheld until the returned
	/// [`Rendition`] is [`set`](Rendition::set) (or dropped). Prefer [`video`](Self::video) /
	/// [`audio`](Self::audio) for the built-in media configs.
	pub fn init<C: RenditionConfig<E>>(&self, name: impl Into<String>) -> Rendition<E, C> {
		Rendition::new(self.clone(), name)
	}

	/// Reserve a video rendition; shorthand for [`init`](Self::init).
	pub fn video(&self, name: impl Into<String>) -> VideoTrack<E> {
		self.init(name)
	}

	/// Reserve an audio rendition; shorthand for [`init`](Self::init).
	pub fn audio(&self, name: impl Into<String>) -> AudioTrack<E> {
		self.init(name)
	}

	/// Resolve a timestamp on the broadcast's shared clock (see [`Producer::timestamp`]).
	pub fn timestamp(&self, hint: Option<moq_net::Timestamp>) -> crate::Result<moq_net::Timestamp> {
		self.catalog.timestamp(hint)
	}

	/// The underlying catalog [`Producer`], for edits that outlive this reservation.
	///
	/// A container importer holds one to edit the catalog directly (e.g. its per-frame reconcile, or
	/// track removals after its initial set is declared) while the reservation itself is dropped to
	/// open the gate. The returned handle does not gate: only live `Reserved`s do.
	pub fn producer(&self) -> Producer<E> {
		self.catalog.clone()
	}
}

impl<E: CatalogExt> Clone for Reserved<E> {
	fn clone(&self) -> Self {
		self.catalog.add_reserver();
		Self {
			catalog: self.catalog.clone(),
		}
	}
}

impl<E: CatalogExt> Drop for Reserved<E> {
	fn drop(&mut self) {
		self.catalog.release_reserver();
	}
}

/// A reserved rendition of config type `C`, retired from the catalog on drop.
///
/// Made via [`Reserved::init`] (or [`video`](Reserved::video) / [`audio`](Reserved::audio)). Fill
/// it in with [`set`](Self::set) and refine it in place with [`update`](Self::update). Until it's
/// set (or dropped) it holds a [`Reserved`] clone, so an unresolved rendition keeps the initial
/// catalog publish gated. On drop the rendition is removed from the shared catalog.
pub struct Rendition<E: CatalogExt, C: RenditionConfig<E>> {
	catalog: Producer<E>,
	name: String,
	/// The reservation this rendition holds until its config is set (or it's dropped unfulfilled).
	/// `Some` gates the initial publish; cleared by [`set`](Self::set).
	gate: Option<Reserved<E>>,
	/// Whether a config has been published, so a lazily-configured importer (e.g. H.264 before its
	/// SPS) holds the handle without a catalog entry, and drops without a spurious removal.
	present: bool,
	/// Detects jitter and bitrate from the frames fed in, keeping the config's fields current.
	metrics: Metrics,
	/// The fields the config carried at [`set`](Self::set): authoritative, never overwritten by
	/// detection. Whatever is absent here is what the detector fills.
	supplied: Estimate,
	_config: PhantomData<fn() -> C>,
}

/// A single video track's catalog rendition. See [`Rendition`].
pub type VideoTrack<E = ()> = Rendition<E, hang::catalog::VideoConfig>;
/// A single audio track's catalog rendition. See [`Rendition`].
pub type AudioTrack<E = ()> = Rendition<E, hang::catalog::AudioConfig>;

impl<E: CatalogExt, C: RenditionConfig<E>> Rendition<E, C> {
	fn new(reserved: Reserved<E>, name: impl Into<String>) -> Self {
		Self {
			catalog: reserved.catalog.clone(),
			gate: Some(reserved),
			name: name.into(),
			present: false,
			metrics: Metrics::new(),
			supplied: Estimate::default(),
			_config: PhantomData,
		}
	}

	/// The track name this rendition is keyed by.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// Resolve a timestamp on the broadcast's shared clock (see [`Producer::timestamp`]).
	pub fn timestamp(&self, hint: Option<moq_net::Timestamp>) -> crate::Result<moq_net::Timestamp> {
		self.catalog.timestamp(hint)
	}

	/// Insert or replace the rendition, fulfilling the reservation and publishing the catalog.
	///
	/// Whatever [`Estimate`] fields `config` already carries are authoritative and left alone; the
	/// rest are auto-detected, seeded with any metrics accumulated before the rendition existed (a
	/// dirty start or a B-frame reorder) and kept current as frames arrive. A caller who wants to
	/// pre-empt detection sets the field on the config, or (for a config an importer builds out of
	/// the bitstream) hands the importer a hint like [`VideoHint`].
	pub fn set(&mut self, mut config: C) {
		self.supplied = config.estimate();
		config.set_estimate(self.resolved());

		// Write the config first (still withheld, since we're holding our reservation), then release
		// the reservation. If this was the last one, the release flushes a complete snapshot.
		{
			let mut guard = self.catalog.lock();
			config.insert(&mut guard, &self.name);
		}
		self.present = true;
		self.gate = None;
	}

	/// The supplied fields, with anything absent filled from the detector.
	fn resolved(&self) -> Estimate {
		let mut estimate = self.supplied.clone();
		if estimate.jitter.is_none() {
			estimate.jitter = self.metrics.jitter();
		}
		if estimate.bitrate.is_none() {
			estimate.bitrate = self.metrics.bitrate();
		}
		estimate
	}

	/// Republish the estimate after the detector moved a field nothing supplied.
	fn refresh(&mut self) {
		let estimate = self.resolved();
		self.update(|config| config.set_estimate(estimate));
	}

	/// Refine the rendition in place (e.g. a synthesized description), publishing if present.
	pub fn update(&mut self, f: impl FnOnce(&mut C)) {
		if !self.present {
			return;
		}
		let mut guard = self.catalog.lock();
		if let Some(config) = C::get_mut(&mut guard, &self.name) {
			f(config);
		}
	}

	/// Record one frame (presentation timestamp + encoded size), auto-filling the jitter if the
	/// config didn't provide it and the detected value changed.
	pub fn record_frame(&mut self, ts: Timestamp, bytes: usize) {
		if self.metrics.record_frame(ts, bytes).is_some() && self.supplied.jitter.is_none() {
			self.refresh();
		}
	}

	/// Record a frame's reorder delay (`PTS - DTS`), auto-filling the jitter as for
	/// [`record_frame`](Self::record_frame).
	pub fn record_reorder(&mut self, reorder: Timestamp) {
		if self.metrics.record_reorder(reorder).is_some() && self.supplied.jitter.is_none() {
			self.refresh();
		}
	}

	/// Close the current group (`next` is its end timestamp when known), auto-filling the bitrate
	/// if the config didn't provide it and the detected maximum rose.
	pub fn record_group_end(&mut self, next: Option<Timestamp>) {
		if self.metrics.finish_group(next).is_some() && self.supplied.bitrate.is_none() {
			self.refresh();
		}
	}
}

impl<E: CatalogExt, C: RenditionConfig<E>> Drop for Rendition<E, C> {
	fn drop(&mut self) {
		if self.present {
			// Removing mutates the catalog, so the guard publishes it (immediately if live, else it
			// accumulates until the gate opens).
			let mut guard = self.catalog.lock();
			C::remove(&mut guard, &self.name);
		}
		// Our reservation (`gate`) drops here. If still held (never set), its release flushes any
		// staged change; if already released by `set`, this is a no-op.
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn video_track() -> (moq_net::broadcast::Producer, super::super::Producer, VideoTrack) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = super::super::Producer::new(&mut broadcast).unwrap();
		let reserved = catalog.reserve();
		let rendition = reserved.video("v");
		// Drop the standalone reservation so only the rendition's own gate remains, which `set`
		// clears; the broadcast handle is returned so the produced tracks outlive the catalog.
		drop(reserved);
		(broadcast, catalog, rendition)
	}

	fn config(bitrate: Option<u64>, jitter: Option<Duration>) -> hang::catalog::VideoConfig {
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.bitrate = bitrate;
		config.jitter = jitter;
		config
	}

	fn ts(micros: u64) -> Timestamp {
		Timestamp::from_micros(micros).unwrap()
	}

	/// Feed ~40ms 100 kB frames (one per group) over more than the bitrate window.
	fn feed<E: CatalogExt, C: RenditionConfig<E>>(rendition: &mut Rendition<E, C>) {
		for i in 0..60u64 {
			let t = ts(i * 40_000);
			rendition.record_group_end(Some(t));
			rendition.record_frame(t, 100_000);
		}
		rendition.record_group_end(None);
	}

	#[test]
	fn detects_absent_jitter_and_bitrate() {
		let (_broadcast, catalog, mut rendition) = video_track();
		rendition.set(config(None, None));
		feed(&mut rendition);

		let snapshot = catalog.snapshot();
		let config = snapshot.video.renditions.get("v").unwrap();
		assert!(config.jitter.is_some(), "absent jitter should be auto-detected");
		assert!(config.bitrate.is_some(), "absent bitrate should be auto-detected");
	}

	#[test]
	fn keeps_provided_jitter_and_bitrate() {
		let (_broadcast, catalog, mut rendition) = video_track();
		rendition.set(config(Some(123), Some(Duration::from_millis(50))));
		feed(&mut rendition);

		let snapshot = catalog.snapshot();
		let config = snapshot.video.renditions.get("v").unwrap();
		assert_eq!(config.bitrate, Some(123), "a provided bitrate must not be overwritten");
		assert_eq!(
			config.jitter,
			Some(Duration::from_millis(50)),
			"a provided jitter must not be overwritten"
		);
	}

	/// A hint is applied by the importer before `set`, so a hinted field is indistinguishable from
	/// one the stream revealed: supplied, and never overwritten by detection.
	#[test]
	fn hinted_fields_count_as_supplied() {
		let (_broadcast, catalog, mut rendition) = video_track();

		let hint = VideoHint {
			bitrate: Some(456),
			..Default::default()
		};
		let mut config = config(None, None);
		hint.apply(&mut config);

		rendition.set(config);
		feed(&mut rendition);

		let snapshot = catalog.snapshot();
		let config = snapshot.video.renditions.get("v").unwrap();
		assert_eq!(config.bitrate, Some(456), "a hinted bitrate must not be overwritten");
		assert!(config.jitter.is_some(), "the unhinted jitter should still be detected");
	}

	/// A detected value the stream later reveals wins: `set` recaptures what the new config carries.
	#[test]
	fn resetting_recaptures_supplied() {
		let (_broadcast, catalog, mut rendition) = video_track();
		rendition.set(config(None, None));
		feed(&mut rendition);
		assert!(catalog.snapshot().video.renditions.get("v").unwrap().bitrate.is_some());

		rendition.set(config(Some(789), None));
		feed(&mut rendition);

		let snapshot = catalog.snapshot();
		let config = snapshot.video.renditions.get("v").unwrap();
		assert_eq!(config.bitrate, Some(789), "the re-set bitrate is now authoritative");
	}

	/// Two renditions can advertise one shared timeline: the same handle yields the same section, so
	/// an aligned ladder (source + rung) indexes off a single `.timeline.z` track.
	#[test]
	fn renditions_share_a_timeline() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = super::super::Producer::new(&mut broadcast).unwrap();
		let reserved = catalog.reserve();

		let shared = catalog.timeline("video");
		let mut source = reserved.video("video0");
		let mut rung = reserved.video("video1");
		drop(reserved);

		for rendition in [&mut source, &mut rung] {
			let mut config = config(None, None);
			config.timeline = Some(shared.section());
			rendition.set(config);
		}

		let snapshot = catalog.snapshot();
		let source_tl = snapshot
			.video
			.renditions
			.get("video0")
			.unwrap()
			.timeline
			.as_ref()
			.unwrap();
		let rung_tl = snapshot
			.video
			.renditions
			.get("video1")
			.unwrap()
			.timeline
			.as_ref()
			.unwrap();
		assert_eq!(source_tl.track, "video.timeline.z");
		assert_eq!(
			rung_tl.track, source_tl.track,
			"both renditions index off one shared timeline track"
		);
	}

	mod custom {
		use std::collections::BTreeMap;

		use serde::{Deserialize, Serialize};

		use super::*;

		#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
		struct TelemetryExt {
			#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
			telemetry: BTreeMap<String, Telemetry>,
		}

		impl CatalogExt for TelemetryExt {}

		#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
		struct Telemetry {
			schema: String,
			#[serde(default, skip_serializing_if = "Option::is_none")]
			bitrate: Option<u64>,
			#[serde(default, skip_serializing_if = "Option::is_none")]
			timeline: Option<hang::catalog::Timeline>,
		}

		impl RenditionConfig<TelemetryExt> for Telemetry {
			fn insert(self, catalog: &mut Catalog<TelemetryExt>, name: &str) {
				catalog.telemetry.insert(name.to_string(), self);
			}
			fn get_mut<'a>(catalog: &'a mut Catalog<TelemetryExt>, name: &str) -> Option<&'a mut Self> {
				catalog.telemetry.get_mut(name)
			}
			fn remove(catalog: &mut Catalog<TelemetryExt>, name: &str) {
				catalog.telemetry.remove(name);
			}

			// Opts into bitrate detection only; jitter is left undetected.
			fn estimate(&self) -> Estimate {
				Estimate::default().with_bitrate(self.bitrate)
			}
			fn set_estimate(&mut self, estimate: Estimate) {
				self.bitrate = estimate.bitrate;
			}
		}

		fn telemetry(bitrate: Option<u64>) -> Telemetry {
			Telemetry {
				schema: "gps/v1".to_string(),
				bitrate,
				timeline: None,
			}
		}

		fn produce() -> (moq_net::broadcast::Producer, crate::catalog::Producer<TelemetryExt>) {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = crate::catalog::Producer::with_catalog(&mut broadcast, Catalog::default()).unwrap();
			(broadcast, catalog)
		}

		/// A custom kind gets the same detection and drop-removal as video/audio, and advertises a
		/// timeline the same explicit way an importer does.
		#[test]
		fn detects_and_advertises() {
			let (_broadcast, catalog) = produce();
			let reserved = catalog.reserve();
			let mut rendition = reserved.init::<Telemetry>("gps");
			drop(reserved);

			// The caller advertises the timeline explicitly, exactly as an importer does for video/audio.
			let mut config = telemetry(None);
			config.timeline = Some(catalog.timeline("gps").section());
			rendition.set(config);
			feed(&mut rendition);

			let snapshot = catalog.snapshot();
			let config = snapshot.telemetry.get("gps").unwrap();
			assert!(config.bitrate.is_some(), "absent bitrate should be auto-detected");
			assert_eq!(
				config.timeline.as_ref().map(|t| t.track.as_str()),
				Some("gps.timeline.z"),
				"the advertised timeline names the companion track"
			);

			drop(rendition);
			assert!(
				!catalog.snapshot().telemetry.contains_key("gps"),
				"the rendition should be removed on drop"
			);
		}

		/// A field the config supplies is authoritative even though the kind opts into detection.
		#[test]
		fn keeps_supplied_bitrate() {
			let (_broadcast, catalog) = produce();
			let reserved = catalog.reserve();
			let mut rendition = reserved.init::<Telemetry>("gps");
			drop(reserved);

			rendition.set(telemetry(Some(4_200)));
			feed(&mut rendition);

			let snapshot = catalog.snapshot();
			assert_eq!(snapshot.telemetry.get("gps").unwrap().bitrate, Some(4_200));
		}

		/// Custom and media renditions share one reservation gate, so the first snapshot carries both.
		#[test]
		fn gates_with_media_tracks() {
			let (_broadcast, catalog) = produce();
			let mut consumer = catalog.consume().unwrap();

			let reserved = catalog.reserve();
			let mut video = reserved.video("v");
			let mut gps = reserved.init::<Telemetry>("gps");
			drop(reserved);

			let waiter = kio::Waiter::noop();
			video.set(config(None, None));
			assert!(
				matches!(consumer.poll_next(&waiter), std::task::Poll::Pending),
				"the catalog stays withheld while the telemetry rendition is unresolved"
			);

			gps.set(telemetry(None));

			let mut latest = None;
			while let std::task::Poll::Ready(Ok(Some(catalog))) = consumer.poll_next(&waiter) {
				latest = Some(catalog);
			}
			let published = latest.expect("catalog published");
			assert!(published.video.renditions.contains_key("v"));
			assert!(published.telemetry.contains_key("gps"));
		}
	}
}
