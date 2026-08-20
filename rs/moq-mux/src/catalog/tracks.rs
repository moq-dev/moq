use std::marker::PhantomData;
use std::time::Duration;

use super::Estimate;
use super::Producer;
use super::hang::{Catalog, CatalogExt};

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
/// configs use the same trait. Writing to `catalog.video` / `catalog.audio` / `catalog.text` from a
/// custom config fights the media pipeline for those sections; stay in your own.
///
/// To index a custom track in the broadcast's timeline, enroll it via
/// [`catalog::Producer::enroll`](crate::catalog::Producer::enroll) (or build it through
/// [`media_producer`](crate::catalog::Producer::media_producer), which does so for you).
///
/// Opting into detection is only half of it: something has to measure. Write the track through a
/// [`container::Producer`](crate::container::Producer) and hand its
/// [`estimate`](crate::container::Producer::estimate) to [`Rendition::estimate`], or drive a
/// [`Estimator`](super::Estimator) yourself.
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
	/// Human-readable rendition name, plumbed through from [`Init::label`](crate::import::Init::label).
	///
	/// Not a hint: every other field here seeds something the stream may also reveal, while a label
	/// can only ever come from the caller. It rides along so one `apply` writes the whole config.
	pub(crate) label: Option<String>,
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

impl From<hang::catalog::VideoConfig> for VideoHint {
	/// Carry a whole rendition across as hints, for a caller that already knows what its encoder
	/// emits (moq-video probes its encoder for exactly this).
	///
	/// Total by construction: every field the hint can hold is taken from the config, so there is no
	/// per-field copy for a caller to forget. Fields with no hint slot (`broadcast`, `description`,
	/// `container`, `stalled`) are set through the catalog directly.
	fn from(config: hang::catalog::VideoConfig) -> Self {
		Self {
			label: config.label,
			codec: Some(config.codec),
			coded_width: config.coded_width,
			coded_height: config.coded_height,
			display_aspect_width: config.display_aspect_width,
			display_aspect_height: config.display_aspect_height,
			bitrate: config.bitrate,
			framerate: config.framerate,
			optimize_for_latency: config.optimize_for_latency,
			jitter: config.jitter,
		}
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
		fill(&mut config.label, self.label.clone());
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

impl<E: CatalogExt> RenditionConfig<E> for hang::catalog::TextConfig {
	fn insert(self, catalog: &mut Catalog<E>, name: &str) {
		catalog.text.renditions.insert(name.to_string(), self);
	}
	fn get_mut<'a>(catalog: &'a mut Catalog<E>, name: &str) -> Option<&'a mut Self> {
		catalog.text.renditions.get_mut(name)
	}
	fn remove(catalog: &mut Catalog<E>, name: &str) {
		catalog.text.renditions.remove(name);
	}

	// Only jitter: a cue track has no bitrate field, so there is nothing to fill from a measurement.
	fn estimate(&self) -> Estimate {
		Estimate::default().with_jitter(self.jitter)
	}
	fn set_estimate(&mut self, estimate: Estimate) {
		self.jitter = estimate.jitter;
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

	/// Track properties for a media track under this catalog, carrying any retention it declares.
	/// See [`Producer::track_info`](super::Producer::track_info).
	pub fn track_info(&self) -> moq_net::track::Info {
		self.catalog.track_info()
	}

	/// Reserve a rendition of config type `C` under `name`, returning the handle that owns it.
	///
	/// The handle holds its own `Reserved` clone, so the catalog stays withheld until the returned
	/// [`Rendition`] is [`set`](Rendition::set) (or dropped). Prefer [`video`](Self::video) /
	/// [`audio`](Self::audio) for the built-in media configs.
	///
	/// Errors if the name is already taken in this section, whether by a live rendition or by an
	/// entry in the catalog. Sections are independent, so the same name in `video` and `audio` is
	/// two unrelated renditions.
	pub fn init<C: RenditionConfig<E>>(&self, name: impl Into<String>) -> crate::Result<Rendition<E, C>> {
		Rendition::new(self.clone(), name.into())
	}

	/// Reserve a video rendition; shorthand for [`init`](Self::init).
	pub fn video(&self, name: impl Into<String>) -> crate::Result<VideoTrack<E>> {
		self.init(name)
	}

	/// Reserve an audio rendition; shorthand for [`init`](Self::init).
	pub fn audio(&self, name: impl Into<String>) -> crate::Result<AudioTrack<E>> {
		self.init(name)
	}

	/// Reserve a text (caption/subtitle) rendition; shorthand for [`init`](Self::init).
	///
	/// Nothing about a cue track is detected from its payload, so the caller [`set`](Rendition::set)s
	/// a complete config up front rather than waiting on a bitstream.
	pub fn text(&self, name: impl Into<String>) -> crate::Result<TextTrack<E>> {
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
///
/// It owns its name for its whole lifetime, from reservation rather than from `set`, so nothing
/// else can reserve that name in the same section meanwhile. That's what a lazily-configured
/// importer needs: an H.264 track publishes no config until its first SPS, and the name has to be
/// unavailable through that window or an entry written into it is overwritten by `set` and then
/// deleted by this `Drop`. Author a rendition by holding one of these, not by writing to
/// `catalog.video` / `catalog.audio` through a [`Guard`](super::Guard), which is raw access to the
/// catalog document and enforces nothing.
pub struct Rendition<E: CatalogExt, C: RenditionConfig<E>> {
	catalog: Producer<E>,
	name: String,
	/// The reservation this rendition holds until its config is set (or it's dropped unfulfilled).
	/// `Some` gates the initial publish; cleared by [`set`](Self::set).
	gate: Option<Reserved<E>>,
	/// Whether a config has been published, so a lazily-configured importer (e.g. H.264 before its
	/// SPS) holds the handle without a catalog entry, and drops without a spurious removal.
	present: bool,
	/// The fields the config carried at [`set`](Self::set): authoritative, never overwritten by
	/// detection. Whatever is absent here is what detection fills.
	supplied: Estimate,
	/// The last estimate handed to [`estimate`](Self::estimate), kept so a config set afterwards
	/// starts from whatever was already measured.
	detected: Estimate,
	/// The last estimate written to the config, so an unchanged one doesn't republish the catalog.
	published: Option<Estimate>,
	_config: PhantomData<fn() -> C>,
}

/// A single video track's catalog rendition. See [`Rendition`].
pub type VideoTrack<E = ()> = Rendition<E, hang::catalog::VideoConfig>;
/// A single audio track's catalog rendition. See [`Rendition`].
pub type AudioTrack<E = ()> = Rendition<E, hang::catalog::AudioConfig>;
/// A single text (caption/subtitle) track's catalog rendition. See [`Rendition`].
pub type TextTrack<E = ()> = Rendition<E, hang::catalog::TextConfig>;

impl<E: CatalogExt, C: RenditionConfig<E>> Rendition<E, C> {
	fn new(reserved: Reserved<E>, name: String) -> crate::Result<Self> {
		// Take the name now, not at `set`: a lazily-configured importer (H.264 before its first SPS)
		// has no catalog entry until much later, and the name has to be ours for that whole window.
		reserved.catalog.acquire::<C>(&name)?;

		Ok(Self {
			catalog: reserved.catalog.clone(),
			gate: Some(reserved),
			name,
			present: false,
			supplied: Estimate::default(),
			detected: Estimate::default(),
			published: None,
			_config: PhantomData,
		})
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
	/// rest are filled by [`estimate`](Self::estimate), seeded with anything already measured before
	/// the rendition existed (a dirty start or a B-frame reorder). A caller who wants to pre-empt
	/// detection sets the field on the config, or (for a config an importer builds out of the
	/// bitstream) hands the importer a hint like [`VideoHint`].
	pub fn set(&mut self, mut config: C) {
		self.supplied = config.estimate();
		let resolved = self.resolved();
		self.published = Some(resolved.clone());
		config.set_estimate(resolved);

		// Write the config first (still withheld, since we're holding our reservation), then release
		// the reservation. If this was the last one, the release flushes a complete snapshot.
		{
			let mut guard = self.catalog.lock();
			// Mark the slot filled before the write, not after. `insert` is caller code that can panic
			// partway, and whatever it managed to write still has to be retired when we drop.
			self.present = true;
			config.insert(&mut guard, &self.name);
		}
		self.gate = None;
	}

	/// The supplied fields, with anything absent filled from what was detected.
	fn resolved(&self) -> Estimate {
		let mut estimate = self.supplied.clone();
		if estimate.jitter.is_none() {
			estimate.jitter = self.detected.jitter;
		}
		if estimate.bitrate.is_none() {
			estimate.bitrate = self.detected.bitrate;
		}
		estimate
	}

	/// Publish a measured [`Estimate`], filling only the fields the config didn't supply at
	/// [`set`](Self::set).
	///
	/// Mint the estimate from an [`Estimator`](super::Estimator), usually the one a
	/// [`container::Producer`](crate::container::Producer) keeps for you:
	/// `rendition.estimate(track.estimate())`. Cheap to call after every write, since an estimate
	/// that resolves to what the catalog already carries doesn't republish it.
	///
	/// Calling this before [`set`](Self::set) is not wasted: the measurement is remembered and seeds
	/// the config once it lands.
	pub fn estimate(&mut self, estimate: Estimate) {
		self.detected = estimate;

		let resolved = self.resolved();
		if self.published.as_ref() == Some(&resolved) {
			return;
		}
		self.published = Some(resolved.clone());

		self.update(|config| config.set_estimate(resolved));
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
}

// Manual so a config that isn't `Debug` doesn't cost the handle its own, and because the interesting
// state is the name and whether it has published yet, not the estimate bookkeeping.
impl<E: CatalogExt, C: RenditionConfig<E>> std::fmt::Debug for Rendition<E, C> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Rendition")
			.field("name", &self.name)
			.field("published", &self.present)
			.finish_non_exhaustive()
	}
}

impl<E: CatalogExt, C: RenditionConfig<E>> Drop for Rendition<E, C> {
	fn drop(&mut self) {
		// The entry and the name it holds are released together under one lock. Removing mutates the
		// catalog, so the guard publishes it (immediately if live, else it accumulates until the gate
		// opens).
		self.catalog.lock().release::<C>(&self.name, self.present);
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
		let rendition = reserved.video("v").unwrap();
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

	fn ts(micros: u64) -> moq_net::Timestamp {
		moq_net::Timestamp::from_micros(micros).unwrap()
	}

	/// Feed ~40ms 100 kB frames (one per group) over more than the bitrate window, as a
	/// [`container::Producer`](crate::container::Producer) would.
	fn feed<E: CatalogExt, C: RenditionConfig<E>>(rendition: &mut Rendition<E, C>) {
		let mut estimator = super::super::Estimator::new();
		for i in 0..60u64 {
			let t = ts(i * 40_000);
			estimator.cut(Some(t));
			estimator.write(t, 100_000);
			rendition.estimate(estimator.estimate());
		}
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

	/// A caller hands moq-video a whole `VideoConfig`, so the conversion into hints must be total.
	/// Hand-copying it field by field is what dropped the label on that path.
	#[test]
	fn a_config_converts_into_hints_without_losing_the_label() {
		let (_broadcast, catalog, mut rendition) = video_track();

		let mut source = config(Some(456), None);
		source.label = Some("Main camera".to_string());
		source.coded_width = Some(1920);
		source.coded_height = Some(1080);
		let hint = VideoHint::from(source);

		// The importer applies the hint to each config it publishes, so check the label survives
		// both the up-front publish and a later one the stream resolved.
		let mut first = config(None, None);
		hint.apply(&mut first);
		rendition.set(first);
		feed(&mut rendition);
		assert_eq!(
			catalog.snapshot().video.renditions.get("v").unwrap().label.as_deref(),
			Some("Main camera"),
			"the label must survive the config the importer publishes up front"
		);

		let mut resolved = config(None, None);
		resolved.coded_width = Some(1280);
		resolved.coded_height = Some(720);
		hint.apply(&mut resolved);
		rendition.set(resolved);
		feed(&mut rendition);

		let snapshot = catalog.snapshot();
		let published = snapshot.video.renditions.get("v").unwrap();
		assert_eq!(
			published.label.as_deref(),
			Some("Main camera"),
			"the label must survive a config the stream resolved later"
		);
		assert_eq!(published.bitrate, Some(456), "the rest of the config converts too");
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

	/// Frames flow before the config resolves (H.264 publishes until its first SPS), so a
	/// measurement taken while the rendition is still empty has to seed the config once it lands.
	#[test]
	fn measurements_before_set_seed_the_config() {
		let (_broadcast, catalog, mut rendition) = video_track();
		feed(&mut rendition);
		rendition.set(config(None, None));

		let snapshot = catalog.snapshot();
		let config = snapshot.video.renditions.get("v").unwrap();
		assert!(
			config.jitter.is_some(),
			"the jitter measured before `set` should carry over"
		);
		assert!(
			config.bitrate.is_some(),
			"the bitrate measured before `set` should carry over"
		);
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

	/// A rendition owns its name from the moment it's reserved, not from `set`. H.264 publishes
	/// frames before its first SPS resolves the config, and the name has to be unavailable through
	/// that window: an entry written into the gap used to be overwritten by `set` and then deleted
	/// by the importer's `Drop`, taking the caller's rendition with it.
	#[test]
	fn an_unresolved_rendition_still_owns_its_name() {
		let (_broadcast, catalog, mut rendition) = video_track();
		assert!(
			catalog.snapshot().video.renditions.is_empty(),
			"nothing is published until the config resolves"
		);

		let err = catalog
			.reserve()
			.video("v")
			.expect_err("an unresolved rendition still owns its name");
		assert!(matches!(err, crate::Error::Hang(hang::Error::Duplicate(name)) if name == "v"));

		// The refusal left nothing behind, so the importer's own config is what lands.
		rendition.set(config(Some(123), None));
		assert_eq!(
			catalog.snapshot().video.renditions.get("v").unwrap().bitrate,
			Some(123),
			"the importer's config owns the rendition"
		);
	}

	/// A resolved rendition still owns its name, now backed by a catalog entry.
	#[test]
	fn a_resolved_rendition_still_owns_its_name() {
		let (_broadcast, catalog, mut rendition) = video_track();
		rendition.set(config(Some(789), None));

		assert!(catalog.reserve().video("v").is_err(), "owned by the live rendition");
		assert_eq!(
			catalog.snapshot().video.renditions.get("v").unwrap().bitrate,
			Some(789),
			"the refused reservation left the rendition intact"
		);
	}

	/// Ownership lasts exactly as long as the rendition, so the name is free again once it drops
	/// and the entry it published goes with it.
	#[test]
	fn dropping_a_rendition_frees_its_name() {
		let (_broadcast, catalog, mut rendition) = video_track();
		rendition.set(config(None, None));
		drop(rendition);
		assert!(
			catalog.snapshot().video.renditions.is_empty(),
			"the entry is retired with its owner"
		);

		let mut replacement = catalog
			.reserve()
			.video("v")
			.expect("the name is free once the rendition drops");
		replacement.set(config(Some(456), None));
		assert_eq!(catalog.snapshot().video.renditions.get("v").unwrap().bitrate, Some(456));
	}

	/// Sections are independent maps, so a video rendition says nothing about the same name in
	/// audio.
	#[test]
	fn a_name_is_owned_per_section() {
		let (_broadcast, catalog, _video) = video_track();
		catalog
			.reserve()
			.audio("v")
			.expect("an audio rendition is a different section from a video one");
	}

	/// An entry already in the catalog is taken too, even with no rendition behind it: a caller
	/// that hand-wrote one through the guard still gets to keep it.
	#[test]
	fn an_existing_entry_owns_its_name() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = super::super::Producer::new(&mut broadcast).unwrap();
		catalog.lock().video.insert("v", config(None, None)).unwrap();

		assert!(catalog.reserve().video("v").is_err(), "the catalog already carries it");
	}

	/// The broadcast has one timeline: every rendition's groups index into the same track, so
	/// an aligned ladder (source + rung) shares it by construction.
	#[test]
	fn renditions_share_the_broadcast_timeline() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = super::super::Producer::new(&mut broadcast).unwrap();

		let _recorder = catalog.enroll("video0").unwrap();
		let timeline = catalog.timeline();
		assert_eq!(timeline.section().track, hang::timeline::DEFAULT_NAME);
		assert_eq!(
			catalog.snapshot().timeline,
			Some(timeline.section()),
			"the one timeline is advertised at the catalog root"
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
			#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
			exploding: BTreeMap<String, Exploding>,
			#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
			stubborn: BTreeMap<String, Stubborn>,
		}

		impl CatalogExt for TelemetryExt {}

		#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
		struct Telemetry {
			schema: String,
			#[serde(default, skip_serializing_if = "Option::is_none")]
			bitrate: Option<u64>,
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
			}
		}

		fn produce() -> (moq_net::broadcast::Producer, crate::catalog::Producer<TelemetryExt>) {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = crate::catalog::Producer::with_catalog(&mut broadcast, Catalog::default()).unwrap();
			(broadcast, catalog)
		}

		/// A config whose `insert` panics, to poison the catalog lock the way any third-party
		/// [`RenditionConfig`] can: `set` runs it while holding that lock.
		#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
		struct Exploding {
			/// Write the entry before panicking, leaving a partially applied config behind.
			wrote: bool,
		}

		impl RenditionConfig<TelemetryExt> for Exploding {
			fn insert(self, catalog: &mut Catalog<TelemetryExt>, name: &str) {
				if self.wrote {
					catalog.exploding.insert(name.to_string(), self);
				}
				panic!("insert exploded");
			}
			fn get_mut<'a>(catalog: &'a mut Catalog<TelemetryExt>, name: &str) -> Option<&'a mut Self> {
				catalog.exploding.get_mut(name)
			}
			fn remove(catalog: &mut Catalog<TelemetryExt>, name: &str) {
				catalog.exploding.remove(name);
			}
		}

		/// A panicking `insert` poisons the catalog lock while `set` holds it. The producer has to
		/// survive that: an unwinding `Rendition::drop` takes the same lock to release its name, and
		/// a panic there would be a panic during a panic, which aborts the process rather than
		/// failing a test.
		#[test]
		fn a_poisoned_lock_does_not_take_the_producer_down() {
			let (_broadcast, catalog) = produce();
			let reserved = catalog.reserve();

			let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
				let mut boom = reserved.init::<Exploding>("gps").unwrap();
				boom.set(Exploding { wrote: false });
			}))
			.expect_err("the config's insert panics");
			assert!(
				err.downcast_ref::<&str>().is_some_and(|msg| *msg == "insert exploded"),
				"the panic is the one the config raised, not a lock failure"
			);

			// The rendition unwound, so it released its name and the catalog is still usable.
			assert!(catalog.snapshot().telemetry.is_empty());
			reserved
				.init::<Exploding>("gps")
				.expect("the unwound rendition released its name");
		}

		/// A config whose cleanup hook panics after mutating, the other half of untrusted caller code:
		/// `Drop` runs `remove` while holding the catalog lock.
		#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
		struct Stubborn;

		impl RenditionConfig<TelemetryExt> for Stubborn {
			fn insert(self, catalog: &mut Catalog<TelemetryExt>, name: &str) {
				catalog.stubborn.insert(name.to_string(), self);
			}
			fn get_mut<'a>(catalog: &'a mut Catalog<TelemetryExt>, name: &str) -> Option<&'a mut Self> {
				catalog.stubborn.get_mut(name)
			}
			fn remove(catalog: &mut Catalog<TelemetryExt>, name: &str) {
				catalog.stubborn.remove(name);
				panic!("remove exploded");
			}
		}

		/// A panicking `remove` must not cost the rendition its name. The name is released before any
		/// caller code runs, so the slot is reservable again even though the hook blew up on the way
		/// out.
		#[test]
		fn a_panicking_remove_still_frees_the_name() {
			let (_broadcast, catalog) = produce();
			let reserved = catalog.reserve();

			let mut rendition = reserved.init::<Stubborn>("gps").unwrap();
			rendition.set(Stubborn);

			std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(rendition)))
				.expect_err("the config's remove panics");

			reserved
				.init::<Stubborn>("gps")
				.expect("a name held by a blown-up cleanup hook would be lost forever");
		}

		/// The partial-application case: `insert` writes its entry and then panics, so `set` never
		/// reaches `present = true`. The entry still has to go when the rendition unwinds, or it sits
		/// in the catalog under a name no handle owns and every later reservation of it is refused.
		#[test]
		fn an_unwinding_rendition_retires_a_partially_written_entry() {
			let (_broadcast, catalog) = produce();
			let reserved = catalog.reserve();

			std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
				let mut boom = reserved.init::<Exploding>("gps").unwrap();
				boom.set(Exploding { wrote: true });
			}))
			.expect_err("the config's insert panics after writing");

			assert!(
				catalog.snapshot().exploding.is_empty(),
				"the entry the panicking insert wrote is retired with its owner"
			);
			reserved
				.init::<Exploding>("gps")
				.expect("a stranded entry would refuse this forever");
		}

		/// A custom kind gets the same detection and drop-removal as video/audio.
		#[test]
		fn detects_and_advertises() {
			let (_broadcast, catalog) = produce();
			let reserved = catalog.reserve();
			let mut rendition = reserved.init::<Telemetry>("gps").unwrap();
			drop(reserved);

			rendition.set(telemetry(None));
			feed(&mut rendition);

			let snapshot = catalog.snapshot();
			let config = snapshot.telemetry.get("gps").unwrap();
			assert!(config.bitrate.is_some(), "absent bitrate should be auto-detected");

			drop(rendition);
			assert!(
				!catalog.snapshot().telemetry.contains_key("gps"),
				"the rendition should be removed on drop"
			);
		}

		/// The whole point of opting in: write a custom track through a
		/// [`container::Producer`](crate::container::Producer) and its catalog bitrate/jitter keep
		/// themselves current, with no per-frame bookkeeping in the caller.
		#[test]
		fn measures_a_custom_track() {
			let (mut broadcast, mut catalog) = produce();
			let reserved = catalog.reserve();
			let mut rendition = reserved.init::<Telemetry>("gps").unwrap();
			drop(reserved);
			rendition.set(telemetry(None));

			let net = broadcast.create_track("gps", None).unwrap();
			let mut track = catalog
				.media_producer(net, crate::catalog::hang::Container::Legacy)
				.unwrap();

			// 40ms records of 5 kB: 1 Mbps, over more than the bitrate window.
			for i in 0..60u64 {
				track
					.write(crate::container::Frame {
						timestamp: ts(i * 40_000),
						duration: None,
						payload: bytes::Bytes::from(vec![0u8; 5_000]),
						keyframe: true,
					})
					.unwrap();
				rendition.estimate(track.estimate());
			}

			let snapshot = catalog.snapshot();
			let config = snapshot.telemetry.get("gps").unwrap();
			assert_eq!(config.bitrate, Some(1_000_000));
		}

		/// A field the config supplies is authoritative even though the kind opts into detection.
		#[test]
		fn keeps_supplied_bitrate() {
			let (_broadcast, catalog) = produce();
			let reserved = catalog.reserve();
			let mut rendition = reserved.init::<Telemetry>("gps").unwrap();
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
			let mut video = reserved.video("v").unwrap();
			let mut gps = reserved.init::<Telemetry>("gps").unwrap();
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
