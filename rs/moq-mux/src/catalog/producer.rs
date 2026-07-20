use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard};

use base64::Engine;

use super::hang::{Catalog, CatalogExt, Consumer, Extra};

/// Reservation bookkeeping shared across a producer's clones and its
/// [`Reserved`](super::Reserved) handles.
///
/// The initial catalog snapshot is withheld from the broadcast until `reservers == 0`. A live
/// `Reserved` counts as one reserver; a [`Rendition`](super::Rendition) holds its own `Reserved`
/// clone until it's fulfilled (or dropped), so an unresolved rendition keeps the gate shut too. See
/// [`Reserved`](super::Reserved).
#[derive(Default)]
struct Reservations {
	/// Live `Reserved` handles (including the one each unfulfilled `Rendition` holds).
	reservers: usize,
	/// A catalog change was made while buffering and is awaiting the initial publish. Without this
	/// we'd emit an empty snapshot when the gate opens on a catalog nobody ever touched.
	pending: bool,
	/// Whether the initial snapshot has been published (buffering is over).
	published: bool,
}

/// Produces both a hang and MSF catalog track for a broadcast.
///
/// Generic over the application extension `E` (defaulting to `()` for none). The catalog is a
/// [`Catalog<E>`](super::hang::Catalog): `video`/`audio` are direct fields (`catalog.video`) and the
/// extension is reachable directly via deref (`catalog.scte35`) or as `catalog.ext`. Define an
/// extension with [`CatalogExt`](super::hang::CatalogExt). The MSF track is always derived from the base
/// media sections, regardless of any extension.
///
/// The JSON catalog is updated when tracks are added/removed but is *not* automatically published.
/// You'll have to call [`lock`](Self::lock) to update and publish the catalog.
/// Both the hang (`catalog.json`) and MSF (`catalog`) tracks are published on drop of the guard.
///
/// The hang track is published through [`moq_json`], which currently emits one snapshot per
/// group (deltas disabled). This routes catalog publishing through the JSON merge-patch helper
/// so deltas can be enabled later without changing the wire format used today.
pub struct Producer<E: CatalogExt = ()> {
	hang: moq_json::snapshot::Producer<Catalog<E>>,
	hangz: moq_json::snapshot::Producer<Catalog<E>>,
	msf_track: moq_net::track::Producer,

	current: Arc<Mutex<Catalog<E>>>,

	/// Gates the initial catalog publish until all reservations resolve (see [`reserve`](Self::reserve)).
	reservations: Arc<Mutex<Reservations>>,

	/// Shared wall clock for the broadcast's tracks. Every importer on this catalog
	/// gets a clone (a `Copy` of the same epoch), so timestamps they synthesize when
	/// a caller has none land on one timeline and audio/video stay in sync.
	clock: crate::Clock,

	/// A clone of the broadcast, retained so per-rendition timeline tracks can be created
	/// lazily when a rendition is registered (the codec importers hold only their media
	/// track, not the broadcast).
	broadcast: moq_net::broadcast::Producer,

	/// The per-rendition timeline producers, memoized by media-track name so the catalog
	/// section and the media track's group recorder share one track. See [`media_producer`](Self::media_producer).
	timelines: Arc<Mutex<BTreeMap<String, crate::timeline::Producer>>>,
}

// Manual Clone so a producer is cheaply clonable regardless of whether `E` is.
impl<E: CatalogExt> Clone for Producer<E> {
	fn clone(&self) -> Self {
		Self {
			hang: self.hang.clone(),
			hangz: self.hangz.clone(),
			msf_track: self.msf_track.clone(),
			current: self.current.clone(),
			reservations: self.reservations.clone(),
			clock: self.clock,
			broadcast: self.broadcast.clone(),
			timelines: self.timelines.clone(),
		}
	}
}

impl Producer<()> {
	/// Create a new media-only catalog producer with the default (empty) catalog.
	///
	/// For an extended catalog, use [`with_catalog`](Self::with_catalog) with a
	/// `Catalog<E>` (e.g. the untyped [`Extra`] for the by-name / FFI path). Set
	/// application sections through [`lock`](Self::lock).
	pub fn new(broadcast: &mut moq_net::broadcast::Producer) -> Result<Self, moq_net::Error> {
		Self::with_catalog(broadcast, Catalog::default())
	}
}

impl<E: CatalogExt> Producer<E> {
	/// Create a new catalog producer with the given initial catalog.
	pub fn with_catalog(
		broadcast: &mut moq_net::broadcast::Producer,
		catalog: Catalog<E>,
	) -> Result<Self, moq_net::Error> {
		let hang_track = broadcast.create_track(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info())?;
		let hangz_track =
			broadcast.create_track(hang::Catalog::COMPRESSED_NAME, hang::Catalog::default_track_info())?;
		let msf_track = broadcast.create_track(moq_msf::DEFAULT_NAME, None)?;

		// Disable deltas for now to stay byte-compatible with consumers that only read snapshots.
		let mut json_config = moq_json::snapshot::ProducerConfig::default();
		json_config.delta_ratio = 0;
		let hang = moq_json::snapshot::Producer::new(hang_track, json_config.clone());

		// The `.z` track carries the same catalog, DEFLATE-compressed. Deltas stay off for parity
		// with the plaintext track; only the per-group compression differs.
		json_config.compression = true;
		let hangz = moq_json::snapshot::Producer::new(hangz_track, json_config);

		Ok(Self {
			hang,
			hangz,
			msf_track,
			current: Arc::new(Mutex::new(catalog)),
			reservations: Arc::new(Mutex::new(Reservations::default())),
			clock: crate::Clock::new(),
			broadcast: broadcast.clone(),
			timelines: Arc::new(Mutex::new(BTreeMap::new())),
		})
	}

	/// Resolve a timestamp, synthesizing one from the broadcast's shared
	/// [`Clock`](crate::Clock) when the caller has none.
	///
	/// Sharing the clock across the catalog's tracks keeps concurrently-produced
	/// audio and video on a single timeline.
	pub fn timestamp(&self, hint: Option<moq_net::Timestamp>) -> crate::Result<moq_net::Timestamp> {
		match hint {
			Some(pts) => Ok(pts),
			None => Ok(moq_net::Timestamp::from_micros(self.clock.micros())?),
		}
	}

	/// Get mutable access to the catalog, publishing it after any changes.
	pub fn lock(&mut self) -> Guard<'_, E> {
		Guard {
			catalog: self.current.lock().unwrap(),
			hang: &mut self.hang,
			hangz: &mut self.hangz,
			msf_track: &mut self.msf_track,
			reservations: &self.reservations,
			updated: false,
		}
	}

	/// Get a snapshot of the current catalog.
	pub fn snapshot(&self) -> Catalog<E> {
		self.current.lock().unwrap().clone()
	}

	/// Begin reserving the initial track set, returning a clonable [`Reserved`](super::Reserved).
	///
	/// Hand it (or clones) to importers; each reserves its rendition via
	/// [`Reserved::init`](super::Reserved::init). The catalog is withheld from the broadcast until
	/// every `Reserved` is dropped, counting both the ones importers hold and the one each unfulfilled
	/// [`Rendition`](super::Rendition) holds until its config resolves. So a one-shot muxer (fMP4,
	/// MPEG-TS) sees the complete track list in the first snapshot instead of a half-converged one.
	/// Producers that don't reserve publish incrementally as before.
	pub fn reserve(&self) -> super::Reserved<E> {
		super::Reserved::new(self.clone())
	}

	/// Register a live [`Reserved`](super::Reserved) handle.
	pub(super) fn add_reserver(&self) {
		self.reservations.lock().unwrap().reservers += 1;
	}

	/// Drop a [`Reserved`](super::Reserved) handle, flushing the initial snapshot if that was the
	/// last thing gating it.
	pub(super) fn release_reserver(&mut self) {
		{
			let mut r = self.reservations.lock().unwrap();
			r.reservers = r.reservers.saturating_sub(1);
		}
		self.flush_if_ready();
	}

	/// Publish the buffered snapshot once, if buffering has just finished with a change staged.
	pub(super) fn flush_if_ready(&mut self) {
		{
			let mut r = self.reservations.lock().unwrap();
			if r.published || r.reservers != 0 {
				return;
			}
			// Nothing was staged (e.g. every stream was ignored): stay unpublished so a later change
			// still triggers the first emit, rather than publishing an empty catalog now.
			if !r.pending {
				return;
			}
			r.pending = false;
			r.published = true;
		}
		let catalog = self.current.lock().unwrap().clone();
		emit(&mut self.hang, &mut self.hangz, &mut self.msf_track, &catalog);
	}

	/// Build the media [`container::Producer`](crate::container::Producer) for `track`, recording its
	/// group opens into the `<name>.timeline.z` timeline named after it.
	///
	/// This is the 1:1 default. To share a timeline across aligned renditions, build the producer
	/// yourself and wire the shared timeline's recorder:
	/// `container::Producer::new(track, container).with_recorder(catalog.timeline(shared).recorder())`,
	/// and advertise `catalog.timeline(shared).section()` on each of their configs.
	pub fn media_producer<C: crate::container::Container>(
		&self,
		track: moq_net::track::Producer,
		container: C,
	) -> crate::container::Producer<C> {
		let recorder = self.timeline(track.name()).recorder();
		crate::container::Producer::new(track, container).with_recorder(recorder)
	}

	/// The [`timeline::Producer`](crate::timeline::Producer) named `name`, creating its
	/// `<name>.timeline.z` track on first use and returning the same shared handle thereafter.
	///
	/// Advertise it on a rendition by setting `config.timeline = Some(timeline.section())`, and
	/// record group opens through its [`recorder`](crate::timeline::Producer::recorder). Two
	/// renditions naming the same timeline share it: an aligned transcode ladder records the source
	/// and has the rungs only advertise the same section.
	pub fn timeline(&self, name: &str) -> crate::timeline::Producer {
		let mut timelines = self.timelines.lock().unwrap();
		timelines
			.entry(name.to_string())
			.or_insert_with(|| {
				crate::timeline::Producer::new(&mut self.broadcast.clone(), name)
					.expect("failed to create timeline track")
			})
			.clone()
	}

	/// Create a consumer for this catalog, receiving updates as they're published.
	pub fn consume(&self) -> Result<Consumer<E>, moq_net::Error> {
		Ok(Consumer::new(self.hang.consume()))
	}

	/// Finish publishing to this catalog.
	pub fn finish(&mut self) -> crate::Result<()> {
		self.hang.finish()?;
		self.hangz.finish()?;
		self.msf_track.finish()?;
		for timeline in self.timelines.lock().unwrap().values_mut() {
			timeline.finish()?;
		}
		Ok(())
	}
}

/// RAII guard for modifying a catalog with automatic publishing on drop.
///
/// Obtained via [`Producer::lock`]. Derefs to the [`Catalog<E>`](super::hang::Catalog), so `video`/`audio`
/// and (through the catalog's own deref) the extension sections are editable directly.
///
/// On drop, the hang, compressed-hang, and MSF catalog tracks are updated if the catalog was mutated.
pub struct Guard<'a, E: CatalogExt = ()> {
	catalog: MutexGuard<'a, Catalog<E>>,
	hang: &'a mut moq_json::snapshot::Producer<Catalog<E>>,
	hangz: &'a mut moq_json::snapshot::Producer<Catalog<E>>,
	msf_track: &'a mut moq_net::track::Producer,
	reservations: &'a Mutex<Reservations>,
	updated: bool,
}

impl<E: CatalogExt> Deref for Guard<'_, E> {
	type Target = Catalog<E>;

	fn deref(&self) -> &Self::Target {
		&self.catalog
	}
}

impl<E: CatalogExt> DerefMut for Guard<'_, E> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		self.updated = true;
		&mut self.catalog
	}
}

impl Guard<'_, Extra> {
	/// Set (or replace) a top-level application catalog section, republished on drop.
	///
	/// Errors if `name` collides with a reserved media section (`video`/`audio`).
	pub fn set_section(&mut self, name: impl Into<String>, value: serde_json::Value) -> crate::Result<()> {
		self.catalog.ext.set(name, value)?;
		self.updated = true;
		Ok(())
	}

	/// Remove a top-level application catalog section, republished on drop if it existed.
	///
	/// Returns the section's previous value, or `None` if it was absent.
	pub fn remove_section(&mut self, name: &str) -> Option<serde_json::Value> {
		let removed = self.catalog.ext.remove(name);
		if removed.is_some() {
			self.updated = true;
		}
		removed
	}
}

impl<E: CatalogExt> Drop for Guard<'_, E> {
	fn drop(&mut self) {
		if !self.updated {
			return;
		}

		{
			let mut r = self.reservations.lock().unwrap();
			// Withhold every emit while still buffering the initial reserved set; the mutation stays
			// in `current` and `pending` marks it for the flush once the gate opens.
			if !r.published && r.reservers != 0 {
				r.pending = true;
				return;
			}
			r.pending = false;
			r.published = true;
		}

		emit(self.hang, self.hangz, self.msf_track, &self.catalog);
	}
}

/// Emit the catalog to all tracks: hang (`catalog.json`), its DEFLATE-compressed `.z` sibling, and
/// the MSF catalog (`catalog`) derived from the base media sections.
fn emit<E: CatalogExt>(
	hang: &mut moq_json::snapshot::Producer<Catalog<E>>,
	hangz: &mut moq_json::snapshot::Producer<Catalog<E>>,
	msf_track: &mut moq_net::track::Producer,
	catalog: &Catalog<E>,
) {
	// One snapshot per group while deltas are disabled; the `.z` track carries the identical catalog.
	let _ = hang.update(catalog);
	let _ = hangz.update(catalog);

	let msf = to_msf(&catalog.media());
	if let Ok(mut group) = msf_track.append_group() {
		let _ = group.write_frame(moq_net::Timestamp::now(), msf.to_json().expect("invalid MSF catalog"));
		let _ = group.finish();
	}
}

/// Determine the SAP starting type for a given video codec.
///
/// SAP type 1: closed GOP with no leading pictures (IDR at every SAP).
/// Used for VP8, which has no B-frames.
///
/// SAP type 2: closed GOP with possible leading pictures. Used for codecs
/// that can carry B-frames (H.264, AV1, VP9) since the encoder may emit
/// leading B-frames after an IDR.
///
/// Returns None for unknown codecs (and H.265, which we don't yet validate
/// the SAP behavior of) so the field is omitted from the catalog.
fn video_sap_type(codec: &hang::catalog::VideoCodec) -> Option<u8> {
	use hang::catalog::VideoCodec;
	match codec {
		VideoCodec::VP8 => Some(1),
		VideoCodec::H264(_) | VideoCodec::AV1(_) | VideoCodec::VP9(_) => Some(2),
		_ => None,
	}
}

/// Convert a hang catalog to an MSF catalog.
fn to_msf(catalog: &hang::Catalog) -> moq_msf::Catalog {
	let mut tracks = Vec::new();

	let has_multiple_video = catalog.video.renditions.len() > 1;
	for (name, config) in &catalog.video.renditions {
		let packaging = match &config.container {
			hang::catalog::Container::Cmaf { .. } => moq_msf::Packaging::Cmaf,
			_ => moq_msf::Packaging::Legacy,
		};

		let init_data = match &config.container {
			hang::catalog::Container::Cmaf { init, .. } => Some(base64::engine::general_purpose::STANDARD.encode(init)),
			_ => config
				.description
				.as_ref()
				.map(|d| base64::engine::general_purpose::STANDARD.encode(d.as_ref())),
		};

		let sap_type = video_sap_type(&config.codec);
		let mut track = moq_msf::Track::new(name.clone(), packaging);
		track.is_live = true;
		track.role = Some(moq_msf::Role::Video);
		track.codec = Some(config.codec.to_string());
		track.width = config.coded_width;
		track.height = config.coded_height;
		track.framerate = config.framerate;
		track.bitrate = config.bitrate;
		track.init_data = init_data;
		track.render_group = Some(1);
		track.alt_group = if has_multiple_video { Some(1) } else { None };
		track.max_grp_sap_starting_type = sap_type;
		track.max_obj_sap_starting_type = sap_type;
		track.jitter = config.jitter;
		tracks.push(track);
	}

	let has_multiple_audio = catalog.audio.renditions.len() > 1;
	for (name, config) in &catalog.audio.renditions {
		let packaging = match &config.container {
			hang::catalog::Container::Cmaf { .. } => moq_msf::Packaging::Cmaf,
			_ => moq_msf::Packaging::Legacy,
		};

		let init_data = match &config.container {
			hang::catalog::Container::Cmaf { init, .. } => Some(base64::engine::general_purpose::STANDARD.encode(init)),
			_ => config
				.description
				.as_ref()
				.map(|d| base64::engine::general_purpose::STANDARD.encode(d.as_ref())),
		};

		let mut track = moq_msf::Track::new(name.clone(), packaging);
		track.is_live = true;
		track.role = Some(moq_msf::Role::Audio);
		track.codec = Some(config.codec.to_string());
		track.samplerate = Some(config.sample_rate);
		track.channel_config = Some(config.channel_count.to_string());
		track.bitrate = config.bitrate;
		track.init_data = init_data;
		track.render_group = Some(1);
		track.alt_group = if has_multiple_audio { Some(1) } else { None };
		track.max_grp_sap_starting_type = Some(1);
		track.max_obj_sap_starting_type = Some(1);
		track.jitter = config.jitter;
		tracks.push(track);
	}

	moq_msf::Catalog::new(tracks)
}

#[cfg(test)]
mod test {
	use std::collections::BTreeMap;

	use std::task::Poll;

	use bytes::Bytes;
	use hang::catalog::{AudioCodec, AudioConfig, Container, H264, VideoConfig};

	use super::*;

	#[test]
	fn publishes_plain_and_compressed_tracks() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = Producer::new(&mut broadcast).unwrap();

		let mut plain = Consumer::new(catalog.hang.consume());
		let mut compressed = Consumer::compressed(catalog.hangz.consume());

		{
			let mut guard = catalog.lock();
			guard
				.audio
				.renditions
				.insert("audio0".to_string(), AudioConfig::new(AudioCodec::Opus, 48_000, 2));
		}
		let expected = catalog.snapshot();

		let waiter = kio::Waiter::noop();
		let got_plain = match plain.poll_next(&waiter) {
			Poll::Ready(Ok(Some(c))) => c,
			other => panic!("expected plain catalog, got {other:?}"),
		};
		let got_compressed = match compressed.poll_next(&waiter) {
			Poll::Ready(Ok(Some(c))) => c,
			other => panic!("expected compressed catalog, got {other:?}"),
		};

		assert_eq!(got_plain, expected);
		assert_eq!(got_compressed, expected);
	}

	fn h264_config() -> VideoConfig {
		let mut config = VideoConfig::new(H264 {
			profile: 0x64,
			constraints: 0x00,
			level: 0x1f,
			inline: true,
		});
		config.container = Container::Legacy;
		config
	}

	// A reserved audio rendition that resolves before a reserved video rendition must not publish an
	// audio-only catalog; the first snapshot a consumer sees carries the complete track set. This is
	// the moq-dev/moq#1979 convergence race.
	#[test]
	fn reservation_gates_until_all_renditions_resolve() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast).unwrap();
		let mut consumer: Consumer = Consumer::new(catalog.hang.consume());
		let waiter = kio::Waiter::noop();

		let reserved = catalog.reserve();
		let mut audio = reserved.audio("audio0");
		let mut video = reserved.video("video0");
		drop(reserved); // done reserving; both renditions still outstanding

		// Audio resolves first: withheld, because video is still outstanding.
		audio.set(AudioConfig::new(AudioCodec::Opus, 48_000, 2));
		assert!(
			matches!(consumer.poll_next(&waiter), Poll::Pending),
			"an audio-only catalog must not publish while video is unresolved"
		);

		// Video resolves: the complete catalog publishes now, in one snapshot.
		video.set(h264_config());
		let snapshot = match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(c))) => c,
			other => panic!("expected the complete catalog, got {other:?}"),
		};
		assert!(snapshot.audio.renditions.contains_key("audio0"));
		assert!(snapshot.video.renditions.contains_key("video0"));
		assert!(
			matches!(consumer.poll_next(&waiter), Poll::Pending),
			"only the one complete snapshot should be published"
		);
	}

	// Dropping a reservation without fulfilling it (a stream that never produced a config) still opens
	// the gate, publishing whatever did resolve.
	#[test]
	fn reservation_gate_opens_when_unresolved_reservation_is_dropped() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast).unwrap();
		let mut consumer: Consumer = Consumer::new(catalog.hang.consume());
		let waiter = kio::Waiter::noop();

		let reserved = catalog.reserve();
		let mut audio = reserved.audio("audio0");
		let video = reserved.video("video0");
		drop(reserved);

		audio.set(AudioConfig::new(AudioCodec::Opus, 48_000, 2));
		assert!(matches!(consumer.poll_next(&waiter), Poll::Pending));

		// The video stream never resolves and is cancelled; the audio-only catalog publishes.
		drop(video);
		let snapshot = match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(c))) => c,
			other => panic!("expected the audio catalog, got {other:?}"),
		};
		assert!(snapshot.audio.renditions.contains_key("audio0"));
		assert!(!snapshot.video.renditions.contains_key("video0"));
	}

	// A change staged while a deferred importer still holds a reservation (mimicking a TS stream whose
	// config resolves late, e.g. AAC) must not publish until that rendition resolves, so no incomplete
	// intermediate snapshot leaks out.
	#[test]
	fn staged_change_waits_for_a_held_reservation() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast).unwrap();
		let mut consumer: Consumer = Consumer::new(catalog.hang.consume());
		let waiter = kio::Waiter::noop();

		// A deferred importer grabs a reservation up front and holds it until it can resolve.
		let deferred = catalog.reserve();

		// Meanwhile an eager rendition resolves and stages a catalog change. The importer keeps its
		// rendition alive (dropping it would retire the track), so bind it.
		let early = catalog.reserve();
		let mut a0 = early.audio("audio0");
		a0.set(AudioConfig::new(AudioCodec::Opus, 48_000, 2));
		drop(early);
		assert!(
			matches!(consumer.poll_next(&waiter), Poll::Pending),
			"the staged rendition must wait for the deferred importer's held reservation"
		);

		// The deferred importer finally builds its rendition and resolves it.
		let mut late = deferred.audio("audio1");
		drop(deferred); // the importer releases its own hold; only the rendition's remains
		assert!(matches!(consumer.poll_next(&waiter), Poll::Pending));
		late.set(AudioConfig::new(AudioCodec::Opus, 48_000, 1));

		let snapshot = match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(c))) => c,
			other => panic!("expected one complete catalog, got {other:?}"),
		};
		assert!(snapshot.audio.renditions.contains_key("audio0"));
		assert!(snapshot.audio.renditions.contains_key("audio1"));
		assert!(
			matches!(consumer.poll_next(&waiter), Poll::Pending),
			"only the one complete snapshot should be published"
		);
	}

	#[test]
	fn convert_simple() {
		let mut video_config = VideoConfig::new(H264 {
			profile: 0x64,
			constraints: 0x00,
			level: 0x1f,
			inline: true,
		});
		video_config.coded_width = Some(1280);
		video_config.coded_height = Some(720);
		video_config.bitrate = Some(6_000_000);
		video_config.framerate = Some(30.0);
		video_config.container = Container::Legacy;

		let mut video_renditions = BTreeMap::new();
		video_renditions.insert("video0.avc3".to_string(), video_config);

		let mut audio_config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		audio_config.bitrate = Some(128_000);
		audio_config.container = Container::Legacy;

		let mut audio_renditions = BTreeMap::new();
		audio_renditions.insert("audio0".to_string(), audio_config);

		let mut catalog = hang::Catalog::default();
		catalog.video.renditions = video_renditions;
		catalog.audio.renditions = audio_renditions;

		let msf = to_msf(&catalog);

		assert_eq!(msf.tracks.len(), 2);

		let video = &msf.tracks[0];
		assert_eq!(video.name, "video0.avc3");
		assert_eq!(video.role, Some(moq_msf::Role::Video));
		assert_eq!(video.packaging, moq_msf::Packaging::Legacy);
		assert_eq!(video.codec, Some("avc3.64001f".to_string()));
		assert_eq!(video.width, Some(1280));
		assert_eq!(video.height, Some(720));
		assert_eq!(video.framerate, Some(30.0));
		assert_eq!(video.bitrate, Some(6_000_000));
		assert!(video.init_data.is_none());
		// H.264 may carry B-frames, so SAP starting type is 2 (leading pictures allowed).
		assert_eq!(video.max_grp_sap_starting_type, Some(2));
		assert_eq!(video.max_obj_sap_starting_type, Some(2));
		assert_eq!(video.jitter, None);

		let audio = &msf.tracks[1];
		assert_eq!(audio.name, "audio0");
		assert_eq!(audio.role, Some(moq_msf::Role::Audio));
		assert_eq!(audio.packaging, moq_msf::Packaging::Legacy);
		assert_eq!(audio.codec, Some("opus".to_string()));
		assert_eq!(audio.samplerate, Some(48_000));
		assert_eq!(audio.channel_config, Some("2".to_string()));
		assert_eq!(audio.bitrate, Some(128_000));
		assert_eq!(audio.max_grp_sap_starting_type, Some(1));
		assert_eq!(audio.max_obj_sap_starting_type, Some(1));
		assert_eq!(audio.jitter, None);
	}

	#[test]
	fn convert_with_description() {
		let mut video_config = VideoConfig::new(H264 {
			profile: 0x64,
			constraints: 0x00,
			level: 0x1f,
			inline: false,
		});
		video_config.description = Some(Bytes::from_static(&[0x01, 0x02, 0x03]));
		video_config.coded_width = Some(1920);
		video_config.coded_height = Some(1080);
		video_config.container = Container::Legacy;

		let mut video_renditions = BTreeMap::new();
		video_renditions.insert("video0.m4s".to_string(), video_config);

		let mut catalog = hang::Catalog::default();
		catalog.video.renditions = video_renditions;

		let msf = to_msf(&catalog);
		let video = &msf.tracks[0];
		assert_eq!(video.init_data, Some("AQID".to_string()));
	}

	#[test]
	fn convert_empty() {
		let catalog = hang::Catalog::default();
		let msf = to_msf(&catalog);
		assert!(msf.tracks.is_empty());
	}

	#[test]
	fn convert_cmaf_packaging() {
		let mut video_config = VideoConfig::new(H264 {
			profile: 0x64,
			constraints: 0x00,
			level: 0x28,
			inline: false,
		});
		video_config.coded_width = Some(1920);
		video_config.coded_height = Some(1080);
		video_config.container = Container::Cmaf {
			init: base64::engine::general_purpose::STANDARD
				.decode("AAAYZ2Z0eXA=")
				.unwrap()
				.into(),
		};

		let mut video_renditions = BTreeMap::new();
		video_renditions.insert("video0.m4s".to_string(), video_config);

		let mut catalog = hang::Catalog::default();
		catalog.video.renditions = video_renditions;

		let msf = to_msf(&catalog);
		let video = &msf.tracks[0];
		assert_eq!(video.packaging, moq_msf::Packaging::Cmaf);
		assert_eq!(video.init_data, Some("AAAYZ2Z0eXA=".to_string()));
	}

	#[test]
	fn convert_sap_h264_with_jitter() {
		let mut video_config = VideoConfig::new(H264 {
			profile: 0x64,
			constraints: 0x00,
			level: 0x1f,
			inline: true,
		});
		video_config.coded_width = Some(1280);
		video_config.coded_height = Some(720);
		video_config.framerate = Some(30.0);
		video_config.container = Container::Legacy;
		video_config.jitter = Some(std::time::Duration::from_millis(100));

		let mut video_renditions = BTreeMap::new();
		video_renditions.insert("video0".to_string(), video_config);

		let mut audio_config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		audio_config.container = Container::Legacy;
		audio_config.jitter = Some(std::time::Duration::from_millis(40));

		let mut audio_renditions = BTreeMap::new();
		audio_renditions.insert("audio0".to_string(), audio_config);

		let mut catalog = hang::Catalog::default();
		catalog.video.renditions = video_renditions;
		catalog.audio.renditions = audio_renditions;

		let msf = to_msf(&catalog);

		let video = &msf.tracks[0];
		assert_eq!(video.role, Some(moq_msf::Role::Video));
		// H.264 may carry B-frames, so SAP starting type is 2.
		assert_eq!(video.max_grp_sap_starting_type, Some(2));
		assert_eq!(video.max_obj_sap_starting_type, Some(2));
		assert_eq!(video.jitter, Some(std::time::Duration::from_millis(100)));

		let audio = &msf.tracks[1];
		assert_eq!(audio.role, Some(moq_msf::Role::Audio));
		assert_eq!(audio.max_grp_sap_starting_type, Some(1));
		assert_eq!(audio.max_obj_sap_starting_type, Some(1));
		assert_eq!(audio.jitter, Some(std::time::Duration::from_millis(40)));
	}

	#[test]
	fn convert_sap_h265() {
		use hang::catalog::H265;

		let mut video_config = VideoConfig::new(H265 {
			in_band: false,
			profile_space: 0,
			profile_idc: 1,
			profile_compatibility_flags: [0, 0, 0, 0],
			tier_flag: false,
			level_idc: 93,
			constraint_flags: [0, 0, 0, 0, 0, 0],
		});
		video_config.coded_width = Some(1920);
		video_config.coded_height = Some(1080);
		video_config.framerate = Some(60.0);
		video_config.container = Container::Legacy;

		let mut video_renditions = BTreeMap::new();
		video_renditions.insert("video0".to_string(), video_config);

		let mut catalog = hang::Catalog::default();
		catalog.video.renditions = video_renditions;

		let msf = to_msf(&catalog);
		let video = &msf.tracks[0];
		// H.265 SAP behavior isn't validated end-to-end yet, so we omit the
		// SAP fields rather than advertise something we haven't verified.
		assert_eq!(video.max_grp_sap_starting_type, None);
		assert_eq!(video.max_obj_sap_starting_type, None);
		assert_eq!(video.jitter, None);
	}

	#[test]
	fn convert_sap_unknown_codec() {
		use hang::catalog::VideoCodec;

		let mut video_config = VideoConfig::new(VideoCodec::Unknown("future-codec.01".to_string()));
		video_config.container = Container::Legacy;

		let mut video_renditions = BTreeMap::new();
		video_renditions.insert("video0".to_string(), video_config);

		let mut catalog = hang::Catalog::default();
		catalog.video.renditions = video_renditions;

		let msf = to_msf(&catalog);
		let video = &msf.tracks[0];
		assert_eq!(video.max_grp_sap_starting_type, None);
		assert_eq!(video.max_obj_sap_starting_type, None);
	}
}
