use std::marker::PhantomData;
use std::time::Duration;

use moq_net::Timestamp;

use super::Producer;
use super::hang::{Catalog, CatalogExt};
use crate::container::jitter::Metrics;

mod sealed {
	pub trait Sealed {}
}

/// The media kind of a reserved rendition, selecting its config type and catalog slot.
///
/// Implemented only by [`Video`] and [`Audio`]; used as the `K` in
/// [`Rendition`] and [`Reserved::init`].
pub trait Kind: sealed::Sealed + 'static {
	/// The catalog config type carried by this kind ([`VideoConfig`](hang::catalog::VideoConfig)
	/// or [`AudioConfig`](hang::catalog::AudioConfig)).
	type Config;
	/// The caller-provided overlay for this kind: [`VideoHint`] for video, `()` for audio (which takes
	/// a complete [`AudioConfig`](hang::catalog::AudioConfig) instead).
	type Hint: Clone + Default;

	#[doc(hidden)]
	fn insert<E: CatalogExt>(catalog: &mut Catalog<E>, name: &str, config: Self::Config);
	#[doc(hidden)]
	fn with_mut<E: CatalogExt>(catalog: &mut Catalog<E>, name: &str, f: impl FnOnce(&mut Self::Config));
	#[doc(hidden)]
	fn remove<E: CatalogExt>(catalog: &mut Catalog<E>, name: &str);

	/// The config's detected-jitter field, so [`Rendition`] can update it generically.
	#[doc(hidden)]
	fn jitter_mut(config: &mut Self::Config) -> &mut Option<Duration>;
	/// The config's bitrate field, so [`Rendition`] can update it generically.
	#[doc(hidden)]
	fn bitrate_mut(config: &mut Self::Config) -> &mut Option<u64>;

	/// The config's timeline field, so [`Rendition`] can advertise the timeline track generically.
	#[doc(hidden)]
	fn timeline_mut(config: &mut Self::Config) -> &mut Option<hang::catalog::Timeline>;

	/// Fill a detected config's absent optional fields from a caller-provided hint.
	#[doc(hidden)]
	fn apply_hint(config: &mut Self::Config, hint: &Self::Hint);
}

/// Video rendition marker for [`Rendition`] / [`Reserved::init`].
pub enum Video {}
/// Audio rendition marker for [`Rendition`] / [`Reserved::init`].
pub enum Audio {}

impl sealed::Sealed for Video {}
impl sealed::Sealed for Audio {}

/// Caller-provided catalog fields for a video track: a starting point for what the importer detects.
///
/// Every field is optional and fills only a gap the stream leaves; a value the stream reveals (the
/// dimensions from an SPS, ...) always wins, so a detected change just updates the catalog. A
/// [`codec`](Self::codec) alone is enough to publish the catalog before the first keyframe (the
/// decoder config then arrives in band). Use it for fields the stream can't reveal (bitrate) or to
/// skip that wait.
///
/// Audio has no equivalent: an audio importer can't resolve its config from frames, so it takes a
/// complete [`AudioConfig`](hang::catalog::AudioConfig) up front instead of a hint.
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
	/// so a resolution change updates the catalog instead of conflicting with the hint.
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

impl Kind for Video {
	type Config = hang::catalog::VideoConfig;
	type Hint = VideoHint;

	fn insert<E: CatalogExt>(catalog: &mut Catalog<E>, name: &str, config: Self::Config) {
		catalog.video.renditions.insert(name.to_string(), config);
	}
	fn with_mut<E: CatalogExt>(catalog: &mut Catalog<E>, name: &str, f: impl FnOnce(&mut Self::Config)) {
		if let Some(config) = catalog.video.renditions.get_mut(name) {
			f(config);
		}
	}
	fn remove<E: CatalogExt>(catalog: &mut Catalog<E>, name: &str) {
		catalog.video.renditions.remove(name);
	}

	fn jitter_mut(config: &mut Self::Config) -> &mut Option<Duration> {
		&mut config.jitter
	}
	fn bitrate_mut(config: &mut Self::Config) -> &mut Option<u64> {
		&mut config.bitrate
	}
	fn timeline_mut(config: &mut Self::Config) -> &mut Option<hang::catalog::Timeline> {
		&mut config.timeline
	}

	fn apply_hint(config: &mut Self::Config, hint: &Self::Hint) {
		hint.apply(config);
	}
}

impl Kind for Audio {
	type Config = hang::catalog::AudioConfig;
	// Audio has no hint: importers publish a complete `AudioConfig` (see the crate docs on why audio
	// is eager while video is lazy), so there's nothing to overlay.
	type Hint = ();

	fn insert<E: CatalogExt>(catalog: &mut Catalog<E>, name: &str, config: Self::Config) {
		catalog.audio.renditions.insert(name.to_string(), config);
	}
	fn with_mut<E: CatalogExt>(catalog: &mut Catalog<E>, name: &str, f: impl FnOnce(&mut Self::Config)) {
		if let Some(config) = catalog.audio.renditions.get_mut(name) {
			f(config);
		}
	}
	fn remove<E: CatalogExt>(catalog: &mut Catalog<E>, name: &str) {
		catalog.audio.renditions.remove(name);
	}

	fn jitter_mut(config: &mut Self::Config) -> &mut Option<Duration> {
		&mut config.jitter
	}
	fn bitrate_mut(config: &mut Self::Config) -> &mut Option<u64> {
		&mut config.bitrate
	}
	fn timeline_mut(config: &mut Self::Config) -> &mut Option<hang::catalog::Timeline> {
		&mut config.timeline
	}

	fn apply_hint(_config: &mut Self::Config, _hint: &Self::Hint) {}
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

	/// Reserve a rendition of kind `K` under `name`, returning a guard to fill it in.
	///
	/// The guard holds its own `Reserved` clone, so the catalog stays withheld until the returned
	/// [`Rendition`] is [`set`](Rendition::set) (or dropped). Prefer [`video`](Self::video) /
	/// [`audio`](Self::audio) at call sites.
	pub fn init<K: Kind>(&self, name: impl Into<String>) -> Rendition<E, K> {
		Rendition::new(self.clone(), name, K::Hint::default())
	}

	/// Reserve a video rendition seeded with caller-provided catalog fields.
	///
	/// The `hint` is carried on the returned [`Rendition`]: every [`set`](Rendition::set) fills the
	/// gaps it declares (a detected value wins). See [`VideoHint`].
	pub fn video_with_hint(&self, name: impl Into<String>, hint: VideoHint) -> Rendition<E, Video> {
		Rendition::new(self.clone(), name, hint)
	}

	/// Reserve a video rendition; shorthand for [`init::<Video>`](Self::init).
	pub fn video(&self, name: impl Into<String>) -> Rendition<E, Video> {
		self.init::<Video>(name)
	}

	/// Reserve an audio rendition; shorthand for [`init::<Audio>`](Self::init).
	pub fn audio(&self, name: impl Into<String>) -> Rendition<E, Audio> {
		self.init::<Audio>(name)
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

/// A reserved rendition of kind `K`, retired from the catalog on drop.
///
/// Made via [`Reserved::init`] (or [`video`](Reserved::video) / [`audio`](Reserved::audio)). Fill
/// it in with [`set`](Self::set) and refine it in place with [`update`](Self::update). Until it's
/// set (or dropped) it holds a [`Reserved`] clone, so an unresolved rendition keeps the initial
/// catalog publish gated. On drop the rendition is removed from the shared catalog.
pub struct Rendition<E: CatalogExt, K: Kind> {
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
	/// Auto-fill `jitter` from the detector only while the config hasn't provided it.
	detect_jitter: bool,
	/// Auto-fill `bitrate` from the detector only while the config hasn't provided it.
	detect_bitrate: bool,
	/// Caller-provided fields, validated against and overlaid onto every [`set`](Self::set).
	hint: K::Hint,
	_kind: PhantomData<fn() -> K>,
}

/// A single video track's catalog rendition. See [`Rendition`].
pub type VideoTrack<E = ()> = Rendition<E, Video>;
/// A single audio track's catalog rendition. See [`Rendition`].
pub type AudioTrack<E = ()> = Rendition<E, Audio>;

impl<E: CatalogExt, K: Kind> Rendition<E, K> {
	fn new(reserved: Reserved<E>, name: impl Into<String>, hint: K::Hint) -> Self {
		Self {
			catalog: reserved.catalog.clone(),
			gate: Some(reserved),
			name: name.into(),
			present: false,
			metrics: Metrics::new(),
			detect_jitter: true,
			detect_bitrate: true,
			hint,
			_kind: PhantomData,
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
	/// The rendition's hint (a [`VideoHint`] from [`Reserved::video_with_hint`], or nothing for audio)
	/// fills any gap `config` leaves first, so a value the stream detected wins over the hint. A field
	/// then present (`jitter` or `bitrate`,
	/// whether from the caller or the hint) is authoritative and left alone; only an absent field is
	/// auto-detected. Any metrics accumulated before the rendition existed (a dirty start or a B-frame
	/// reorder) are seeded into the fields being detected.
	///
	/// Also advertises the rendition's timeline track in the config, so a consumer can index its groups
	/// without downloading media (see [`Producer::timeline_section`](crate::catalog::Producer::timeline_section)).
	pub fn set(&mut self, mut config: K::Config) {
		K::apply_hint(&mut config, &self.hint);
		self.detect_jitter = K::jitter_mut(&mut config).is_none();
		self.detect_bitrate = K::bitrate_mut(&mut config).is_none();

		if self.detect_jitter
			&& let Some(jitter) = self.metrics.jitter()
		{
			*K::jitter_mut(&mut config) = Some(jitter);
		}
		if self.detect_bitrate
			&& let Some(bitrate) = self.metrics.bitrate()
		{
			*K::bitrate_mut(&mut config) = Some(bitrate);
		}

		*K::timeline_mut(&mut config) = Some(self.catalog.timeline_section(&self.name));

		// Write the config first (still withheld, since we're holding our reservation), then release
		// the reservation. If this was the last one, the release flushes a complete snapshot.
		{
			let mut guard = self.catalog.lock();
			K::insert(&mut guard, &self.name, config);
		}
		self.present = true;
		self.gate = None;
	}

	/// Refine the rendition in place (e.g. a synthesized description), publishing if present.
	pub fn update(&mut self, f: impl FnOnce(&mut K::Config)) {
		if !self.present {
			return;
		}
		let mut guard = self.catalog.lock();
		K::with_mut(&mut guard, &self.name, f);
	}

	/// Record one frame (presentation timestamp + encoded size), auto-filling the jitter if the
	/// config didn't provide it and the detected value changed.
	pub fn record_frame(&mut self, ts: Timestamp, bytes: usize) {
		if let Some(jitter) = self.metrics.record_frame(ts, bytes)
			&& self.detect_jitter
		{
			self.update(|config| *K::jitter_mut(config) = Some(jitter));
		}
	}

	/// Record a frame's reorder delay (`PTS - DTS`), auto-filling the jitter as for
	/// [`record_frame`](Self::record_frame).
	pub fn record_reorder(&mut self, reorder: Timestamp) {
		if let Some(jitter) = self.metrics.record_reorder(reorder)
			&& self.detect_jitter
		{
			self.update(|config| *K::jitter_mut(config) = Some(jitter));
		}
	}

	/// Close the current group (`next` is its end timestamp when known), auto-filling the bitrate
	/// if the config didn't provide it and the detected maximum rose.
	pub fn record_group_end(&mut self, next: Option<Timestamp>) {
		if let Some(bitrate) = self.metrics.finish_group(next)
			&& self.detect_bitrate
		{
			self.update(|config| raise_bitrate(K::bitrate_mut(config), bitrate));
		}
	}
}

/// Raise a catalog `bitrate` field, never lowering a previously detected value.
fn raise_bitrate(field: &mut Option<u64>, bitrate: u64) {
	if field.is_none_or(|current| bitrate > current) {
		*field = Some(bitrate);
	}
}

impl<E: CatalogExt, K: Kind> Drop for Rendition<E, K> {
	fn drop(&mut self) {
		if self.present {
			// Removing mutates the catalog, so the guard publishes it (immediately if live, else it
			// accumulates until the gate opens).
			let mut guard = self.catalog.lock();
			K::remove(&mut guard, &self.name);
		}
		// Our reservation (`gate`) drops here. If still held (never set), its release flushes any
		// staged change; if already released by `set`, this is a no-op.
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn video_track() -> (
		moq_net::broadcast::Producer,
		super::super::Producer,
		Rendition<(), Video>,
	) {
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
	fn feed(rendition: &mut Rendition<(), Video>) {
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
}
