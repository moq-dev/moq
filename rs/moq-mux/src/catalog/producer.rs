use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard};

use base64::Engine;
use hang::catalog::Reference;

use super::hang::{Catalog, CatalogExt, Consumer, Extra};

/// Everything a catalog producer shares with its clones, under one lock.
///
/// One lock rather than one per concern. A name check only means anything if the catalog can't
/// change under it: split those and two reservations of the same name both pass, which is the state
/// that let one rendition's entry be written over and deleted by the other. Keeping the publish
/// gate here too means a [`Guard`] never has to take a second lock while holding this one.
struct State<E: CatalogExt> {
	catalog: Catalog<E>,

	/// The names live renditions hold, keyed by section (the [`RenditionConfig`](super::RenditionConfig)
	/// type) and name. The section is part of the key because each writes a disjoint map: `video["v"]`
	/// and `audio["v"]` are different renditions and neither says anything about the other. The
	/// value is whether the rendition's config is listed, since a lazily-configured one holds its
	/// name long before it has an entry.
	owned: BTreeMap<(std::any::TypeId, String), bool>,

	/// Reports whether the extension lists a track of its own, registered by an importer that
	/// records tracks there (see [`Producer::count_ext_tracks`]).
	ext_tracks: Option<fn(&mut E) -> bool>,

	/// Gates the initial catalog publish until all reservations resolve (see
	/// [`reserve`](Producer::reserve)).
	reservations: Reservations,

	/// Why the catalog tracks are closed, so [`Producer::modify`] refuses further edits with the
	/// cause: [`Producer::finish`], [`Producer::abort`], or a failed drop-time publish.
	closed: Option<crate::Error>,

	/// The broadcast's shared clock, advertised at the catalog root. Every importer on this
	/// catalog reads it (the same timeline), so timestamps they synthesize when a caller has none
	/// land on one timeline and audio/video stay in sync.
	clock: crate::Clock,

	/// Whether the clock's mapping is fixed: by [`Config::with_clock`], by an importer's first
	/// frame placing it, by [`Producer::clock`] handing it out, or by the first publish putting it
	/// on the wire. Until then an importer's first frame may still place it; after, every
	/// importer offsets onto it instead.
	anchored: bool,

	/// Holds estimate-driven publishes to one per [`ESTIMATE_WINDOW`].
	estimates: Throttle,

	/// The last revision [`Producer::fresh_track`] handed out per base name, so a successor never
	/// reuses a name.
	revisions: std::collections::HashMap<String, u64>,

	/// What the MSF catalog declares.
	msf: Msf<E>,
}

impl<E: CatalogExt> State<E> {
	/// Whether the catalog names anything this broadcast can serve, which the initial publish
	/// waits for.
	///
	/// Root metadata and application sections can describe a stream before its media config is
	/// known, and must not make an empty broadcast discoverable. A disabled video or audio rendition
	/// promises no frames, so it doesn't count. A listed rendition covers application-defined
	/// sections the base fields can't see, and an importer that lists tracks in the extension
	/// reports them through [`ext_tracks`](Self::ext_tracks).
	fn lists_track(&mut self) -> bool {
		use std::any::TypeId;
		// Video and audio carry `enabled`, which only the base check reads.
		let base = [
			TypeId::of::<hang::catalog::VideoConfig>(),
			TypeId::of::<hang::catalog::AudioConfig>(),
		];
		let catalog = &mut self.catalog;
		catalog.video.enabled().next().is_some()
			|| catalog.audio.enabled().next().is_some()
			|| !catalog.text.renditions.is_empty()
			|| !catalog.json.tracks.is_empty()
			|| !catalog.binary.tracks.is_empty()
			|| catalog.m2ts.is_some()
			|| self.ext_tracks.is_some_and(|lists| lists(&mut catalog.ext))
			|| self.owned.iter().any(|((ty, _), &listed)| listed && !base.contains(ty))
	}
}

/// How often a rising `jitter` or `delay` estimate may republish the catalog. Each republish makes
/// every viewer update every media subscription, and an overloaded publisher's estimate rises a
/// millisecond at a time.
const ESTIMATE_WINDOW: std::time::Duration = std::time::Duration::from_secs(1);

/// The window an estimate-driven publish opens (see [`Guard::commit_estimate`]).
///
/// No timer: a held estimate goes out on the first observation after the window ends, so the
/// catalog needs no runtime. If media stops, so does the estimate it would carry.
#[derive(Default)]
struct Throttle {
	/// When the last publish carrying an estimate went out.
	last: Option<moq_clock::Instant>,
	/// An estimate is in the catalog but not yet on the wire.
	pending: bool,
}

impl Throttle {
	/// A publish is going out: any held estimate rides it and opens a new window.
	fn fold(&mut self) {
		if std::mem::take(&mut self.pending) {
			self.last = Some(moq_clock::now());
		}
	}
}

/// The MSF catalog's publish state.
///
/// MSF declares only the estimates a publisher hinted, never measured ones: a demand-driven
/// publisher measures nothing until a viewer subscribes, and an MSF viewer finds its tracks only
/// through this catalog. MSF-01 §5.3 fixes a declared track's attributes, so each track keeps the
/// estimates it was first declared with, and the MSF catalog changes only when the track list or a
/// track's identity does. The hang catalog keeps updating its estimates in place.
struct Msf<E: CatalogExt> {
	/// The hinted estimates of each listed rendition that measures its own, keyed like
	/// [`State::owned`]. MSF declares these in place of what the catalog carries.
	hints: HashMap<(std::any::TypeId, String), Hint<E>>,
	/// The estimates each MSF track was first declared with, by namespace and name.
	declared: HashMap<(Option<String>, String), Declared>,
	/// The last MSF catalog written, so an identical one isn't written again.
	last: Option<String>,
}

impl<E: CatalogExt> Default for Msf<E> {
	fn default() -> Self {
		Self {
			hints: HashMap::new(),
			declared: HashMap::new(),
			last: None,
		}
	}
}

/// A rendition's hinted estimates, and how to write them into its config.
struct Hint<E: CatalogExt> {
	estimate: super::Estimate,
	apply: fn(&mut Catalog<E>, &str, super::Estimate),
}

impl<E: CatalogExt> Msf<E> {
	/// The MSF catalog to write for `catalog`, or `None` when it matches the last one written.
	fn render(&mut self, catalog: &Catalog<E>, base: &moq_net::Path) -> crate::Result<Option<String>> {
		let mut msf = if self.hints.is_empty() {
			to_msf(catalog, base)
		} else {
			let mut hinted = catalog.clone();
			for ((_, name), hint) in &self.hints {
				(hint.apply)(&mut hinted, name, hint.estimate.clone());
			}
			to_msf(&hinted, base)
		};
		// Rebuilt from the listed tracks, so a removed track's declaration goes with it.
		let mut declared = HashMap::with_capacity(msf.tracks.len());
		for track in &mut msf.tracks {
			let key = (track.namespace.clone(), track.name.clone());
			let first = self.declared.remove(&key).unwrap_or_else(|| Declared::of(track));
			first.apply(track);
			declared.insert(key, first);
		}
		self.declared = declared;

		// The MSF catalog is derived from our own types, so a serialize failure means an extension
		// broke the shape; report it like any other JSON failure rather than panicking.
		let json = msf.to_json().map_err(moq_json::Error::from)?;
		if self.last.as_ref() == Some(&json) {
			return Ok(None);
		}
		self.last = Some(json.clone());
		Ok(Some(json))
	}
}

/// Write `estimate` over the estimates of the `C` config under `name`.
fn apply_hint<E: CatalogExt, C: super::RenditionConfig<E>>(
	catalog: &mut Catalog<E>,
	name: &str,
	estimate: super::Estimate,
) {
	if let Some(config) = C::get_mut(catalog, name) {
		config.set_estimate(estimate);
	}
}

/// The estimated fields an MSF track was first declared with.
struct Declared {
	bitrate: Option<u64>,
	framerate: Option<f64>,
	jitter: Option<std::time::Duration>,
	delay: Option<std::time::Duration>,
}

impl Declared {
	fn of(track: &moq_msf::Track) -> Self {
		Self {
			bitrate: track.bitrate,
			framerate: track.framerate,
			jitter: track.jitter,
			delay: track.delay,
		}
	}

	fn apply(&self, track: &mut moq_msf::Track) {
		track.bitrate = self.bitrate;
		track.framerate = self.framerate;
		track.jitter = self.jitter;
		track.delay = self.delay;
	}
}

/// The producer's `clock` under a root `clock` of `advertised`: re-based on a changed mapping while
/// nothing has `anchored` the clock, and refused once something has. Clearing it is always refused.
fn adopt(clock: crate::Clock, anchored: bool, advertised: Option<hang::catalog::Clock>) -> crate::Result<crate::Clock> {
	if advertised == clock.wall() {
		return Ok(clock);
	}
	let refuse = |why: &str| crate::Error::UnmappableTimestamp(format!("root clock {advertised:?} {why}"));
	let wall = crate::Clock::exact(advertised.ok_or_else(|| refuse("clears the producer's clock"))?)?;
	// The same mapping in other units needs no host time, which may be unset or behind PTS zero.
	if Some(wall) == clock.wall() {
		return Ok(clock);
	}
	if anchored {
		return Err(refuse("changes the producer's clock after it was fixed"));
	}
	crate::Clock::adopted(wall)
}

/// Take the shared state, ignoring a poisoned lock.
///
/// A panic under this lock comes from code we hand the catalog to: a [`RenditionConfig::insert`]
/// implementation, or serializing an extension during a publish. Neither leaves the state
/// inconsistent (at worst the catalog is missing an entry, while `owned` and `reservations` still
/// describe exactly what is live), so poisoning protects nothing here. It would only turn the next
/// [`Rendition`](super::Rendition) drop into a panic during unwinding, which aborts the process.
fn take<E: CatalogExt>(state: &Mutex<State<E>>) -> MutexGuard<'_, State<E>> {
	state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The key a rendition name is held under: its section (the config type) plus the name.
fn owner_key<C: 'static>(name: &str) -> (std::any::TypeId, String) {
	(std::any::TypeId::of::<C>(), name.to_string())
}

/// The initial-publish gate, tracking the [`Reserved`](super::Reserved) handles still outstanding.
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
	/// Whether anything ever reserved, making this an importer's catalog: its initial snapshot also
	/// waits for a [listed track](State::lists_track), so input that declares none never announces.
	/// An incremental producer that never reserves publishes as it goes.
	reserved: bool,
}

/// The catalog tracks and the HLS record tracks advertised in them.
struct Outputs<E: CatalogExt> {
	hang: moq_json::snapshot::Producer<Catalog<E>>,
	hang_track: moq_net::track::Producer,
	msf_track: moq_net::track::Producer,
	/// The broadcast's HLS record tracks, unless [`Config::with_hls`] disabled them.
	hls: Option<crate::hls::Tracks>,
	/// Keeps the broadcast open while the catalog is, so a publisher may hand its own handle to a
	/// media producer and drop it.
	broadcast: moq_net::broadcast::Producer,
}

impl<E: CatalogExt> Clone for Outputs<E> {
	fn clone(&self) -> Self {
		Self {
			hang: self.hang.clone(),
			hang_track: self.hang_track.clone(),
			msf_track: self.msf_track.clone(),
			hls: self.hls.clone(),
			broadcast: self.broadcast.clone(),
		}
	}
}

impl<E: CatalogExt> Outputs<E> {
	/// The catalog as published: each local rendition with HLS records gets its `hls` field.
	///
	/// Filled in here rather than stored, so an owner that rewrites its rendition's config can't
	/// drop the field, and a track whose duration is not yet declared simply lacks it.
	fn advertise<'a>(&self, catalog: &'a Catalog<E>) -> std::borrow::Cow<'a, Catalog<E>> {
		let Some(hls) = &self.hls else {
			return std::borrow::Cow::Borrowed(catalog);
		};
		let mut catalog = catalog.clone();
		let video = catalog
			.video
			.renditions
			.iter_mut()
			.map(|(id, config)| (id, &mut config.hls));
		let audio = catalog
			.audio
			.renditions
			.iter_mut()
			.map(|(id, config)| (id, &mut config.hls));
		// An enrolled rendition is keyed by its track's name, which its reservation holds, so only
		// that entry gets the records; any other entry, such as a reference to a same-named track of
		// another broadcast, keeps its own field.
		for (id, field) in video.chain(audio) {
			if let Some(advertised) = hls.hls(id) {
				*field = Some(advertised);
			}
		}
		std::borrow::Cow::Owned(catalog)
	}

	/// Emit the catalog to hang and MSF. Neither writes a catalog identical to its last.
	fn emit(&mut self, catalog: &Catalog<E>, msf: &mut Msf<E>) -> crate::Result<()> {
		let catalog = self.advertise(catalog);
		let catalog = catalog.as_ref();
		// Every consumer refuses a catalog that fails this, so refuse it before it reaches one.
		catalog.check_renditions()?;
		// One snapshot per group while deltas are disabled.
		self.hang
			.update(moq_net::Timed::from(catalog).at(moq_net::Timestamp::now()))?;

		// `abort` below is what keeps the tracks consistent if this fails after the hang track was
		// updated.
		let Some(msf) = msf.render(catalog, &self.msf_track.broadcast().path)? else {
			return Ok(());
		};
		let mut group = self.msf_track.append_group()?;
		group.write_frame(moq_net::Timestamp::now(), msf)?;
		group.finish()?;

		Ok(())
	}

	/// Abort every catalog track after a publish nobody could return the error for.
	///
	/// The two tracks carry one catalog, so a failure part way through `emit` must not leave some
	/// of them serving a newer catalog than the others. Consumers see the error instead.
	fn abort(&mut self, err: &crate::Error) {
		let reason = err.reason();
		// Idempotent: a track that is already closed keeps its first reason.
		let _ = self.hang.clone().abort(reason.clone());
		let _ = self.msf_track.clone().abort(reason);
	}
}

/// Produces both a hang and MSF catalog track for a broadcast.
///
/// Generic over the application extension `E` (defaulting to `()` for none). The catalog is a
/// [`Catalog<E>`](super::hang::Catalog): `video`/`audio` are direct fields (`catalog.video`) and the
/// extension is reachable through `catalog.ext` (for example, `catalog.ext.scte35`). Define an
/// extension with [`CatalogExt`](super::hang::CatalogExt). The MSF track carries the same catalog:
/// the media sections become MSF tracks and the extension's sections ride the MSF root.
///
/// The JSON catalog is updated when tracks are added/removed but is *not* automatically published.
/// You'll have to call [`modify`](Self::modify) to update and publish the catalog.
/// Both the hang (`catalog.json`) and MSF (`catalog`) tracks are published on drop of the guard.
///
/// The hang track is published through [`moq_json`], which currently emits one snapshot per
/// group (deltas disabled). This routes catalog publishing through the JSON merge-patch helper
/// so deltas can be enabled later without changing the wire format used today.
pub struct Producer<E: CatalogExt = ()> {
	outputs: Outputs<E>,

	current: Arc<Mutex<State<E>>>,

	/// How each media track's HLS segments are cut, unless disabled. See [`Config::with_hls`].
	hls: Option<crate::hls::Config>,
	/// Retention override for the media tracks minted under this catalog, or `None` to keep
	/// hang's default. Fixed at construction, so every clone and every
	/// [`Reserved`](super::Reserved) mints tracks under one policy. See
	/// [`Config::with_max_age`].
	max_age: Option<std::time::Duration>,
	/// Connection allocator passthrough tracks claim their peak-hold bitrate on.
	/// See [`Config::with_bandwidth`].
	bandwidth: moq_net::bandwidth::Allocator,
	/// The write order across this catalog's renditions, which each rendition's advertised
	/// `delay` is measured against.
	order: super::estimate::Order,
	/// What each media track minted under this catalog has written. See [`stats`](Self::stats).
	sent: Arc<crate::stats::Registry<crate::stats::Sent>>,
}

// Manual Clone so a producer is cheaply clonable regardless of whether `E` is.
impl<E: CatalogExt> Clone for Producer<E> {
	fn clone(&self) -> Self {
		Self {
			outputs: self.outputs.clone(),
			current: self.current.clone(),
			hls: self.hls.clone(),
			max_age: self.max_age,
			bandwidth: self.bandwidth.clone(),
			order: self.order.clone(),
			sent: self.sent.clone(),
		}
	}
}

/// Everything a catalog [`Producer`] is built from: the initial catalog and any overrides.
///
/// Start from [`default`](Default::default), which is the media-only catalog with hang's own
/// defaults, and chain the setters.
///
/// These are constructor arguments rather than setters on a live producer, so a clone or a
/// [`Reserved`](super::Reserved) taken at any point mints tracks under the same policy.
#[derive(Clone)]
pub struct Config<E: CatalogExt = ()> {
	catalog: Catalog<E>,
	max_age: Option<std::time::Duration>,
	bandwidth: moq_net::bandwidth::Allocator,
	clock: Option<crate::Clock>,
	hls: Option<crate::hls::Config>,
}

impl Default for Config<()> {
	fn default() -> Self {
		Self {
			catalog: Catalog::default(),
			max_age: None,
			bandwidth: moq_net::bandwidth::Allocator::unlimited(),
			clock: None,
			hls: Some(crate::hls::Config::default()),
		}
	}
}

impl<E: CatalogExt> Config<E> {
	/// Start from an extended catalog rather than the media-only one, e.g. the untyped
	/// [`Extra`] for the by-name / FFI path. Set application sections through
	/// [`Producer::modify`](Producer::modify) afterwards. A root `clock` in it is adopted, not
	/// replaced (see [`Producer::new`]).
	pub fn with_catalog<F: CatalogExt>(self, catalog: Catalog<F>) -> Config<F> {
		Config {
			catalog,
			max_age: self.max_age,
			bandwidth: self.bandwidth,
			clock: self.clock,
			hls: self.hls,
		}
	}

	/// Publish with this broadcast clock instead of a fresh one.
	///
	/// The clock's wall mapping is advertised at the catalog root and fixed from the start, so
	/// every container importer (fMP4, MPEG-TS, FLV, MKV) shifts its stream's timestamps onto it,
	/// its first frame landing at the clock's reading on arrival. Without this, the catalog starts
	/// a fresh clock, and the first importer to receive a frame before anything fixes the clock
	/// places it there instead, keeping its own timestamps verbatim (see [`Timebase`](super::Timebase)).
	///
	/// The root `clock` is this clock: a derivative that only learns its input's mapping later
	/// writes it through [`Producer::modify`] instead, before anything fixes the clock.
	pub fn with_clock(mut self, clock: crate::Clock) -> Self {
		self.clock = Some(clock);
		self
	}

	/// Override how long the media tracks minted under this catalog keep a non-latest group
	/// fetchable, replacing hang's default. `None` keeps it.
	///
	/// That default suits a segmented egress (HLS/DASH), which may only advertise segments a
	/// FETCH can still reach. Lower it when nothing reads history and the memory matters;
	/// raise it for a deeper seek window. It is a RETENTION budget, not a delivery one, so it
	/// never makes a subscriber play further behind live -- it caps what one may ask to wait
	/// for, and a subscriber's own budget still defaults to zero.
	///
	/// Applies to the media tracks the catalog mints. The catalog and HLS record tracks keep
	/// moq-net's default: both are read at the live edge, which is retained unconditionally.
	pub fn with_max_age(mut self, max_age: impl Into<Option<std::time::Duration>>) -> Self {
		self.max_age = max_age.into();
		self
	}

	/// Claim each media track's peak-hold catalog bitrate on `bandwidth`.
	///
	/// A passthrough import has no configured ceiling, so it reserves the
	/// measured maximum instead: taken on the first 1 s window, raised when a
	/// later window exceeds it, never walked back. A co-resident encoder then
	/// targets what is left of the uplink. `unlimited` (the default) claims
	/// nothing a sender can follow.
	pub fn with_bandwidth(mut self, bandwidth: moq_net::bandwidth::Allocator) -> Self {
		self.bandwidth = bandwidth;
		self
	}

	/// Publish HLS segment records for each video and audio rendition, cut by `hls`; `None` publishes
	/// none. On by default, cut by [`hls::Config::default`](crate::hls::Config::default).
	pub fn with_hls(mut self, hls: impl Into<Option<crate::hls::Config>>) -> Self {
		self.hls = hls.into();
		self
	}
}

impl<E: CatalogExt> Producer<E> {
	/// Create a new catalog producer from a full [`Config`].
	///
	/// A root `clock` seeded through [`Config::with_catalog`] is adopted under the same rules as
	/// [`modify`](Self::modify): the producer runs on it, unless [`Config::with_clock`] fixed a
	/// different mapping, which is refused.
	pub fn new(broadcast: &mut moq_net::broadcast::Producer, config: Config<E>) -> crate::Result<Self> {
		// The broadcast clock is advertised at the catalog root from the first snapshot,
		// whether or not HLS records are enabled. A host with an unset wall clock advertises none.
		// Resolved before any track exists, so a refused seed leaves the broadcast untouched.
		let anchored = config.clock.is_some();
		let mut catalog = config.catalog;
		let clock = match catalog.clock {
			// An unseeded catalog runs on the configured clock, or a fresh one.
			None => config.clock.unwrap_or_default(),
			seeded => adopt(config.clock.unwrap_or_default(), anchored, seeded)?,
		};
		catalog.clock = clock.wall();

		// Compressed, as `default_track_info` sets: catalog JSON is small and repetitive.
		let hang_track = broadcast.create_track(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info())?;
		// The MSF track is the same catalog in another encoding, so it takes the same
		// priority. Leaving it at the default would rank a subscriber reading MSF
		// below every media track on a relay's upstream leg. It stays uncompressed: MSF is
		// read by other implementations, which hang's compression decisions don't cover.
		let msf_info = hang::Catalog::default_track_info().with_compress(false);
		let msf_track = broadcast.create_track(moq_msf::DEFAULT_NAME, msf_info)?;

		// Disable deltas for now to stay byte-compatible with consumers that only read snapshots.
		let json_config = moq_json::snapshot::Config::default().with_deltas(false);
		let hang = moq_json::snapshot::Producer::new(hang_track.clone(), json_config);

		let hls = config.hls.as_ref().map(|_| crate::hls::Tracks::new(broadcast));

		Ok(Self {
			outputs: Outputs {
				hang,
				hang_track,
				msf_track,
				hls,
				broadcast: broadcast.clone(),
			},
			current: Arc::new(Mutex::new(State {
				catalog,
				owned: BTreeMap::new(),
				ext_tracks: None,
				reservations: Reservations::default(),
				closed: None,
				clock,
				anchored,
				estimates: Throttle::default(),
				revisions: Default::default(),
				msf: Msf::default(),
			})),
			hls: config.hls,
			max_age: config.max_age,
			bandwidth: config.bandwidth,
			order: Default::default(),
			sent: Default::default(),
		})
	}

	/// Track properties for a media track under this catalog: hang's media defaults, plus any
	/// [`Config::with_max_age`] override.
	///
	/// `priority` should come from [`PRIORITY`](hang::catalog::PRIORITY) for the kind of media
	/// the track carries. Chain [`with_timescale`](moq_net::track::Info::with_timescale) for a
	/// container that carries the source's own scale (CMAF and Matroska both do).
	pub fn track_info(&self, priority: i8) -> moq_net::track::Info {
		let info = hang::container::track_info(priority);
		match self.max_age {
			Some(max_age) => info.with_max_age(max_age),
			None => info,
		}
	}

	/// Resolve a timestamp, synthesizing one from the broadcast's shared
	/// [`Clock`](crate::Clock) when the caller has none.
	///
	/// Sharing the clock across the catalog's tracks keeps concurrently-produced
	/// audio and video on a single timeline.
	pub fn timestamp(&self, hint: Option<moq_net::Timestamp>) -> crate::Result<moq_net::Timestamp> {
		match hint {
			Some(pts) => Ok(pts),
			None => Ok(self.clock().now()),
		}
	}

	/// Edit the catalog in place and publish the result.
	///
	/// The closure receives the current catalog, composed from every owner so far. Edit it in place;
	/// on return the result is published, a no-op if the closure changed nothing. Independent owners
	/// can each edit only their own sections, so they compose instead of clobbering one another.
	///
	/// This is [`modify`](Self::modify) opened and committed for you; take the guard to hold the lock
	/// across several edits, or to reach a [`Guard`] method like
	/// [`set_section`](Guard::set_section).
	pub fn mutate(&mut self, f: impl FnOnce(&mut Catalog<E>)) -> crate::Result<()> {
		let mut guard = self.modify()?;
		f(&mut guard);
		guard.commit()
	}

	/// Get mutable access to the catalog, publishing it after any changes.
	///
	/// The publish happens when the returned [`Guard`] drops. Use [`mutate`](Self::mutate) for a
	/// single edit, which is this guard opened and committed for you. Fails once the catalog tracks
	/// are closed, the one publication failure that happens in normal operation, so nothing is left
	/// to check after the guard drops. Anything else that stops the drop from publishing (an extension
	/// that won't serialize, a catalog too large for a frame) aborts the catalog tracks with that
	/// error: consumers see it instead of a stale catalog, and the next `modify` returns it here.
	/// Call [`Guard::commit`] to get the error back immediately instead.
	///
	/// The in-memory catalog is already mutated, so a panic while the guard is held still
	/// publishes: skipping would leave `snapshot` ahead of the wire with no dirty flag to catch up.
	///
	/// The root `clock` is the producer's [`clock`](Self::clock). Writing a different one re-bases
	/// the clock on it until something fixes the clock ([`Config::with_clock`], taking
	/// [`clock`](Self::clock), an importer's first frame, or the first publish), so a derivative
	/// adopts its input's mapping. After that a changed `clock` is refused, as is clearing it at any
	/// time; writing the same mapping again is no change. Writes through
	/// [`Reserved::video`](super::Reserved::video) and the codec producers keep their timestamps
	/// verbatim, so they mean something only under the mapping they were written for.
	pub fn modify(&mut self) -> crate::Result<Guard<'_, E>> {
		let state = take(&self.current);
		if let Some(err) = &state.closed {
			return Err(err.clone());
		}

		Ok(Guard {
			state,
			outputs: &mut self.outputs,
			updated: false,
		})
	}

	/// Get a snapshot of the current catalog.
	pub fn snapshot(&self) -> Catalog<E> {
		let state = take(&self.current);
		self.outputs.advertise(&state.catalog).into_owned()
	}

	/// The broadcast's shared clock: the timeline producers stamp against and the wall mapping
	/// advertised at the catalog root.
	///
	/// Copies share the timeline, so handing them to concurrent producers keeps every track on
	/// one clock. Taking the clock fixes it: an importer whose first frame arrives later shifts its
	/// timestamps onto this clock rather than moving it, so a copy never goes stale.
	pub fn clock(&self) -> crate::Clock {
		let mut state = take(&self.current);
		state.anchored = true;
		state.clock
	}

	/// The clock as it stands, without fixing it: for readers that follow it on every use.
	pub(super) fn current_clock(&self) -> crate::Clock {
		take(&self.current).clock
	}

	/// The config `name` holds in `C`'s section, cloned without copying the rest of the catalog.
	pub(super) fn config<C: super::RenditionConfig<E>>(&self, name: &str) -> Option<C> {
		C::get_mut(&mut take(&self.current).catalog, name).cloned()
	}

	/// Anchor a timebase on its first timestamp `pts`, which happened at `wall` (or arrived now),
	/// returning the timebase's offset.
	///
	/// A clock nothing has fixed yet is placed so `pts` lands at that instant, and the offset is
	/// zero. A fixed one stays, and the offset shifts `pts` onto it. Either way the timebase keeps the
	/// offset, so every later frame from any of its importers shifts alike.
	pub(super) fn anchor(
		&mut self,
		timebase: &std::sync::OnceLock<super::Offset>,
		pts: moq_net::Timestamp,
		wall: Option<moq_clock::Wall>,
	) -> crate::Result<super::Offset> {
		let mut guard = self.modify()?;
		// Another clone of the timebase may have anchored first; the lock orders them. A placement
		// is refused then, since it would not land where it says.
		if let Some(offset) = timebase.get() {
			if wall.is_some() {
				return Err(crate::Error::UnmappableTimestamp(
					"the timebase already anchored on its first frame".to_string(),
				));
			}
			return Ok(*offset);
		}

		let offset = if guard.state.anchored {
			let clock = guard.state.clock;
			let at = match wall {
				Some(wall) => clock.reading(wall)?,
				None => clock.now().as_micros() as i128,
			};
			super::Offset::from_micros(at - pts.as_micros() as i128)?
		} else {
			let since = std::time::Duration::from_micros(u64::try_from(pts.as_micros()).unwrap_or(u64::MAX));
			let clock = match wall {
				Some(wall) => crate::Clock::placed(since, wall)?,
				None => crate::Clock::arrival(since)?,
			};
			guard.state.clock = clock;
			guard.state.anchored = true;
			match wall {
				// A placement can precede every track. It stages no publish, so the catalog first
				// goes out listing them rather than carrying a clock alone.
				Some(_) => guard.state.catalog.clock = clock.wall(),
				None => guard.clock = clock.wall(),
			}
			super::Offset::default()
		};

		timebase.set(offset).expect("set under the catalog lock");
		guard.commit()?;
		Ok(offset)
	}

	/// A timestamp base for one source, shared by every importer reading it.
	///
	/// Mint each importer's reservation from it with [`Timebase::reserve`](super::Timebase::reserve), so
	/// they all shift by one offset. See [`Timebase`](super::Timebase).
	pub fn timebase(&self) -> super::Timebase<E> {
		super::Timebase::new(self.clone())
	}

	/// Begin reserving the initial track set, returning a clonable [`Reserved`](super::Reserved).
	///
	/// Hand it (or clones) to importers, or call its role-specific track constructors. The catalog is
	/// withheld until every reservation is dropped and every created producer either publishes its
	/// config or drops, and after that until the catalog names a track. A one-shot muxer therefore
	/// publishes the complete track list in its first snapshot instead of a half-converged one, and
	/// input that declares no track publishes nothing. An application's extension track counts only
	/// once a track constructor such as [`track`](Self::track) publishes it; one seeded or written
	/// through [`modify`](Self::modify) is invisible to this check. Producers created directly on a
	/// catalog that never reserved publish incrementally.
	///
	/// Each call is a fresh [`Timebase`](super::Timebase): an importer handed it shifts its timestamps by
	/// its own offset. Reserve through one timebase instead for importers sharing a timestamp base.
	pub fn reserve(&self) -> super::Reserved<E> {
		self.timebase().reserve()
	}

	/// Take a rendition name without withholding the catalog's initial snapshot.
	///
	/// Use this for a long-lived incremental producer whose config may arrive
	/// later or never arrive. The returned handle owns the name immediately,
	/// publishes the config on [`set`](super::Rendition::set), and retires it on
	/// drop. Use [`reserve`](Self::reserve) when the initial catalog must wait for
	/// the complete track set instead.
	pub(super) fn rendition<C: super::RenditionConfig<E>>(
		&self,
		name: impl Into<String>,
	) -> crate::Result<super::Rendition<E, C>> {
		super::Rendition::live(self.clone(), name.into())
	}

	/// Publish a video track and own its catalog rendition.
	pub fn video<C: crate::container::Container>(
		&self,
		track: moq_net::track::Producer,
		container: C,
		config: impl Into<Option<hang::catalog::VideoConfig>>,
	) -> crate::Result<crate::container::Producer<C, hang::catalog::VideoConfig>>
	where
		crate::Error: From<C::Error>,
	{
		self.track(track, container, config)
	}

	/// Publish an audio track and own its catalog rendition.
	pub fn audio<C: crate::container::Container>(
		&self,
		track: moq_net::track::Producer,
		container: C,
		config: impl Into<Option<hang::catalog::AudioConfig>>,
	) -> crate::Result<crate::container::Producer<C, hang::catalog::AudioConfig>>
	where
		crate::Error: From<C::Error>,
	{
		self.track(track, container, config)
	}

	/// Publish a text track and own its catalog rendition.
	pub fn text<C: crate::container::Container>(
		&self,
		track: moq_net::track::Producer,
		container: C,
		config: impl Into<Option<hang::catalog::TextConfig>>,
	) -> crate::Result<crate::container::Producer<C, hang::catalog::TextConfig>>
	where
		crate::Error: From<C::Error>,
	{
		self.track(track, container, config)
	}

	/// Publish a track using a custom catalog config.
	pub fn track<C, R>(
		&self,
		track: moq_net::track::Producer,
		container: C,
		config: impl Into<Option<R>>,
	) -> crate::Result<crate::container::Producer<C, R>>
	where
		C: crate::container::Container,
		R: super::RenditionConfig<E>,
		crate::Error: From<C::Error>,
	{
		let rendition = self.rendition(track.name())?;
		self.media(track, container, rendition, config.into(), None)
	}

	/// Count the tracks `lists` finds in the extension toward the initial publish's
	/// [track requirement](State::lists_track), for an importer that records tracks there.
	pub(crate) fn count_ext_tracks(&self, lists: fn(&mut E) -> bool) {
		take(&self.current).ext_tracks = Some(lists);
	}

	/// Whether a live producer or published config already claims `name` in `R`'s section.
	pub fn is_claimed<R: super::RenditionConfig<E>>(&self, name: &str) -> bool {
		let mut state = take(&self.current);
		R::get_mut(&mut state.catalog, name).is_some() || state.owned.contains_key(&owner_key::<R>(name))
	}

	/// Take `name` in `C`'s section for a new [`Rendition`](super::Rendition), which owns it until
	/// the handle drops.
	///
	/// Errors if a live rendition already holds the name, or if the catalog already carries an entry
	/// under it. That's what makes the rendition the only thing that writes or removes its own slot:
	/// a lazily-configured importer (H.264 before its first SPS) has no catalog entry yet, and
	/// without the name being held an outside write would land in the gap, be overwritten by
	/// [`set`](super::Rendition::set), and then be deleted by the rendition's `Drop`.
	pub(super) fn acquire<C: super::RenditionConfig<E>>(&self, name: &str) -> crate::Result<()> {
		let mut state = take(&self.current);
		let key = owner_key::<C>(name);
		if C::get_mut(&mut state.catalog, name).is_some() || state.owned.contains_key(&key) {
			return Err(hang::Error::Duplicate(name.to_string()).into());
		}
		state.owned.insert(key, false);
		Ok(())
	}

	/// Register a live [`Reserved`](super::Reserved) handle.
	pub(super) fn add_reserver(&self) {
		let mut state = take(&self.current);
		state.reservations.reservers += 1;
		state.reservations.reserved = true;
	}

	/// Drop a [`Reserved`](super::Reserved) handle, flushing the initial snapshot if that was the
	/// last thing gating it and the catalog names a track.
	pub(super) fn release_reserver(&mut self) {
		{
			let mut state = take(&self.current);
			let r = &mut state.reservations;
			r.reservers = r.reservers.saturating_sub(1);
		}
		self.flush_if_ready();
	}

	/// Publish the buffered snapshot once, if buffering has just finished with a change staged.
	pub(super) fn flush_if_ready(&mut self) {
		let mut state = take(&self.current);
		let r = &state.reservations;
		// Nothing was staged (e.g. every stream was ignored): stay unpublished so a later change
		// still triggers the first emit, rather than publishing an empty catalog now. The track
		// scan runs last, so releasing each of many reservations stays linear.
		if r.published || r.reservers != 0 || !r.pending || !state.lists_track() {
			return;
		}
		let r = &mut state.reservations;
		r.pending = false;
		r.published = true;
		// A snapshot on the wire is a mapping a reader may have copied.
		state.anchored = true;
		state.estimates.fold();
		// Emitted under the lock, as a guard's publish is, since the MSF state belongs to it.
		let state = &mut *state;
		if let Err(err) = self.outputs.emit(&state.catalog, &mut state.msf) {
			tracing::error!(%err, "failed to publish the catalog, aborting its tracks");
			self.outputs.abort(&err);
			state.closed = Some(err);
		}
	}

	/// Create the successor of track `name` under the next `<name>/<n>` this catalog has not
	/// handed out, for a rendition whose identity changed: a name always means the same content.
	///
	/// A successor's own successor counts on from the same base, so `v/1` is followed by `v/2`.
	pub(crate) fn fresh_track(
		&self,
		name: &str,
		info: moq_net::track::Info,
	) -> crate::Result<moq_net::track::Producer> {
		let mut state = take(&self.current);
		let base = match name.rsplit_once('/') {
			Some((base, n)) if n.parse::<u64>().is_ok() && state.revisions.contains_key(base) => base,
			_ => name,
		};
		let revision = state.revisions.entry(base.to_string()).or_default();
		loop {
			*revision += 1;
			match self
				.outputs
				.broadcast
				.create_track(format!("{base}/{revision}"), info.clone())
			{
				// Someone else published that name explicitly; try the next.
				Err(moq_net::Error::Duplicate) => continue,
				track => return Ok(track?),
			}
		}
	}

	/// Build the media [`container::Producer`](crate::container::Producer) for `track`,
	/// enrolling a video or audio track for HLS records unless [`Config::with_hls`] disabled them.
	pub(super) fn media<C, R>(
		&self,
		track: moq_net::track::Producer,
		container: C,
		rendition: super::Rendition<E, R>,
		config: Option<R>,
		hls_hint: Option<std::time::Duration>,
	) -> crate::Result<crate::container::Producer<C, R>>
	where
		C: crate::container::Container,
		R: super::RenditionConfig<E>,
		crate::Error: From<C::Error>,
	{
		// Only video and audio renditions advertise records, and a sparse role (text) enrolled
		// first would pace them, holding their segments open between its groups.
		let role = std::any::TypeId::of::<R>();
		let recorded = role == std::any::TypeId::of::<hang::catalog::VideoConfig>()
			|| role == std::any::TypeId::of::<hang::catalog::AudioConfig>();
		let recorder = match recorded {
			true => self.enroll(track.name(), hls_hint)?,
			false => None,
		};
		let sent = self.sent(rendition.name());
		let mut producer = crate::container::Producer::with_rendition(track, container, rendition)
			.with_recorder(recorder)
			.with_bandwidth(self.bandwidth.clone())
			.with_sent(sent);
		if let Some(config) = config {
			producer.set(config)?;
		}
		Ok(producer)
	}

	/// Build an internal container producer for a non-rendition catalog entry.
	pub(crate) fn media_raw<C>(
		&self,
		track: moq_net::track::Producer,
		container: C,
	) -> crate::Result<crate::container::Producer<C>>
	where
		C: crate::container::Container,
		crate::Error: From<C::Error>,
	{
		let sent = self.sent(track.name());
		Ok(crate::container::Producer::new(track, container)
			.with_bandwidth(self.bandwidth.clone())
			.with_sent(sent))
	}

	/// The counters a track writing under catalog key `name` bumps, for one that writes its groups
	/// by hand rather than through a [`container::Producer`](crate::container::Producer).
	pub(crate) fn sent(&self, name: &str) -> Arc<crate::stats::Sent> {
		self.sent.track(name)
	}

	/// What each media track minted under this catalog has written, keyed by its catalog key: the
	/// rendition ID, or the track name for an `m2ts` or verbatim `mpegts` track.
	///
	/// Counters are cumulative for the life of the catalog, so a track that ended keeps its final
	/// counts and a later track under the same key continues them, until 1,024 keys are held: then
	/// ended tracks make way for new ones. Fold this into a
	/// [`hang::stats::Snapshot`] on the stats interval.
	pub fn stats(&self) -> std::collections::BTreeMap<String, hang::stats::Rendition> {
		self.sent.sent()
	}

	/// A fresh estimator whose `delay` is measured against this catalog's other renditions.
	pub(crate) fn estimator(&self) -> super::Estimator {
		super::Estimator::with_order(self.order.clone())
	}

	/// The allocator passthrough tracks claim on. fMP4 writes groups by hand, so it reads this itself.
	pub(crate) fn bandwidth(&self) -> moq_net::bandwidth::Allocator {
		self.bandwidth.clone()
	}

	/// Enroll `track` for HLS records unless [`Config::with_hls`] disabled them, advertising them in its
	/// rendition's `hls` field once the nominal duration is known.
	///
	/// The video and audio track constructors call this for you. fMP4 passthrough calls it directly
	/// because it writes groups by hand instead of using a [`container::Producer`](crate::container::Producer).
	pub(crate) fn enroll(
		&self,
		track: &str,
		hint: Option<std::time::Duration>,
	) -> crate::Result<Option<crate::hls::Recorder>> {
		if let Some(err) = &take(&self.current).closed {
			return Err(err.clone());
		}
		let (Some(tracks), Some(config)) = (&self.outputs.hls, &self.hls) else {
			return Ok(None);
		};
		let mut config = config.clone();
		if hint.is_some() {
			config = config.with_duration_hint(hint);
		}
		let mut recorder = tracks.track(track, config)?;
		if tracks.hls(track).is_some() {
			// Hinted: the entry the track belongs to advertises it when it commits.
			return Ok(Some(recorder));
		}

		// Unhinted: the first segment declares the duration, which republishes the catalog.
		let current = self.current.clone();
		let mut outputs = self.outputs.clone();
		recorder.on_declare(move |_| {
			let mut state = take(&current);
			// Before the initial publish, that publish advertises it whenever its gate opens.
			if state.closed.is_some() || !state.reservations.published {
				return;
			}
			let state = &mut *state;
			if let Err(err) = outputs.emit(&state.catalog, &mut state.msf) {
				tracing::error!(%err, "failed to publish an HLS declaration, aborting catalog tracks");
				outputs.abort(&err);
				state.closed = Some(err);
			}
		});
		Ok(Some(recorder))
	}

	/// How many HLS record tracks are held, live or kept for a re-enrollment.
	#[cfg(test)]
	pub(crate) fn hls_tracks(&self) -> usize {
		self.outputs.hls.as_ref().map_or(0, |tracks| tracks.len())
	}

	/// Finish and forget the HLS records of a dropped `track` whose name is never published again.
	pub fn retire(&self, track: &str) -> crate::Result<()> {
		match &self.outputs.hls {
			Some(tracks) => tracks.retire(track),
			None => Ok(()),
		}
	}

	/// [`retire`](Self::retire) `track` from a teardown that cannot return the error.
	pub(crate) fn retire_dropped(&self, track: &str) {
		if let Err(err) = self.retire(track) {
			tracing::warn!(%err, track, "failed to retire a dropped track's HLS records");
		}
	}

	/// Publish `track` as a latest-value JSON track, advertising it in the catalog.
	///
	/// The caller creates the track on the broadcast, as it does for a media track; this writes its
	/// catalog entry and removes the entry when the returned handle drops. The catalog key is
	/// [`track.name()`](moq_net::track::Producer::name) verbatim. Create `track` with
	/// [`compress`](moq_net::track::Info::compress) set: JSON is small and repetitive.
	///
	/// `config` is a [`json::Config`](crate::json::Config) for the `json` section, or an
	/// application's own entry embedding a [`JsonConfig`](hang::catalog::JsonConfig) (see
	/// [`IntoRendition`](super::IntoRendition)). The producer sets its `mode` and fills an absent
	/// `bitrate` from what it writes. `options`
	/// configures the encoder and is not written into the catalog entry. With
	/// [`deltas`](crate::json::Options::deltas), create `track` with a
	/// [`refresh`](moq_net::track::Info::refresh) ratio and drive
	/// [`Snapshot::refresh`](crate::json::Snapshot::refresh), so joiners get a fresh group.
	///
	/// Errors if the entry's section already carries that name, for example an entry seeded
	/// through [`Config::with_catalog`] or one pointing at a sibling broadcast, or if the entry
	/// references another broadcast.
	pub fn json_snapshot<T: serde::Serialize>(
		&self,
		track: moq_net::track::Producer,
		config: impl super::IntoRendition<E, hang::catalog::JsonConfig>,
		options: crate::json::Options,
	) -> crate::Result<crate::json::Snapshot<T>> {
		let rendition = self.data_entry(track.name())?;
		crate::json::Snapshot::new(track, rendition, config.into_rendition(), options)
	}

	/// Publish `track` as an append-log JSON track, advertising it in the catalog.
	///
	/// See [`json_snapshot`](Self::json_snapshot) for the lifecycle; this differs only in that every
	/// record is preserved rather than superseded.
	pub fn json_stream<T: serde::Serialize>(
		&self,
		track: moq_net::track::Producer,
		config: impl super::IntoRendition<E, hang::catalog::JsonConfig>,
	) -> crate::Result<crate::json::Stream<T>> {
		let rendition = self.data_entry(track.name())?;
		crate::json::Stream::new(track, rendition, config.into_rendition())
	}

	/// Reserve the catalog entry a data producer owns, keyed by its track name.
	///
	/// The rendition is reserved but not yet set, so the caller fills it in with the config for the
	/// mode it is about to publish. Reserving is what rejects a name the catalog already carries,
	/// including an entry with no local track behind it (seeded through
	/// [`Config::with_catalog`], or pointing at a sibling broadcast), which publishing over would
	/// silently replace and then retire when this handle drops.
	fn data_entry<C: super::RenditionConfig<E>>(&self, name: &str) -> crate::Result<super::Rendition<E, C>> {
		self.reserve().init(name)
	}

	/// Create a consumer for this catalog, receiving updates as they're published.
	pub fn consume(&self) -> Result<Consumer<E>, moq_net::Error> {
		Ok(Consumer::new(self.outputs.hang.consume()))
	}

	/// Abort every catalog track with `err`, preventing further edits.
	pub fn abort(mut self, err: moq_net::Error) {
		let reason = crate::Error::from(err.clone());
		{
			let mut state = take(&self.current);
			if state.closed.is_some() {
				return;
			}
			state.closed = Some(reason.clone());
		}
		self.outputs.abort(&reason);
		// The records fail too, rather than reading as a complete recording.
		if let Some(hls) = &self.outputs.hls {
			hls.abort(&err);
		}
	}

	/// Finish publishing to this catalog.
	///
	/// An estimate rise still held by the once-a-second limit is not published: media has ended.
	pub fn finish(&mut self) -> crate::Result<()> {
		// A track that ends before its first segment closes declares its duration now, while the
		// catalog can still advertise it.
		if let Some(hls) = &self.outputs.hls {
			hls.flush();
		}
		{
			let mut state = take(&self.current);
			if let Some(err) = &state.closed {
				return Err(err.clone());
			}
			state.closed = Some(moq_net::Error::Closed.into());
		}
		self.outputs.hang.finish()?;
		self.outputs.msf_track.finish()?;
		if let Some(hls) = &self.outputs.hls {
			hls.finish();
		}
		Ok(())
	}
}

/// RAII guard for modifying a catalog with automatic publishing on drop.
///
/// Obtained via [`Producer::modify`]. Derefs to the [`Catalog<E>`](super::hang::Catalog), so base
/// sections are editable directly and application sections are editable through `ext`.
///
/// On drop, the hang and MSF catalog tracks are updated if the catalog was
/// mutated. That publish cannot return an error, so a failure (an extension that won't serialize,
/// a catalog too large for a frame) aborts the catalog tracks instead: consumers see the error and
/// the next [`Producer::modify`] returns it. Call [`commit`](Self::commit) when the caller can act
/// on the failure itself.
pub struct Guard<'a, E: CatalogExt = ()> {
	state: MutexGuard<'a, State<E>>,
	outputs: &'a mut Outputs<E>,
	updated: bool,
}

impl<E: CatalogExt> Guard<'_, E> {
	/// Publish the edited catalog to every catalog track, returning any error.
	///
	/// Consumes the guard, so the subsequent drop publishes nothing. A no-op if the catalog was never
	/// mutated, and still withheld while a [`Reserved`](super::Reserved) gates the initial snapshot.
	/// Unlike a drop, a failure here leaves the tracks open: the caller has the error and decides
	/// what to do with it.
	pub fn commit(mut self) -> crate::Result<()> {
		self.publish()
	}

	/// Publish a mutated catalog once, clearing the flag so it isn't published again.
	fn publish(&mut self) -> crate::Result<()> {
		if !self.updated {
			return Ok(());
		}
		self.updated = false;

		// The root `clock` is the producer's clock: never advertise one it disagrees with.
		match adopt(self.state.clock, self.state.anchored, self.state.catalog.clock) {
			Ok(clock) => {
				self.state.clock = clock;
				self.state.catalog.clock = clock.wall();
			}
			Err(err) => {
				self.state.catalog.clock = self.state.clock.wall();
				return Err(err);
			}
		}

		{
			// Withhold every emit while still buffering the initial reserved set, or until a reserved
			// catalog names a track; the mutation stays in `current` and `pending` marks it for the
			// flush once the gate opens. The track scan runs last, so a published catalog skips it.
			let r = &self.state.reservations;
			let withhold = !r.published && (r.reservers != 0 || (r.reserved && !self.state.lists_track()));
			let r = &mut self.state.reservations;
			if withhold {
				r.pending = true;
				return Ok(());
			}
			r.pending = false;
			r.published = true;
		}
		// A snapshot on the wire is a mapping a reader may have copied.
		self.state.anchored = true;

		// Folded only once it is on the wire, so a failed emit leaves the estimate held.
		let state = &mut *self.state;
		self.outputs.emit(&state.catalog, &mut state.msf)?;
		self.state.estimates.fold();
		Ok(())
	}

	/// Publish an estimate-driven edit, at most once per [`ESTIMATE_WINDOW`] when `throttle` is set.
	///
	/// The first estimate publishes at once. Later ones inside the window stay in the catalog and go
	/// out with the first call after the window ends, which an unchanged estimate makes too, or with
	/// any [`commit`](Self::commit) before then. Without `throttle` it publishes at once, still
	/// opening a window.
	pub(super) fn commit_estimate(mut self, throttle: bool) -> crate::Result<()> {
		if std::mem::take(&mut self.updated) {
			self.state.estimates.pending = true;
		}
		let estimates = &self.state.estimates;
		if !estimates.pending {
			return Ok(());
		}
		let now = moq_clock::now();
		if throttle && estimates.last.is_some_and(|last| now < last + ESTIMATE_WINDOW) {
			return Ok(());
		}
		self.updated = true;
		self.publish()
	}

	/// Declare `estimate` in MSF for the rendition holding `name` in `C`'s section, in place of
	/// whatever it measures. It goes out with the next publish.
	pub(super) fn hint<C: super::RenditionConfig<E>>(&mut self, name: &str, estimate: super::Estimate) {
		let hint = Hint {
			estimate,
			apply: apply_hint::<E, C>,
		};
		self.state.msf.hints.insert(owner_key::<C>(name), hint);
	}

	/// Mark the rendition holding `name` in `C`'s section as listed, so it counts toward the
	/// initial publish's [track requirement](State::lists_track).
	pub(super) fn list<C: super::RenditionConfig<E>>(&mut self, name: &str) {
		if let Some(listed) = self.state.owned.get_mut(&owner_key::<C>(name)) {
			*listed = true;
		}
	}

	/// Release a name taken by [`Producer::acquire`], along with the entry it owns.
	///
	/// Both under one lock, so an [`acquire`](Producer::acquire) never sees the rendition half-gone:
	/// name freed with its entry still there (a spurious duplicate), or entry gone with the name
	/// still held (a spurious refusal).
	///
	/// The name goes first, before any caller code. [`RenditionConfig::remove`](super::RenditionConfig::remove)
	/// is a third-party hook that can panic, and a stranded entry is recoverable while a stranded
	/// name is not: nothing would ever reserve it again.
	pub(super) fn release<C: super::RenditionConfig<E>>(&mut self, name: &str, present: bool) {
		self.state.owned.remove(&owner_key::<C>(name));
		self.state.msf.hints.remove(&owner_key::<C>(name));

		if present {
			C::remove(&mut self.state.catalog, name);
			self.updated = true;
		}
	}
}

impl<E: CatalogExt> Deref for Guard<'_, E> {
	type Target = Catalog<E>;

	fn deref(&self) -> &Self::Target {
		&self.state.catalog
	}
}

impl<E: CatalogExt> DerefMut for Guard<'_, E> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		self.updated = true;
		&mut self.state.catalog
	}
}

impl Guard<'_, Extra> {
	/// Set (or replace) a top-level application catalog section, republished on drop.
	///
	/// Errors if `name` is a HANG root (`video`, `audio`, `text`, `archive`, `clock`, `json`,
	/// `binary`, or retired `timeline`) or an MSF root (`version`, `generatedAt`, `isComplete`,
	/// `tracks`, or `initDataList`).
	pub fn set_section(&mut self, name: impl Into<String>, value: serde_json::Value) -> crate::Result<()> {
		self.state.catalog.ext.set(name, value)?;
		self.updated = true;
		Ok(())
	}

	/// Remove a top-level application catalog section, republished on drop if it existed.
	///
	/// Returns the section's previous value, or `None` if it was absent.
	pub fn remove_section(&mut self, name: &str) -> Option<serde_json::Value> {
		let removed = self.state.catalog.ext.remove(name);
		if removed.is_some() {
			self.updated = true;
		}
		removed
	}
}

impl<E: CatalogExt> Drop for Guard<'_, E> {
	fn drop(&mut self) {
		if let Err(err) = self.publish() {
			if !std::thread::panicking() {
				tracing::error!(%err, "failed to publish the catalog on guard drop, aborting its tracks");
			}
			self.outputs.abort(&err);
			self.state.closed = Some(err);
		}
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

/// The MSF packaging that announces `container`, or `None` to leave the rendition out of the catalog.
///
/// Exhaustive on purpose: a catch-all is what announced LOC as legacy, and it would do the same to the
/// next container added. hang says an unrecognized one must be ignored, so it keeps its wire kind (no
/// MSF consumer claims one it does not know) and is dropped when it does not even name one.
fn packaging_of(container: &hang::catalog::Container) -> Option<moq_msf::Packaging> {
	match container {
		hang::catalog::Container::Cmaf { .. } => Some(moq_msf::Packaging::Cmaf),
		hang::catalog::Container::Loc => Some(moq_msf::Packaging::Loc),
		hang::catalog::Container::Legacy => Some(moq_msf::Packaging::Legacy),
		hang::catalog::Container::Unknown(unknown) => {
			unknown.kind().map(|kind| moq_msf::Packaging::Unknown(kind.to_string()))
		}
	}
}

/// Convert a catalog to its MSF equivalent: the media sections become MSF tracks and the
/// extension rides the MSF root under the same member names it uses in the hang catalog.
fn to_msf<E: CatalogExt>(catalog: &Catalog<E>, base: &moq_net::Path) -> moq_msf::Catalog<E> {
	let mut msf = to_msf_media(&catalog.media(), base);
	msf.ext = catalog.ext.clone();
	msf
}

/// Convert the base media sections of a hang catalog into MSF tracks.
///
/// MSF names a track by its absolute namespace and wire name. An unresolvable reference, one
/// a reader would take as the catalog itself ([`names_catalog`](crate::catalog::msf::names_catalog)),
/// is refused instead of advertising the track on the catalog's broadcast. A later rendition with
/// the same identity replaces the earlier one.
fn to_msf_media<E: CatalogExt>(catalog: &hang::Catalog, base: &moq_net::Path) -> moq_msf::Catalog<E> {
	let mut tracks = Vec::new();
	let reference = |id: &str, config: &dyn Reference| {
		let name = config.track(id);
		if let Some(epoch) = config.epoch() {
			// MSF has nowhere to name an instance, so its readers take whichever resolves first.
			tracing::warn!(rendition = id, %epoch, "MSF cannot carry a reference epoch; advertising without it");
		}
		let namespace = match config.broadcast().filter(|rel| !rel.is_empty()) {
			None => None,
			Some(rel) => {
				let Some(target) = base.try_resolve(rel) else {
					tracing::error!(rendition = id, %rel, %base, "MSF broadcast reference escapes the root");
					return None;
				};
				if target == *base {
					None
				} else if crate::catalog::msf::names_catalog(&target, base) {
					tracing::error!(rendition = id, %rel, %base, "MSF readers would take this namespace as the catalog's own");
					return None;
				} else {
					Some(target.to_string())
				}
			}
		};
		Some((name.to_string(), namespace))
	};

	let has_multiple_video = catalog.video.renditions.len() > 1;
	for (name, config) in &catalog.video.renditions {
		let Some(packaging) = packaging_of(&config.container) else {
			continue;
		};

		let init_data = match &config.container {
			hang::catalog::Container::Cmaf { init, .. } => Some(base64::engine::general_purpose::STANDARD.encode(init)),
			_ => config
				.description
				.as_ref()
				.map(|d| base64::engine::general_purpose::STANDARD.encode(d.as_ref())),
		};

		let sap_type = video_sap_type(&config.codec);
		let Some((wire, namespace)) = reference(name, config) else {
			continue;
		};
		let mut track = moq_msf::Track::new(wire, packaging);
		track.namespace = namespace;
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
		track.delay = config.delay;
		tracks.push(track);
	}

	let has_multiple_audio = catalog.audio.renditions.len() > 1;
	for (name, config) in &catalog.audio.renditions {
		let Some(packaging) = packaging_of(&config.container) else {
			continue;
		};

		let init_data = match &config.container {
			hang::catalog::Container::Cmaf { init, .. } => Some(base64::engine::general_purpose::STANDARD.encode(init)),
			_ => config
				.description
				.as_ref()
				.map(|d| base64::engine::general_purpose::STANDARD.encode(d.as_ref())),
		};

		let Some((wire, namespace)) = reference(name, config) else {
			continue;
		};
		let mut track = moq_msf::Track::new(wire, packaging);
		track.namespace = namespace;
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
		track.delay = config.delay;
		tracks.push(track);
	}

	for (name, config) in &catalog.text.renditions {
		let Some(packaging) = packaging_of(&config.container) else {
			continue;
		};
		let Some((wire, namespace)) = reference(name, config) else {
			continue;
		};
		let mut track = moq_msf::Track::new(wire, packaging);
		track.namespace = namespace;
		track.is_live = true;
		track.role = Some(config.role.to_string().parse().unwrap());
		track.codec = Some(config.format.to_string());
		track.lang = config.lang.clone();
		track.label = config.label.clone();
		if let hang::catalog::Container::Cmaf { init, .. } = &config.container {
			track.init_data = Some(base64::engine::general_purpose::STANDARD.encode(init));
		}
		track.render_group = Some(1);
		track.jitter = config.jitter;
		track.delay = config.delay;
		tracks.push(track);
	}

	let mut index = HashMap::new();
	let mut unique: Vec<moq_msf::Track> = Vec::with_capacity(tracks.len());
	for track in tracks {
		match index.entry((track.namespace.clone(), track.name.clone())) {
			Entry::Occupied(entry) => unique[*entry.get()] = track,
			Entry::Vacant(entry) => {
				entry.insert(unique.len());
				unique.push(track);
			}
		}
	}
	moq_msf::Catalog::with_ext(unique, E::default())
}

#[cfg(test)]
mod test {
	use std::collections::BTreeMap;

	use std::str::FromStr;
	use std::task::Poll;

	use bytes::Bytes;
	use hang::catalog::{AudioCodec, AudioConfig, Container, H264, VideoConfig};

	use super::*;

	#[tokio::test(start_paused = true)]
	async fn msf_publishes_the_referenced_namespace() {
		for (path, expected) in [(Some("room/catalog"), "room/left"), (Some(""), "left"), (None, "left")] {
			let (origin, driver) = moq_net::origin::Producer::new(Default::default());
			let driver = tokio::spawn(moq_net::time::run(driver));
			let mut broadcast = match path {
				Some(path) => origin.create_broadcast(path).unwrap().broadcast().clone(),
				None => moq_net::broadcast::Info::new().produce(),
			};
			let mut producer = Producer::new(&mut broadcast, Config::default()).unwrap();
			let mut config = VideoConfig::new(hang::catalog::VideoCodec::VP8);
			config.container = Container::Loc;
			config.broadcast = Some(moq_net::path::Relative::new("./left").unwrap());
			producer
				.modify()
				.unwrap()
				.video
				.renditions
				.insert("video".into(), config);
			let mut track = broadcast
				.consume()
				.track(moq_msf::DEFAULT_NAME)
				.unwrap()
				.subscribe(None)
				.await
				.unwrap()
				.ordered();
			let mut group = track.next_group().await.unwrap().unwrap();
			let frame = group.read_frame().await.unwrap().unwrap();
			let wire: serde_json::Value = serde_json::from_slice(&frame.payload).unwrap();
			assert_eq!(wire["tracks"][0]["namespace"], expected);
			driver.abort();
		}
	}

	#[test]
	fn msf_omits_media_report_discovery() {
		let catalog =
			hang::Catalog::<()>::from_str(include_str!("../../../hang/fixtures/catalog-stats-echo.json")).unwrap();
		let msf = to_msf::<()>(&catalog, &moq_net::Path::empty());
		let value = serde_json::to_value(msf).unwrap();
		assert!(value.get("stats").is_none());
		assert!(value.get("echo").is_none());
		assert!(!value["tracks"].as_array().unwrap().is_empty());
	}

	#[tokio::test(start_paused = true)]
	async fn abort_preserves_the_cause_on_every_catalog_format_and_clone() {
		let mut broadcast = moq_net::broadcast::Info::default().produce();
		let catalog = Producer::<()>::new(&mut broadcast, Config::default()).unwrap();
		let mut copy = catalog.clone();
		let mut readers = Vec::new();
		for name in [hang::Catalog::DEFAULT_NAME, moq_msf::DEFAULT_NAME] {
			readers.push(broadcast.consume().track(name).unwrap().subscribe(None).await.unwrap());
		}
		catalog.abort(moq_net::Error::Unauthorized);
		for reader in &mut readers {
			assert!(matches!(reader.recv_group().await, Err(moq_net::Error::Unauthorized)));
		}
		assert!(copy.finish().is_err());
		assert!(matches!(
			copy.modify().err(),
			Some(crate::Error::Moq(moq_net::Error::Unauthorized))
		));
		copy.abort(moq_net::Error::Cancel);
	}

	/// An aborted catalog fails its HLS records with the cause, so an edge never ends the playlist
	/// as a complete recording.
	#[tokio::test(start_paused = true)]
	async fn abort_fails_the_hls_records() {
		let mut broadcast = moq_net::broadcast::Info::default().produce();
		let catalog = Producer::<()>::new(&mut broadcast, Config::default()).unwrap();
		let _recorder = catalog
			.enroll("video0", Some(std::time::Duration::from_secs(4)))
			.unwrap();
		let mut reader = broadcast
			.consume()
			.track("video0.hls")
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();
		catalog.abort(moq_net::Error::Unauthorized);
		loop {
			match reader.recv_group().await {
				Ok(Some(_)) => continue,
				Ok(None) => panic!("the records ended as complete"),
				Err(err) => {
					assert!(matches!(err, moq_net::Error::Unauthorized), "{err:?}");
					break;
				}
			}
		}
	}

	#[tokio::test(start_paused = true)]
	async fn abort_does_not_replace_a_finished_catalog() {
		let mut broadcast = moq_net::broadcast::Info::default().produce();
		let mut catalog = Producer::<()>::new(&mut broadcast, Config::default()).unwrap();
		let copy = catalog.clone();
		let mut reader = broadcast
			.consume()
			.track(hang::Catalog::DEFAULT_NAME)
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();
		catalog.finish().unwrap();
		copy.abort(moq_net::Error::Unauthorized);
		while reader.recv_group().await.unwrap().is_some() {}
		assert!(matches!(
			catalog.modify().err(),
			Some(crate::Error::Moq(moq_net::Error::Closed))
		));
	}

	/// The catalog, its MSF twin, and the HLS records all rank above media. They're the
	/// index a player reads before any media is useful, and they're small enough that
	/// sitting above media can't starve it. Regression: the MSF and record tracks
	/// were minted with no `Info` at all, so one broadcast advertised its hang catalog
	/// at 100 and the same catalog in MSF at 0.
	#[tokio::test]
	async fn non_media_tracks_rank_above_media() {
		use hang::catalog::PRIORITY;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		let _recorder = catalog.enroll("video", None).unwrap();

		let consumer = broadcast.consume();
		for name in [hang::Catalog::DEFAULT_NAME, moq_msf::DEFAULT_NAME, "video.hls"] {
			let track = consumer.track(name).expect("track");
			let info = track.info().await.expect("info");
			assert_eq!(info.priority, PRIORITY.catalog, "{name} should rank with the catalog");
		}
	}

	#[test]
	fn text_tracks_round_trip_through_msf() {
		for (format, role) in [
			(hang::catalog::TextFormat::Vtt, hang::catalog::TextRole::Caption),
			(hang::catalog::TextFormat::Ttml, hang::catalog::TextRole::Subtitle),
			(
				hang::catalog::TextFormat::Utf8,
				hang::catalog::TextRole::Unknown("commentary".into()),
			),
		] {
			let mut config = hang::catalog::TextConfig::new(format);
			config.role = role;
			config.lang = Some("es-419".into());
			config.label = Some("Español".into());
			config.jitter = Some(std::time::Duration::from_millis(7));
			config.delay = Some(std::time::Duration::from_millis(11));
			config.container = Container::Loc;
			let mut catalog = hang::Catalog::<()>::default();
			catalog.text.insert("text", config.clone()).unwrap();
			let msf = to_msf_media::<()>(&catalog, &moq_net::Path::empty());
			assert_eq!(msf.tracks.len(), 1);
			let wire = msf.to_json().unwrap();
			assert!(wire.contains(r#""lang":"es-419""#));
			assert!(wire.contains(r#""label":"Español""#));
			let msf = moq_msf::Catalog::<()>::from_str(&wire).unwrap();
			let decoded = crate::catalog::msf::from_msf(&msf, &moq_net::Path::empty()).unwrap();
			assert_eq!(decoded.text.renditions["text"], config);
			let mut roundtrip = hang::Catalog::<()>::default();
			roundtrip.text = decoded.text;
			assert_eq!(
				to_msf_media::<()>(&roundtrip, &moq_net::Path::empty()).tracks,
				msf.tracks
			);
		}
	}

	#[test]
	fn msf_names_each_track_by_its_wire_name() {
		let catalog = hang::Catalog::<()>::from_str(
			r#"{"video": {"renditions": {
				"hd": {"codec": "avc1.64001f", "container": {"kind": "legacy"}},
				"left": {"broadcast": "./left", "track": "video", "codec": "avc1.64001f", "container": {"kind": "legacy"}},
				"right": {"broadcast": "./right", "track": "video", "codec": "avc1.64001f", "container": {"kind": "legacy"}}
			}}}"#,
		)
		.unwrap();
		let msf = to_msf_media::<()>(&catalog, &moq_net::Path::empty());
		let names: Vec<_> = msf.tracks.iter().map(|track| track.name.as_str()).collect();
		assert_eq!(names, ["hd", "video", "video"]);
		assert_eq!(msf.tracks[1].namespace.as_deref(), Some("left"));
		assert_eq!(msf.tracks[2].namespace.as_deref(), Some("right"));
	}

	#[test]
	fn cross_broadcast_renditions_round_trip_through_msf() {
		let base = moq_net::Path::from_static("room/catalog");
		let catalog = hang::Catalog::<()>::from_str(r#"{
			"video": {"renditions": {"view": {"broadcast": "./left", "track": "video", "codec": "vp8", "container": {"kind": "loc"}}}},
			"audio": {"renditions": {"sound": {"broadcast": "./right", "track": "audio", "codec": "opus", "sampleRate": 48000, "numberOfChannels": 2, "container": {"kind": "loc"}}}},
			"text": {"renditions": {"words": {"broadcast": "../captions", "track": "text", "format": "vtt", "role": "caption", "container": {"kind": "loc"}}}}
		}"#).unwrap();
		let msf = to_msf_media::<()>(&catalog, &base);
		let wire = msf.to_json().unwrap();
		let msf = moq_msf::Catalog::<()>::from_str(&wire).unwrap();
		let restored = crate::catalog::msf::from_msf(&msf, &base).unwrap();
		assert_eq!(
			base.try_resolve(restored.video.renditions["video"].broadcast.as_ref().unwrap()),
			Some(moq_net::Path::from_static("room/left"))
		);
		assert_eq!(
			base.try_resolve(restored.audio.renditions["audio"].broadcast.as_ref().unwrap()),
			Some(moq_net::Path::from_static("room/right"))
		);
		assert_eq!(
			base.try_resolve(restored.text.renditions["text"].broadcast.as_ref().unwrap()),
			Some(moq_net::Path::from_static("captions"))
		);
	}

	#[test]
	fn msf_refuses_unresolvable_broadcast_references() {
		let base = moq_net::Path::from_static("room/catalog");
		for reference in ["../../left", "../../../left"] {
			let mut catalog = hang::Catalog::<()>::default();
			let mut config = VideoConfig::new(hang::catalog::VideoCodec::VP8);
			config.broadcast = Some(moq_net::path::Relative::new(reference).unwrap());
			catalog.video.renditions.insert("video".into(), config);
			assert!(to_msf_media::<()>(&catalog, &base).tracks.is_empty());
			assert!(to_msf_media::<()>(&catalog, &moq_net::Path::empty()).tracks.is_empty());
		}
	}

	/// A reader takes a namespace aligned with the catalog's tail as the catalog itself, so a
	/// reference to such a broadcast is refused rather than read from the catalog's broadcast.
	#[test]
	fn msf_refuses_references_readers_would_take_as_the_catalog() {
		let base = moq_net::Path::from_static("room/catalog");
		for reference in ["../catalog", "./catalog/room/catalog"] {
			let mut catalog = hang::Catalog::<()>::default();
			let mut config = VideoConfig::new(hang::catalog::VideoCodec::VP8);
			config.container = Container::Loc;
			config.broadcast = Some(moq_net::path::Relative::new(reference).unwrap());
			catalog.video.renditions.insert("video".into(), config);
			assert!(to_msf_media::<()>(&catalog, &base).tracks.is_empty(), "{reference}");
		}
	}

	/// MSF names a track by namespace and name, but a reader keys renditions by name alone, so
	/// the later of two same-named tracks wins there.
	#[test]
	fn msf_names_are_unique_per_namespace_and_the_last_wins_a_hang_id() {
		let base = moq_net::Path::from_static("room/catalog");
		let mut catalog = hang::Catalog::<()>::default();
		for (id, broadcast) in [("left", "./left"), ("right", "./right")] {
			let mut config = VideoConfig::new(hang::catalog::VideoCodec::VP8);
			config.track = Some("video".into());
			config.broadcast = Some(moq_net::path::Relative::new(broadcast).unwrap());
			catalog.video.renditions.insert(id.into(), config);
		}
		let msf = to_msf_media::<()>(&catalog, &base);
		assert_eq!(msf.tracks.len(), 2);
		assert_eq!(msf.tracks[0].namespace.as_deref(), Some("room/left"));
		assert_eq!(msf.tracks[1].namespace.as_deref(), Some("room/right"));
		let restored = crate::catalog::msf::from_msf(&msf, &base).unwrap();
		assert_eq!(restored.video.renditions.len(), 1);
		assert_eq!(
			base.try_resolve(restored.video.renditions["video"].broadcast.as_ref().unwrap()),
			Some(moq_net::Path::from_static("room/right"))
		);
	}

	/// Two renditions naming one track in one broadcast advertise it once, as the later one.
	#[test]
	fn msf_repeated_identity_keeps_the_last() {
		let mut catalog = hang::Catalog::<()>::default();
		for (id, width) in [("a", 640), ("b", 1280)] {
			let mut config = VideoConfig::new(hang::catalog::VideoCodec::VP8);
			config.container = Container::Loc;
			config.track = Some("video".into());
			config.coded_width = Some(width);
			catalog.video.renditions.insert(id.into(), config);
		}
		let msf = to_msf_media::<()>(&catalog, &moq_net::Path::empty());
		assert_eq!(msf.tracks.len(), 1);
		assert_eq!(msf.tracks[0].name, "video");
		assert_eq!(msf.tracks[0].width, Some(1280));
	}

	#[test]
	fn media_tracks_inherit_the_catalogs_declared_retention() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();

		// Unset, a catalog mints hang's media defaults, sized so a segmented egress can serve a
		// full playlist window. Raw tracks impose no publisher age limit.
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		assert_eq!(
			catalog.track_info(hang::catalog::PRIORITY.video).max_age,
			hang::container::track_info(hang::catalog::PRIORITY.video).max_age
		);

		// An override reaches every media track this catalog mints, and does NOT disturb the
		// timescale hang pins (or survive a retimescale for a source-scale container).
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let config = Config::default().with_max_age(std::time::Duration::from_secs(3));
		let catalog = Producer::new(&mut broadcast, config).unwrap();

		let info = catalog.track_info(hang::catalog::PRIORITY.video);
		assert_eq!(info.max_age, Some(std::time::Duration::from_secs(3)));
		assert_eq!(info.timescale, Some(hang::container::TIMESCALE));

		let at = info.with_timescale(moq_net::Timescale::MILLI);
		assert_eq!(at.max_age, Some(std::time::Duration::from_secs(3)));
		assert_eq!(at.timescale, Some(moq_net::Timescale::MILLI));

		// Every handle mints under the same policy, whatever order it was taken in: the codec
		// paths hold a reservation and the container paths hold a clone.
		assert_eq!(
			catalog.reserve().track_info(hang::catalog::PRIORITY.video).max_age,
			Some(std::time::Duration::from_secs(3))
		);
		assert_eq!(
			catalog.clone().track_info(hang::catalog::PRIORITY.video).max_age,
			Some(std::time::Duration::from_secs(3))
		);
	}

	#[test]
	fn publishes_one_compressed_catalog_track() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = Producer::new(&mut broadcast, Config::default()).unwrap();

		let track = catalog.outputs.hang.consume();
		assert!(catalog.outputs.hang_track.info().compress, "catalog.json is compressed");
		assert!(!catalog.outputs.msf_track.info().compress, "the MSF catalog is not");
		let mut consumer = Consumer::new(track);

		{
			let mut guard = catalog.modify().unwrap();
			guard
				.audio
				.renditions
				.insert("audio0".to_string(), AudioConfig::new(AudioCodec::Opus, 48_000, 2));
		}
		let expected = catalog.snapshot();

		let waiter = kio::Waiter::noop();
		let got = match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(c))) => c,
			other => panic!("expected a catalog, got {other:?}"),
		};
		assert_eq!(got, expected);
	}

	#[test]
	fn mutate_composes_independent_owners() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		let mut plain = Consumer::new(catalog.outputs.hang.consume());

		catalog
			.mutate(|c| {
				c.audio
					.renditions
					.insert("audio0".to_string(), AudioConfig::new(AudioCodec::Opus, 48_000, 2));
			})
			.unwrap();

		// The second owner starts from the published catalog and adds its own section.
		catalog
			.mutate(|c| {
				c.video.renditions.insert("video0".to_string(), h264_config());
			})
			.unwrap();

		// A closure that changes nothing publishes nothing: the catalog track runs one snapshot per
		// group, so a third publish would open a third group.
		catalog.mutate(|_| {}).unwrap();
		assert_eq!(sent(&catalog), Some(1));

		let expected = catalog.snapshot();
		let waiter = kio::Waiter::noop();
		let mut last = None;
		while let Poll::Ready(Ok(Some(c))) = plain.poll_next(&waiter) {
			last = Some(c);
		}
		assert_eq!(last.unwrap(), expected);
		assert!(expected.audio.renditions.contains_key("audio0"));
		assert!(expected.video.renditions.contains_key("video0"));
	}

	#[test]
	fn modify_refuses_a_finished_catalog() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = Producer::new(&mut broadcast, Config::default()).unwrap();

		catalog
			.modify()
			.unwrap()
			.audio
			.renditions
			.insert("audio0".to_string(), AudioConfig::new(AudioCodec::Opus, 48_000, 2));
		catalog.finish().unwrap();

		assert!(matches!(
			catalog.modify(),
			Err(crate::Error::Moq(moq_net::Error::Closed))
		));
		assert!(matches!(
			catalog.enroll("audio0", None),
			Err(crate::Error::Moq(moq_net::Error::Closed))
		));
	}

	#[test]
	fn a_dropped_failed_edit_aborts_every_catalog_track() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let config = Config::default().with_catalog(Catalog::<Extra>::default());
		let mut catalog = Producer::<Extra>::new(&mut broadcast, config).unwrap();
		let mut hang = catalog.outputs.hang.consume().ordered();
		let mut msf = catalog.outputs.msf_track.subscribe(None).ordered();

		// Something the hang track rejects, so the publish fails before the MSF track is reached.
		catalog
			.modify()
			.unwrap()
			.set_section("big", "x".repeat(moq_net::group::MAX_CACHE_BYTES as usize + 1).into())
			.unwrap();

		assert!(matches!(
			catalog.modify(),
			Err(crate::Error::Json(moq_json::Error::Net(moq_net::Error::FrameTooLarge)))
		));
		let waiter = kio::Waiter::noop();
		assert!(matches!(
			hang.poll_next_group(&waiter),
			Poll::Ready(Err(moq_net::Error::FrameTooLarge))
		));
		assert!(
			matches!(
				msf.poll_next_group(&waiter),
				Poll::Ready(Err(moq_net::Error::FrameTooLarge))
			),
			"a track the publish never reached must not outlive the others"
		);
	}

	#[test]
	fn a_failed_commit_leaves_the_catalog_open() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let config = Config::default().with_catalog(Catalog::<Extra>::default());
		let mut catalog = Producer::<Extra>::new(&mut broadcast, config).unwrap();

		let mut guard = catalog.modify().unwrap();
		guard
			.set_section("big", "x".repeat(moq_net::group::MAX_CACHE_BYTES as usize + 1).into())
			.unwrap();
		assert!(matches!(
			guard.commit(),
			Err(crate::Error::Json(moq_json::Error::Net(moq_net::Error::FrameTooLarge)))
		));

		// The caller got the error, so the tracks stay usable once the catalog fits again.
		catalog.modify().unwrap().remove_section("big").unwrap();
		catalog.finish().unwrap();
		assert_eq!(
			catalog.outputs.hang_track.largest(),
			Some(moq_net::track::Position::group(0))
		);
	}

	/// Every consumer refuses an ID naming both a video and an audio rendition, so the producer
	/// refuses to publish one.
	#[test]
	fn a_cross_kind_duplicate_id_is_refused() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = Producer::new(&mut broadcast, Config::default()).unwrap();

		let result = catalog.mutate(|catalog| {
			catalog.video.renditions.insert(
				"main".to_string(),
				hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8),
			);
			catalog.audio.renditions.insert(
				"main".to_string(),
				hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2),
			);
		});
		assert!(matches!(
			result,
			Err(crate::Error::Hang(hang::Error::DuplicateRendition(id))) if id == "main"
		));
		assert!(
			catalog.outputs.hang_track.largest().is_none(),
			"the refused catalog reached the wire"
		);
	}

	#[test]
	fn commit_publishes_once() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = Producer::new(&mut broadcast, Config::default()).unwrap();

		let mut guard = catalog.modify().unwrap();
		guard
			.audio
			.renditions
			.insert("audio0".to_string(), AudioConfig::new(AudioCodec::Opus, 48_000, 2));
		guard.commit().unwrap();

		// The drop that follows `commit` must not publish a second snapshot.
		catalog.finish().unwrap();
		assert_eq!(
			catalog.outputs.hang_track.largest(),
			Some(moq_net::track::Position::group(0))
		);
	}

	/// A video rendition `video0` on `catalog`, with a keyframe group at each of `starts` (ms).
	fn video0(
		broadcast: &mut moq_net::broadcast::Producer,
		catalog: &Producer,
		starts: &[u64],
	) -> crate::container::Producer<crate::catalog::hang::Container, VideoConfig> {
		let net = broadcast
			.create_track("video0", catalog.track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let mut video = catalog
			.video(
				net,
				crate::catalog::hang::Container::Legacy(crate::container::Kind::Video),
				VideoConfig::new(hang::catalog::VideoCodec::VP8),
			)
			.unwrap();
		for start in starts {
			video
				.write(crate::container::Frame {
					timestamp: moq_net::Timestamp::from_millis(*start).unwrap(),
					duration: None,
					payload: Bytes::from_static(b"keyframe"),
					keyframe: true,
				})
				.unwrap();
		}
		video
	}

	#[test]
	fn hls_reports_a_track_collision() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();

		// Something else already took the name the record track wants.
		let _taken = broadcast.create_track("video0.hls", None).unwrap();
		assert!(catalog.enroll("video0", None).is_err());
	}

	#[test]
	fn nothing_publishes_hls_when_disabled() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast, Config::default().with_hls(None)).unwrap();
		assert!(
			catalog
				.enroll("audio0", Some(std::time::Duration::from_secs(4)))
				.unwrap()
				.is_none()
		);

		let _video = video0(&mut broadcast, &catalog, &[0, 4_000, 8_000]);
		assert_eq!(catalog.snapshot().video.renditions["video0"].hls, None);
		// The name stays free for the application.
		assert!(broadcast.create_track("video0.hls", None).is_ok());
	}

	#[tokio::test]
	async fn an_estimated_duration_is_advertised_after_its_first_segment() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		let mut consumer = catalog.consume().unwrap();

		let mut video = video0(&mut broadcast, &catalog, &[0]);
		let initial = consumer.next().await.unwrap().unwrap();
		assert_eq!(initial.video.renditions["video0"].hls, None, "nothing declared it yet");

		video
			.write(crate::container::Frame {
				timestamp: moq_net::Timestamp::from_millis(4_000).unwrap(),
				duration: None,
				payload: Bytes::from_static(b"keyframe"),
				keyframe: true,
			})
			.unwrap();
		let declared = consumer.next().await.unwrap().unwrap();
		assert_eq!(
			declared.video.renditions["video0"].hls,
			Some(hang::catalog::Hls::new("video0.hls", std::time::Duration::from_secs(4)))
		);
	}

	#[test]
	fn a_track_finished_before_its_first_segment_is_advertised() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		let mut video = video0(&mut broadcast, &catalog, &[0]);

		video.finish().unwrap();
		let hls = catalog.snapshot().video.renditions["video0"].hls.clone();
		assert_eq!(hls.unwrap().duration, crate::hls::DEFAULT_DURATION);
	}

	#[test]
	fn a_track_hint_overrides_the_configured_one() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let config = Config::default()
			.with_hls(crate::hls::Config::default().with_duration_hint(std::time::Duration::from_secs(6)));
		let catalog = Producer::new(&mut broadcast, config).unwrap();
		let _video = catalog.enroll("video0", None).unwrap();
		let _audio = catalog
			.enroll("audio0", Some(std::time::Duration::from_secs(5)))
			.unwrap();

		let tracks = catalog.outputs.hls.as_ref().unwrap();
		assert_eq!(
			tracks.hls("video0").unwrap().duration,
			std::time::Duration::from_secs(6)
		);
		assert_eq!(
			tracks.hls("audio0").unwrap().duration,
			std::time::Duration::from_secs(5)
		);
	}

	#[test]
	fn only_the_enrolled_rendition_gets_its_records() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::<()>::new(&mut broadcast, Config::default()).unwrap();
		let _video = catalog
			.enroll("video", Some(std::time::Duration::from_secs(2)))
			.unwrap();

		// Same-named tracks of another broadcast, one carrying that broadcast's own records.
		let source = hang::catalog::Hls::new("video.hls", std::time::Duration::from_secs(6));
		let local = VideoConfig::new(hang::catalog::VideoCodec::VP8);
		let mut sibling = local.clone();
		sibling.broadcast = Some(moq_net::path::Relative::new("./other").unwrap());
		sibling.track = Some("video".into());
		sibling.hls = Some(source.clone());
		let mut unrecorded = sibling.clone();
		unrecorded.hls = None;
		let mut input = Catalog::<()>::default();
		input.video.renditions.insert("video".into(), local);
		input.video.renditions.insert("sibling".into(), sibling);
		input.video.renditions.insert("unrecorded".into(), unrecorded);

		let advertised = catalog.outputs.advertise(&input);
		let renditions = &advertised.video.renditions;
		let records = catalog.outputs.hls.as_ref().unwrap().hls("video");
		assert!(records.is_some());
		assert_eq!(renditions["video"].hls, records);
		assert_eq!(renditions["sibling"].hls, Some(source));
		assert_eq!(renditions["unrecorded"].hls, None);
	}

	/// A retired track's record track ends for the readers already on it and is forgotten, while
	/// one still recording is refused.
	#[test]
	fn retiring_a_track_ends_its_records() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		let recorder = catalog
			.enroll("video0", Some(std::time::Duration::from_secs(4)))
			.unwrap();
		let waiter = kio::Waiter::noop();
		let mut subscribing = broadcast
			.consume()
			.track("video0.hls")
			.unwrap()
			.subscribe(None)
			.into_inner();
		let std::task::Poll::Ready(Ok(mut reader)) = kio::Task::poll(&mut subscribing, &waiter) else {
			panic!("the record track is live");
		};
		assert!(catalog.retire("video0").is_err(), "its recorder is still alive");

		drop(recorder);
		catalog.retire("video0").unwrap();
		assert_eq!(catalog.hls_tracks(), 0);
		assert!(matches!(reader.poll_finished(&waiter), std::task::Poll::Ready(Ok(_))));
	}

	#[test]
	fn a_text_track_neither_records_nor_paces() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		let net = broadcast
			.create_track("text0", catalog.track_info(hang::catalog::PRIORITY.text))
			.unwrap();
		let _text = catalog
			.text(
				net,
				crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
				hang::catalog::TextConfig::new(hang::catalog::TextFormat::Vtt),
			)
			.unwrap();
		assert_eq!(catalog.hls_tracks(), 0);

		// The video enrolled after it paces its own segments.
		let _video = video0(&mut broadcast, &catalog, &[0, 4_000, 8_000]);
		assert_eq!(catalog.hls_tracks(), 1);
		let snapshot = catalog.snapshot();
		assert!(snapshot.video.renditions["video0"].hls.is_some());
	}

	#[test]
	fn an_undeclared_enrollment_releases_the_broadcast() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		let recorder = catalog.enroll("video0", None).unwrap();
		let waiter = kio::Waiter::noop();
		let mut subscribing = broadcast
			.consume()
			.track("video0.hls")
			.unwrap()
			.subscribe(None)
			.into_inner();
		let std::task::Poll::Ready(Ok(mut reader)) = kio::Task::poll(&mut subscribing, &waiter) else {
			panic!("the record track is live");
		};

		// Dropped before any frame, so it never declared a duration.
		drop((recorder, catalog, broadcast));
		assert!(matches!(reader.poll_finished(&waiter), std::task::Poll::Ready(_)));
	}

	#[test]
	fn clock_is_advertised_without_hls() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast, Config::default().with_hls(None)).unwrap();

		let snapshot = catalog.snapshot();
		assert_eq!(snapshot.clock, catalog.clock().wall());
	}

	#[test]
	fn sibling_broadcasts_share_a_clock_and_capture_time() {
		use std::time::Duration;

		let captured = moq_clock::now();
		let wall = moq_clock::Wall::UNIX_EPOCH + Duration::from_millis(hang::catalog::MOQ_EPOCH_UNIX_MILLIS + 60_000);
		let clock = crate::Clock::at(captured, wall).unwrap();
		let (origin, _driver) = moq_net::origin::Producer::new(Default::default());
		let front = origin.create_broadcast("robot/front.hang").unwrap();
		let rear = origin.create_broadcast("robot/rear.hang").unwrap();
		let catalogs = [
			Producer::new(&mut front.broadcast().clone(), Config::default().with_clock(clock)).unwrap(),
			Producer::new(&mut rear.broadcast().clone(), Config::default().with_clock(clock)).unwrap(),
		];
		let mut broadcasts = [front, rear];
		for (catalog, broadcast) in catalogs.iter().zip(&mut broadcasts) {
			assert_eq!(catalog.snapshot().clock, clock.wall());
			let timestamp = catalog.clock().capture(captured).unwrap();
			let track = broadcast
				.create_track("video", catalog.track_info(hang::catalog::PRIORITY.video))
				.unwrap();
			let mut video = catalog
				.video(
					track,
					crate::catalog::hang::Container::Legacy(crate::container::Kind::Video),
					VideoConfig::new(hang::catalog::VideoCodec::VP8),
				)
				.unwrap();
			let mut consumer = video.consume();
			video
				.write(crate::container::Frame {
					timestamp,
					duration: None,
					payload: Bytes::from_static(b"keyframe"),
					keyframe: true,
				})
				.unwrap();
			let waiter = kio::Waiter::noop();
			let Poll::Ready(Ok(Some(mut group))) = consumer.poll_recv_group(&waiter) else {
				panic!("captured group is published");
			};
			let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) else {
				panic!("captured frame is published");
			};
			let timestamp = frame.timestamp.expect("captured frame is timed");
			assert_eq!(catalog.clock().wall_clock(timestamp).unwrap(), wall);
			assert_eq!(catalog.snapshot().clock.unwrap().wall_clock(timestamp).unwrap(), wall);
		}
	}

	/// How far `pts` lands from `clock`'s reading now, which is zero for a frame live on arrival.
	fn lateness(clock: crate::Clock, pts: moq_net::Timestamp) -> std::time::Duration {
		let now = clock.now().as_micros();
		std::time::Duration::from_micros(now.abs_diff(pts.as_micros()) as u64)
	}

	/// A `with_clock` catalog is fixed from the start: even the first importer shifts onto it,
	/// landing at the clock's reading on arrival, and the advertised mapping never moves.
	#[moq_clock::test]
	async fn with_clock_shifts_the_first_importer() {
		use std::time::Duration;

		moq_clock::sleep(Duration::from_secs(5)).await;
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let wall = moq_clock::Wall::UNIX_EPOCH + Duration::from_millis(hang::catalog::MOQ_EPOCH_UNIX_MILLIS + 60_000);
		let start = moq_clock::now() - Duration::from_secs(5);
		let config = Config::default().with_clock(crate::Clock::at(start, wall).expect("a representable wall"));
		let catalog = Producer::new(&mut broadcast, config).unwrap();

		let advertised = Some(moq_net::Timestamp::from_micros(60_000_000).unwrap());
		assert_eq!(catalog.snapshot().clock.map(|clock| clock.wall), advertised);

		// The stream starts an hour in; the clock reads five seconds.
		let first = moq_net::Timestamp::from_micros(3_600_000_000).unwrap();
		let shifted = catalog.timebase().shift(first).unwrap();
		assert!(
			lateness(catalog.clock(), shifted) < Duration::from_secs(1),
			"{shifted:?}"
		);
		assert_eq!(catalog.snapshot().clock.map(|clock| clock.wall), advertised);
	}

	/// The first timebase to anchor on a clock nothing fixed places it, keeping its timestamps
	/// verbatim; its clones share that, while another timebase offsets onto the placed clock.
	#[test]
	fn the_first_input_places_the_clock() {
		use std::time::Duration;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		let timebase = catalog.timebase();

		let first = moq_net::Timestamp::from_micros(3_600_000_000).unwrap();
		assert_eq!(
			timebase.clone().shift(first).unwrap(),
			first,
			"the first timebase stays verbatim"
		);
		let clock = catalog.clock();
		assert_eq!(catalog.snapshot().clock, clock.wall(), "the anchor is advertised");
		let actual = clock.wall_clock(first).unwrap();
		let now = moq_clock::wall();
		let drift = actual.max(now).saturating_duration_since(actual.min(now));
		assert!(drift < Duration::from_secs(1), "the first timestamp is live on arrival");

		// Its clones keep the offset, whatever they see first.
		let later = moq_net::Timestamp::from_micros(3_601_000_000).unwrap();
		assert_eq!(timebase.shift(later).unwrap(), later);

		// A separate timebase starting at zero lands at now, and the clock stays.
		let other = catalog
			.timebase()
			.shift(moq_net::Timestamp::from_micros(0).unwrap())
			.unwrap();
		assert!(lateness(clock, other) < Duration::from_secs(1), "{other:?}");
		assert_eq!(
			catalog.clock().wall(),
			clock.wall(),
			"a second timebase never moves the clock"
		);
	}

	/// Taking the clock fixes it, so a copy handed to a capture never goes stale.
	#[test]
	fn taking_the_clock_fixes_it() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		let clock = catalog.clock();

		let shifted = catalog
			.timebase()
			.shift(moq_net::Timestamp::from_micros(3_600_000_000).unwrap())
			.unwrap();
		assert_eq!(catalog.clock().wall(), clock.wall());
		assert!(
			lateness(clock, shifted) < std::time::Duration::from_secs(1),
			"{shifted:?}"
		);
	}

	/// The first publish fixes the clock too: a snapshot on the wire is a mapping a reader may have
	/// copied. A stream at PTS zero would otherwise step it back ten seconds.
	#[test]
	fn the_first_publish_fixes_the_clock() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		catalog
			.mutate(|c| {
				c.audio
					.renditions
					.insert("audio0".to_string(), AudioConfig::new(AudioCodec::Opus, 48_000, 2));
			})
			.unwrap();
		let published = catalog.snapshot().clock;

		let shifted = catalog
			.timebase()
			.shift(moq_net::Timestamp::from_micros(0).unwrap())
			.unwrap();
		assert_eq!(catalog.snapshot().clock, published);
		assert!(
			shifted.as_micros() >= 10_000_000,
			"{shifted:?} lands at the clock's reading"
		);
	}

	/// A timebase that knows when its timestamps happened places them there, on a clock it places
	/// or one already fixed alike, so two importers of one stream publish identical timestamps.
	#[test]
	fn a_placed_input_lands_where_it_says() {
		use std::time::Duration;

		let wall = moq_clock::Wall::now() - Duration::from_secs(2);
		let pts = moq_net::Timestamp::from_micros(100_000_000).unwrap();
		let later = moq_net::Timestamp::from_micros(101_000_000).unwrap();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		let timebase = catalog.timebase();
		timebase.place(pts, wall).unwrap();
		assert_eq!(
			timebase.shift(later).unwrap(),
			later,
			"it placed the clock, so it stays verbatim"
		);
		let placed = catalog.clock();
		assert_eq!(placed.reading(wall).unwrap(), pts.as_micros() as i128);

		// Placed again on the now fixed clock, the same mapping shifts by nothing.
		let again = catalog.timebase();
		again.place(pts, wall).unwrap();
		assert_eq!(again.shift(later).unwrap(), later);
		assert_eq!(catalog.clock().wall(), placed.wall());

		// A timebase that already anchored keeps its offset.
		assert!(matches!(
			timebase.place(pts, wall),
			Err(crate::Error::UnmappableTimestamp(_))
		));
	}

	/// A placement lists no track, so alone it publishes nothing: an importer placed before its
	/// input declares a track has no catalog to announce until it does, and that catalog carries
	/// the placed clock.
	#[test]
	fn a_placement_alone_publishes_nothing() {
		use std::time::Duration;

		let wall = moq_clock::Wall::now() - Duration::from_secs(2);
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		catalog.timebase().place(moq_net::Timestamp::ZERO, wall).unwrap();
		assert_eq!(sent(&catalog), None, "a clock alone reached the wire");

		catalog
			.mutate(|catalog| {
				catalog
					.audio
					.renditions
					.insert("audio0".to_string(), AudioConfig::new(AudioCodec::Opus, 48_000, 2));
			})
			.unwrap();
		assert_eq!(sent(&catalog), Some(0));
		assert_eq!(catalog.clock().reading(wall).unwrap(), 0);
		assert_eq!(wire(&catalog).clock, catalog.clock().wall());
	}

	/// Fake this thread's host now a day past the moq epoch, returning a root `clock` an hour
	/// before it in `scale` units.
	fn an_hour_ago(scale: moq_net::Timescale) -> hang::catalog::Clock {
		use std::time::Duration;

		let now = Duration::from_secs(86_400);
		crate::clock::fake_host_wall(
			moq_clock::Wall::UNIX_EPOCH + Duration::from_millis(hang::catalog::MOQ_EPOCH_UNIX_MILLIS) + now,
		);
		let zero = moq_net::Timestamp::from_secs(now.as_secs() - 3600).unwrap();
		hang::catalog::Clock::new(zero.convert(scale).unwrap()).unwrap()
	}

	/// A derivative writes its input's mapping through `modify()` before anything fixes the clock,
	/// and the producer's clock re-bases on it: what it stamps lands under the input's mapping.
	#[tokio::test(start_paused = true)]
	async fn a_root_clock_written_before_the_clock_is_fixed_rebases_it() {
		let input = an_hour_ago(moq_net::Timescale::MICRO);
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		catalog.mutate(|c| c.clock = Some(input)).unwrap();

		assert_eq!(catalog.snapshot().clock, Some(input), "advertised unchanged");
		let clock = catalog.clock();
		assert_eq!(clock.wall(), Some(input));
		assert_eq!(
			clock.now(),
			moq_net::Timestamp::from_secs(3600)
				.unwrap()
				.convert(crate::Clock::TIMESCALE)
				.unwrap()
		);

		tokio::time::advance(std::time::Duration::from_secs(3)).await;
		assert_eq!(
			catalog.timestamp(None).unwrap(),
			moq_net::Timestamp::from_micros(3_603_000_000).unwrap()
		);

		// Re-applying the same mapping once fixed changes nothing.
		catalog.mutate(|c| c.clock = Some(input)).unwrap();
		assert_eq!(catalog.clock(), clock);
	}

	/// A root `clock` seeded through `with_catalog` is adopted like one written through `modify()`,
	/// never replaced by a fresh clock; one that disagrees with `with_clock` is refused.
	#[test]
	fn a_seeded_root_clock_is_adopted() {
		use std::time::Duration;

		let input = an_hour_ago(moq_net::Timescale::MICRO);
		let mut seed = Catalog::<()>::default();
		seed.clock = Some(input);
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast, Config::default().with_catalog(seed.clone())).unwrap();
		assert_eq!(catalog.snapshot().clock, Some(input));
		assert_eq!(catalog.clock().wall(), Some(input));

		// The same mapping from `with_clock` agrees in any units, without reading the host's wall
		// clock, which may be unset.
		let zero = moq_clock::Wall::UNIX_EPOCH
			+ Duration::from_millis(hang::catalog::MOQ_EPOCH_UNIX_MILLIS)
			+ Duration::from_micros(input.wall.as_micros() as u64);
		let fixed = crate::Clock::at(moq_clock::now(), zero).unwrap();
		seed.clock = Some(an_hour_ago(moq_net::Timescale::MILLI));
		crate::clock::fake_host_wall(moq_clock::Wall::UNIX_EPOCH + Duration::from_secs(86_400));
		let config = Config::default().with_catalog(seed.clone()).with_clock(fixed);
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = Producer::new(&mut broadcast, config).unwrap();
		assert_eq!(catalog.clock(), fixed);
		catalog.mutate(|c| c.clock = seed.clock).unwrap();
		assert_eq!(catalog.snapshot().clock, Some(input));

		// A different one is refused before any catalog track exists.
		let other = crate::Clock::at(moq_clock::now(), zero + Duration::from_secs(1)).unwrap();
		let config = Config::default().with_catalog(seed).with_clock(other);
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		assert!(matches!(
			Producer::new(&mut broadcast, config),
			Err(crate::Error::UnmappableTimestamp(_))
		));
		broadcast
			.create_track(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info())
			.expect("a refused seed leaves the broadcast untouched");
	}

	/// A millisecond wall is the same mapping in the clock's microseconds, advertised as such.
	#[test]
	fn a_millisecond_root_clock_converts() {
		let input = an_hour_ago(moq_net::Timescale::MILLI);
		let micro = an_hour_ago(moq_net::Timescale::MICRO);
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		catalog.mutate(|c| c.clock = Some(input)).unwrap();
		assert_eq!(catalog.snapshot().clock, Some(micro));
		assert_eq!(catalog.clock().wall(), Some(micro));

		// The millisecond spelling of the fixed mapping is no change.
		catalog.mutate(|c| c.clock = Some(input)).unwrap();
		assert_eq!(catalog.snapshot().clock, Some(micro));
	}

	/// A root `clock` the producer cannot run on is refused, and the catalog keeps advertising the
	/// producer's own.
	#[test]
	fn an_unadoptable_root_clock_is_refused() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		let own = catalog.current_clock().wall();
		let refused = |catalog: &mut Producer, clock: Option<hang::catalog::Clock>| {
			let err = catalog.mutate(|c| c.clock = clock).unwrap_err();
			assert!(matches!(err, crate::Error::UnmappableTimestamp(_)), "{err}");
			assert_eq!(catalog.snapshot().clock, own);
		};

		// One 90 kHz tick is no whole microsecond.
		let inexact = an_hour_ago(moq_net::Timescale::MICRO)
			.wall
			.convert(moq_net::Timescale::new(90_000).unwrap());
		let inexact = moq_net::Timestamp::new(inexact.unwrap().value() + 1, moq_net::Timescale::new(90_000).unwrap());
		refused(&mut catalog, Some(hang::catalog::Clock::new(inexact.unwrap()).unwrap()));

		// PTS zero a minute after this host's now.
		let future = an_hour_ago(moq_net::Timescale::SECOND).wall.value() + 3660;
		let future = hang::catalog::Clock::new(moq_net::Timestamp::from_secs(future).unwrap()).unwrap();
		refused(&mut catalog, Some(future));

		// Clearing it, even before anything fixed it.
		refused(&mut catalog, None);
		assert_eq!(catalog.current_clock().wall(), own, "nothing re-based");

		// A change once the clock is handed out.
		let taken = catalog.clock();
		refused(&mut catalog, Some(an_hour_ago(moq_net::Timescale::MICRO)));
		assert_eq!(catalog.clock(), taken);
	}

	/// A host whose wall clock reads before 2020 (booted before NTP sync) publishes its media on the
	/// monotonic timeline with no root `clock`, rather than advertise a false one. Placing a source's
	/// wall time is refused, whether it would place the clock or offset onto it.
	#[test]
	fn an_unset_host_clock_publishes_without_a_wall() {
		use std::time::Duration;

		crate::clock::fake_host_wall(moq_clock::Wall::UNIX_EPOCH + Duration::from_secs(86_400));
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		assert_eq!(catalog.snapshot().clock, None);

		let wall = moq_clock::Wall::UNIX_EPOCH + Duration::from_millis(hang::catalog::MOQ_EPOCH_UNIX_MILLIS);
		let pts = moq_net::Timestamp::from_micros(1_000_000).unwrap();
		assert!(matches!(
			catalog.timebase().place(pts, wall),
			Err(crate::Error::UnmappableTimestamp(_))
		));

		let net = broadcast
			.create_track("video0", catalog.track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let mut video = catalog
			.video(
				net,
				crate::catalog::hang::Container::Legacy(crate::container::Kind::Video),
				VideoConfig::new(hang::catalog::VideoCodec::VP8),
			)
			.unwrap();
		let mut subscriber = video.consume();
		let timestamp = catalog.timestamp(None).unwrap();
		video
			.write(crate::container::Frame {
				timestamp,
				duration: None,
				payload: Bytes::from_static(b"keyframe"),
				keyframe: true,
			})
			.unwrap();

		let snapshot = catalog.snapshot();
		assert!(snapshot.video.renditions.contains_key("video0"));
		assert_eq!(snapshot.clock, None, "publishing never invents a wall");
		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(Some(mut group))) = subscriber.poll_recv_group(&waiter) else {
			panic!("the frame's group is published");
		};
		let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) else {
			panic!("the frame is published");
		};
		assert_eq!(frame.timestamp, Some(timestamp));

		// The catalog clock is fixed now, and has no wall to offset a placement onto.
		assert!(matches!(
			catalog.timebase().place(pts, wall),
			Err(crate::Error::UnmappableTimestamp(_))
		));
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
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		let mut consumer: Consumer = Consumer::new(catalog.outputs.hang.consume());
		let waiter = kio::Waiter::noop();

		let reserved = catalog.reserve();
		let mut audio = reserved.init::<AudioConfig>("audio0").unwrap();
		let mut video = reserved.init::<VideoConfig>("video0").unwrap();
		drop(reserved); // done reserving; both renditions still outstanding

		// Audio resolves first: withheld, because video is still outstanding.
		audio.set(AudioConfig::new(AudioCodec::Opus, 48_000, 2)).unwrap();
		assert!(
			matches!(consumer.poll_next(&waiter), Poll::Pending),
			"an audio-only catalog must not publish while video is unresolved"
		);

		// Video resolves: the complete catalog publishes now, in one snapshot.
		video.set(h264_config()).unwrap();
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

	#[test]
	fn live_rendition_owns_its_name_without_gating_the_catalog() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		let mut consumer: Consumer = Consumer::new(catalog.outputs.hang.consume());
		let waiter = kio::Waiter::noop();

		let _unresolved = catalog.rendition::<VideoConfig>("video0").unwrap();
		assert!(
			catalog.rendition::<VideoConfig>("video0").is_err(),
			"the unresolved live rendition owns its name"
		);
		let mut audio = catalog.rendition::<AudioConfig>("audio0").unwrap();
		audio.set(AudioConfig::new(AudioCodec::Opus, 48_000, 2)).unwrap();

		let snapshot = match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(c))) => c,
			other => panic!("expected the incremental audio catalog, got {other:?}"),
		};
		assert!(snapshot.audio.renditions.contains_key("audio0"));
		assert!(!snapshot.video.renditions.contains_key("video0"));
	}

	#[test]
	fn metadata_alone_does_not_publish_the_initial_catalog() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let config = Config::default().with_catalog(Catalog::<Extra>::default());
		let mut catalog = Producer::new(&mut broadcast, config).unwrap();
		let mut consumer: Consumer<Extra> = Consumer::new(catalog.outputs.hang.consume());
		let waiter = kio::Waiter::noop();
		let reserved = catalog.reserve();

		catalog
			.modify()
			.unwrap()
			.set_section("mpegts", serde_json::json!({ "programs": [1] }))
			.unwrap();
		assert!(
			matches!(consumer.poll_next(&waiter), Poll::Pending),
			"extension metadata must not publish an empty broadcast"
		);
		drop(reserved);
		assert!(
			matches!(consumer.poll_next(&waiter), Poll::Pending),
			"releasing the importer must not publish metadata without a track"
		);

		catalog
			.modify()
			.unwrap()
			.audio
			.renditions
			.insert("audio0".to_string(), AudioConfig::new(AudioCodec::Opus, 48_000, 2));
		let snapshot = match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(c))) => c,
			other => panic!("expected a catalog naming the track, got {other:?}"),
		};
		assert!(snapshot.audio.renditions.contains_key("audio0"));
		assert!(snapshot.ext.get("mpegts").is_some());
	}

	/// A passthrough lists its multiplex as `m2ts`, not as a rendition, and that is a track.
	#[test]
	fn m2ts_counts_as_a_servable_track() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		let mut consumer: Consumer = Consumer::new(catalog.outputs.hang.consume());
		let waiter = kio::Waiter::noop();

		let reserved = catalog.reserve();
		catalog
			.mutate(|catalog| catalog.m2ts = Some(hang::catalog::M2ts::new("input.m2ts")))
			.unwrap();
		drop(reserved);
		let snapshot = match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(c))) => c,
			other => panic!("expected the passthrough catalog, got {other:?}"),
		};
		assert_eq!(snapshot.m2ts.unwrap().track, "input.m2ts");
	}

	/// A metadata edit after the last reservation released is still an initial publish, so it waits
	/// for a track too.
	#[test]
	fn metadata_after_release_does_not_publish_the_initial_catalog() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let config = Config::default().with_catalog(Catalog::<Extra>::default());
		let mut catalog = Producer::new(&mut broadcast, config).unwrap();
		let mut consumer: Consumer<Extra> = Consumer::new(catalog.outputs.hang.consume());
		let waiter = kio::Waiter::noop();

		let reserved = catalog.reserve();
		catalog
			.modify()
			.unwrap()
			.set_section("mpegts", serde_json::json!({ "programs": [1] }))
			.unwrap();
		drop(reserved);
		catalog
			.modify()
			.unwrap()
			.set_section("mpegts", serde_json::json!({ "programs": [1], "muxRate": 1 }))
			.unwrap();
		assert!(
			matches!(consumer.poll_next(&waiter), Poll::Pending),
			"a later metadata edit must not publish an empty broadcast"
		);

		catalog
			.modify()
			.unwrap()
			.audio
			.renditions
			.insert("audio0".to_string(), AudioConfig::new(AudioCodec::Opus, 48_000, 2));
		let snapshot = match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(c))) => c,
			other => panic!("expected a catalog naming the track, got {other:?}"),
		};
		assert!(snapshot.audio.renditions.contains_key("audio0"));
	}

	/// A live rendition holds its name before its config arrives, which lists no track yet.
	#[test]
	fn an_unconfigured_rendition_does_not_publish_the_initial_catalog() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let config = Config::default().with_catalog(Catalog::<Extra>::default());
		let mut catalog = Producer::new(&mut broadcast, config).unwrap();
		let mut consumer: Consumer<Extra> = Consumer::new(catalog.outputs.hang.consume());
		let waiter = kio::Waiter::noop();

		let net = broadcast
			.create_track("video0", catalog.track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let mut video = catalog
			.video(
				net,
				crate::catalog::hang::Container::Legacy(crate::container::Kind::Video),
				None,
			)
			.unwrap();
		let reserved = catalog.reserve();
		catalog
			.modify()
			.unwrap()
			.set_section("mpegts", serde_json::json!({ "programs": [1] }))
			.unwrap();
		drop(reserved);
		assert!(
			matches!(consumer.poll_next(&waiter), Poll::Pending),
			"an unconfigured rendition is not a listed track"
		);

		video.set(VideoConfig::new(hang::catalog::VideoCodec::VP8)).unwrap();
		let snapshot = match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(c))) => c,
			other => panic!("expected a catalog naming the track, got {other:?}"),
		};
		assert!(snapshot.video.renditions.contains_key("video0"));
	}

	/// A disabled rendition promises no frames, so it lists no track until it is enabled.
	#[test]
	fn a_disabled_rendition_does_not_publish_the_initial_catalog() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let config = Config::default().with_catalog(Catalog::<Extra>::default());
		let catalog = Producer::new(&mut broadcast, config).unwrap();
		let mut consumer: Consumer<Extra> = Consumer::new(catalog.outputs.hang.consume());
		let waiter = kio::Waiter::noop();

		let net = broadcast
			.create_track("audio0", catalog.track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let mut audio = catalog
			.audio(
				net,
				crate::catalog::hang::Container::Legacy(crate::container::Kind::Audio),
				None,
			)
			.unwrap();
		let reserved = catalog.reserve();
		let mut disabled = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		disabled.enabled = false;
		audio.set(disabled.clone()).unwrap();
		drop(reserved);
		assert!(
			matches!(consumer.poll_next(&waiter), Poll::Pending),
			"a disabled rendition is not a servable track"
		);

		disabled.enabled = true;
		audio.set(disabled).unwrap();
		let snapshot = match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(c))) => c,
			other => panic!("expected a catalog naming the track, got {other:?}"),
		};
		assert!(snapshot.audio.renditions["audio0"].enabled);
	}

	// Dropping a reservation without fulfilling it (a stream that never produced a config) still opens
	// the gate, publishing whatever did resolve.
	#[test]
	fn reservation_gate_opens_when_unresolved_reservation_is_dropped() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		let mut consumer: Consumer = Consumer::new(catalog.outputs.hang.consume());
		let waiter = kio::Waiter::noop();

		let reserved = catalog.reserve();
		let mut audio = reserved.init::<AudioConfig>("audio0").unwrap();
		let video = reserved.init::<VideoConfig>("video0").unwrap();
		drop(reserved);

		audio.set(AudioConfig::new(AudioCodec::Opus, 48_000, 2)).unwrap();
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
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		let mut consumer: Consumer = Consumer::new(catalog.outputs.hang.consume());
		let waiter = kio::Waiter::noop();

		// A deferred importer grabs a reservation up front and holds it until it can resolve.
		let deferred = catalog.reserve();

		// Meanwhile an eager rendition resolves and stages a catalog change. The importer keeps its
		// rendition alive (dropping it would retire the track), so bind it.
		let early = catalog.reserve();
		let mut a0 = early.init::<AudioConfig>("audio0").unwrap();
		a0.set(AudioConfig::new(AudioCodec::Opus, 48_000, 2)).unwrap();
		drop(early);
		assert!(
			matches!(consumer.poll_next(&waiter), Poll::Pending),
			"the staged rendition must wait for the deferred importer's held reservation"
		);

		// The deferred importer finally builds its rendition and resolves it.
		let mut late = deferred.init::<AudioConfig>("audio1").unwrap();
		drop(deferred); // the importer releases its own hold; only the rendition's remains
		assert!(matches!(consumer.poll_next(&waiter), Poll::Pending));
		late.set(AudioConfig::new(AudioCodec::Opus, 48_000, 1)).unwrap();

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

		let msf = to_msf_media::<()>(&catalog, &moq_net::Path::empty());

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

	// A LOC rendition must advertise `packaging: loc`. MSF-01 has no legacy packaging, so falling into
	// the legacy arm published a catalog that named a container the receiver would not find on the wire.
	#[test]
	fn convert_video_loc_container() {
		let mut video_config = VideoConfig::new(H264 {
			profile: 0x64,
			constraints: 0x00,
			level: 0x1f,
			inline: true,
		});
		video_config.container = Container::Loc;

		let mut video_renditions = BTreeMap::new();
		video_renditions.insert("video0.avc3".to_string(), video_config);

		let mut catalog = hang::Catalog::default();
		catalog.video.renditions = video_renditions;

		let msf = to_msf_media::<()>(&catalog, &moq_net::Path::empty());
		assert_eq!(msf.tracks.len(), 1);
		assert_eq!(msf.tracks[0].packaging, moq_msf::Packaging::Loc);
	}

	#[test]
	fn convert_audio_loc_container() {
		let mut audio_config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		audio_config.container = Container::Loc;

		let mut audio_renditions = BTreeMap::new();
		audio_renditions.insert("audio0".to_string(), audio_config);

		let mut catalog = hang::Catalog::default();
		catalog.audio.renditions = audio_renditions;

		let msf = to_msf_media::<()>(&catalog, &moq_net::Path::empty());
		assert_eq!(msf.tracks.len(), 1);
		assert_eq!(msf.tracks[0].packaging, moq_msf::Packaging::Loc);
	}

	// An unrecognized container keeps its wire kind instead of posing as legacy: hang says such a
	// rendition must be ignored, and no MSF consumer claims an unknown packaging.
	#[test]
	fn convert_unknown_container_keeps_its_kind() {
		let container: Container = serde_json::from_value(serde_json::json!({ "kind": "future" })).unwrap();
		assert!(matches!(container, Container::Unknown(_)));

		let mut audio_config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		audio_config.container = container;

		let mut audio_renditions = BTreeMap::new();
		audio_renditions.insert("audio0".to_string(), audio_config);

		let mut catalog = hang::Catalog::default();
		catalog.audio.renditions = audio_renditions;

		let msf = to_msf_media::<()>(&catalog, &moq_net::Path::empty());
		assert_eq!(msf.tracks.len(), 1);
		assert_eq!(
			msf.tracks[0].packaging,
			moq_msf::Packaging::Unknown("future".to_string())
		);
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

		let msf = to_msf_media::<()>(&catalog, &moq_net::Path::empty());
		let video = &msf.tracks[0];
		assert_eq!(video.init_data, Some("AQID".to_string()));
	}

	#[test]
	fn convert_empty() {
		let catalog = hang::Catalog::default();
		let msf = to_msf_media::<()>(&catalog, &moq_net::Path::empty());
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

		let msf = to_msf_media::<()>(&catalog, &moq_net::Path::empty());
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
		video_config.delay = Some(std::time::Duration::from_millis(200));

		let mut video_renditions = BTreeMap::new();
		video_renditions.insert("video0".to_string(), video_config);

		let mut audio_config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		audio_config.container = Container::Legacy;
		audio_config.jitter = Some(std::time::Duration::from_millis(40));
		audio_config.delay = Some(std::time::Duration::from_millis(80));

		let mut audio_renditions = BTreeMap::new();
		audio_renditions.insert("audio0".to_string(), audio_config);

		let mut catalog = hang::Catalog::default();
		catalog.video.renditions = video_renditions;
		catalog.audio.renditions = audio_renditions;

		let msf = to_msf_media::<()>(&catalog, &moq_net::Path::empty());

		let video = &msf.tracks[0];
		assert_eq!(video.role, Some(moq_msf::Role::Video));
		// H.264 may carry B-frames, so SAP starting type is 2.
		assert_eq!(video.max_grp_sap_starting_type, Some(2));
		assert_eq!(video.max_obj_sap_starting_type, Some(2));
		assert_eq!(video.jitter, Some(std::time::Duration::from_millis(100)));
		assert_eq!(video.delay, Some(std::time::Duration::from_millis(200)));

		let audio = &msf.tracks[1];
		assert_eq!(audio.role, Some(moq_msf::Role::Audio));
		assert_eq!(audio.max_grp_sap_starting_type, Some(1));
		assert_eq!(audio.max_obj_sap_starting_type, Some(1));
		assert_eq!(audio.jitter, Some(std::time::Duration::from_millis(40)));
		assert_eq!(audio.delay, Some(std::time::Duration::from_millis(80)));
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

		let msf = to_msf_media::<()>(&catalog, &moq_net::Path::empty());
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

		let msf = to_msf_media::<()>(&catalog, &moq_net::Path::empty());
		let video = &msf.tracks[0];
		assert_eq!(video.max_grp_sap_starting_type, None);
		assert_eq!(video.max_obj_sap_starting_type, None);
	}

	/// A catalog with live video renditions named `names`, each already set.
	fn estimating(names: &[&str]) -> (moq_net::broadcast::Producer, Producer, Vec<super::super::VideoTrack>) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		let reserved = catalog.reserve();
		let renditions = names
			.iter()
			.map(|name| {
				let mut rendition = reserved.init::<VideoConfig>(*name).unwrap();
				rendition.set(VideoConfig::new(hang::catalog::VideoCodec::VP8)).unwrap();
				rendition
			})
			.collect();
		drop(reserved);
		(broadcast, catalog, renditions)
	}

	fn jitter(ms: u64) -> super::super::Estimate {
		super::super::Estimate::default().with_jitter(std::time::Duration::from_millis(ms))
	}

	/// How many catalogs have gone out on the wire.
	fn sent(catalog: &Producer) -> Option<u64> {
		catalog.outputs.hang_track.largest().map(|position| position.group)
	}

	/// The catalog a viewer joining now receives.
	fn wire(catalog: &Producer) -> Catalog<()> {
		let mut consumer: Consumer = Consumer::new(catalog.outputs.hang.consume());
		match consumer.poll_next(&kio::Waiter::noop()) {
			Poll::Ready(Ok(Some(catalog))) => catalog,
			_ => panic!("a published catalog"),
		}
	}

	fn wire_jitter(catalog: &Producer, name: &str) -> Option<std::time::Duration> {
		wire(catalog).video.renditions[name].jitter
	}

	/// A rising estimate publishes at once, then coalesces until an observation after the window,
	/// which carries the latest value. Regression: every rise republished the catalog.
	#[tokio::test(start_paused = true)]
	async fn estimate_rises_publish_at_the_leading_edge_and_coalesce() {
		let (_broadcast, catalog, mut renditions) = estimating(&["v"]);
		let v = &mut renditions[0];
		let initial = sent(&catalog);

		v.estimate(jitter(1)).unwrap();
		let leading = sent(&catalog);
		assert_ne!(leading, initial, "the first rise publishes at once");
		assert_eq!(wire_jitter(&catalog, "v"), Some(std::time::Duration::from_millis(1)));

		for ms in 2..=17 {
			tokio::time::advance(std::time::Duration::from_millis(50)).await;
			v.estimate(jitter(ms)).unwrap();
		}
		assert_eq!(sent(&catalog), leading, "rises inside the window are held");
		assert_eq!(
			catalog.snapshot().video.renditions["v"].jitter,
			Some(std::time::Duration::from_millis(17)),
			"the held value is already in the catalog"
		);

		// No timer: the window ending publishes nothing until the next observation.
		tokio::time::advance(std::time::Duration::from_millis(200)).await;
		assert_eq!(sent(&catalog), leading);
		v.estimate(jitter(17)).unwrap();
		let trailing = sent(&catalog);
		assert_ne!(trailing, leading, "an unchanged observation releases the held rise");
		assert_eq!(wire_jitter(&catalog, "v"), Some(std::time::Duration::from_millis(17)));

		// Nothing is held, so later observations publish nothing.
		tokio::time::advance(std::time::Duration::from_secs(2)).await;
		v.estimate(jitter(17)).unwrap();
		assert_eq!(sent(&catalog), trailing);

		// A rise a window after the trailing publish is a new leading edge.
		v.estimate(jitter(18)).unwrap();
		assert_ne!(sent(&catalog), trailing);
		assert_eq!(wire_jitter(&catalog, "v"), Some(std::time::Duration::from_millis(18)));
	}

	/// A structural edit never waits, carries the held estimate, and opens a new window.
	#[tokio::test(start_paused = true)]
	async fn structural_edit_folds_a_held_estimate() {
		let (_broadcast, mut catalog, mut renditions) = estimating(&["v"]);
		let v = &mut renditions[0];
		v.estimate(jitter(1)).unwrap();
		tokio::time::advance(std::time::Duration::from_millis(100)).await;
		v.estimate(jitter(2)).unwrap();
		let held = sent(&catalog);

		catalog
			.mutate(|catalog| {
				catalog.video.display = Some(hang::catalog::Display {
					width: 1280,
					height: 720,
				})
			})
			.unwrap();
		let folded = sent(&catalog);
		assert_ne!(folded, held, "a structural edit publishes at once");
		assert_eq!(wire_jitter(&catalog, "v"), Some(std::time::Duration::from_millis(2)));

		// The fold opened a window: the next rise inside it is held, and nothing is left to flush.
		tokio::time::advance(std::time::Duration::from_millis(950)).await;
		v.estimate(jitter(3)).unwrap();
		assert_eq!(sent(&catalog), folded);
		tokio::time::advance(std::time::Duration::from_millis(50)).await;
		v.estimate(jitter(3)).unwrap();
		assert_ne!(sent(&catalog), folded);
		assert_eq!(wire_jitter(&catalog, "v"), Some(std::time::Duration::from_millis(3)));
	}

	/// The window is the catalog's, so a delay rise on one track waits behind a jitter rise on
	/// another, and any track's next observation releases it.
	#[tokio::test(start_paused = true)]
	async fn estimate_window_is_shared_across_renditions() {
		let (_broadcast, catalog, mut renditions) = estimating(&["a", "b"]);
		let [a, b] = renditions.as_mut_slice() else {
			unreachable!()
		};
		a.estimate(jitter(1)).unwrap();
		let leading = sent(&catalog);
		b.estimate(super::super::Estimate::default().with_delay(std::time::Duration::from_millis(7)))
			.unwrap();
		assert_eq!(sent(&catalog), leading);

		tokio::time::advance(std::time::Duration::from_secs(1)).await;
		a.estimate(jitter(1)).unwrap();
		assert_ne!(sent(&catalog), leading);
		let snapshot = wire(&catalog);
		assert_eq!(
			snapshot.video.renditions["b"].delay,
			Some(std::time::Duration::from_millis(7))
		);
	}

	/// Bitrate and framerate are already measured per second, so a new value never waits, but it
	/// still opens a window for the jitter it carries.
	#[tokio::test(start_paused = true)]
	async fn a_bitrate_estimate_publishes_immediately() {
		let (_broadcast, catalog, mut renditions) = estimating(&["v"]);
		let v = &mut renditions[0];
		v.estimate(jitter(1).with_bitrate(1_000_000)).unwrap();
		let first = sent(&catalog);
		v.estimate(jitter(2).with_bitrate(1_000_000)).unwrap();
		assert_eq!(sent(&catalog), first, "the bitrate publish opened a window");

		tokio::time::advance(std::time::Duration::from_millis(10)).await;
		v.estimate(jitter(3).with_bitrate(2_000_000)).unwrap();
		assert_ne!(sent(&catalog), first);
		let snapshot = wire(&catalog);
		assert_eq!(snapshot.video.renditions["v"].bitrate, Some(2_000_000));
		assert_eq!(
			snapshot.video.renditions["v"].jitter,
			Some(std::time::Duration::from_millis(3))
		);
	}

	/// A rise while the initial catalog is reserved is held by the gate, goes out with the first
	/// snapshot, and that snapshot opens the window.
	#[tokio::test(start_paused = true)]
	async fn estimate_rise_waits_for_the_reservation_gate() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		let reserved = catalog.reserve();
		let mut v = reserved.init::<VideoConfig>("v").unwrap();
		v.set(VideoConfig::new(hang::catalog::VideoCodec::VP8)).unwrap();
		v.estimate(jitter(1)).unwrap();
		assert_eq!(sent(&catalog), None, "the gate withholds the rise");

		drop(reserved);
		assert_eq!(sent(&catalog), Some(0));
		assert_eq!(wire_jitter(&catalog, "v"), Some(std::time::Duration::from_millis(1)));
		v.estimate(jitter(2)).unwrap();
		assert_eq!(sent(&catalog), Some(0), "the gated publish opened a window");
	}

	fn delay(ms: u64) -> super::super::Reading {
		super::super::Reading {
			estimate: super::super::Estimate::default().with_delay(std::time::Duration::from_millis(ms)),
			period: Some(std::time::Duration::from_millis(10)),
		}
	}

	/// A measured delay creeping a millisecond at a time republishes once per frame duration it
	/// crosses, carrying the value measured then. Regression: every rise republished, once a second.
	#[tokio::test(start_paused = true)]
	async fn a_creeping_delay_republishes_once_per_frame() {
		let (_broadcast, catalog, mut renditions) = estimating(&["v"]);
		let v = &mut renditions[0];
		let initial = sent(&catalog).unwrap();

		let mut published = Vec::new();
		for ms in 1..=35 {
			// Past the once-a-second window, so only the frame step holds a rise back.
			tokio::time::advance(std::time::Duration::from_secs(2)).await;
			let before = sent(&catalog);
			v.estimate(delay(ms)).unwrap();
			if sent(&catalog) != before {
				published.push(wire(&catalog).video.renditions["v"].delay.unwrap().as_millis());
			}
		}
		assert_eq!(published, [10, 20, 30]);
		assert_eq!(sent(&catalog), Some(initial + 3));
		assert_eq!(
			catalog.snapshot().video.renditions["v"].delay,
			Some(std::time::Duration::from_millis(30)),
			"a rise short of a frame is not even staged"
		);
	}

	/// A known framerate sets the step, whatever shorter advance the timestamps show.
	#[tokio::test(start_paused = true)]
	async fn a_framerate_sets_the_delay_step() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		let reserved = catalog.reserve();
		let mut v = reserved.init::<VideoConfig>("v").unwrap();
		let mut config = VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.framerate = Some(25.0);
		v.set(config).unwrap();
		drop(reserved);

		v.estimate(delay(39)).unwrap();
		assert_eq!(wire(&catalog).video.renditions["v"].delay, None);
		v.estimate(delay(40)).unwrap();
		assert_eq!(
			wire(&catalog).video.renditions["v"].delay,
			Some(std::time::Duration::from_millis(40))
		);
	}

	/// How many MSF catalogs have gone out on the wire.
	fn msf_sent(catalog: &Producer) -> Option<u64> {
		catalog.outputs.msf_track.largest().map(|position| position.group)
	}

	/// The last MSF catalog on the wire.
	fn msf_wire(catalog: &Producer) -> moq_msf::Catalog<()> {
		let state = take(&catalog.current);
		moq_msf::Catalog::from_str(state.msf.last.as_deref().expect("a published MSF catalog")).unwrap()
	}

	/// A change MSF doesn't carry republishes hang but not MSF.
	#[test]
	fn identical_msf_content_publishes_once() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		catalog
			.mutate(|catalog| {
				catalog
					.video
					.renditions
					.insert("v".to_string(), VideoConfig::new(hang::catalog::VideoCodec::VP8));
			})
			.unwrap();
		assert_eq!(msf_sent(&catalog), Some(0));
		let hang = sent(&catalog);

		catalog
			.mutate(|catalog| {
				catalog.video.display = Some(hang::catalog::Display {
					width: 1280,
					height: 720,
				})
			})
			.unwrap();
		assert_ne!(sent(&catalog), hang);
		assert_eq!(msf_sent(&catalog), Some(0));
	}

	/// A demand-driven track measures nothing until a viewer subscribes, which an MSF viewer can do
	/// only once MSF lists it. So an unhinted track declares MSF at once without estimates, and they
	/// stay absent once hang measures them. Regression: MSF waited for the estimates.
	#[tokio::test(start_paused = true)]
	async fn an_unhinted_track_declares_msf_at_once() {
		let (_broadcast, catalog, mut renditions) = estimating(&["v"]);
		let v = &mut renditions[0];
		assert_eq!(msf_sent(&catalog), Some(0), "no media has been written");
		let track = &msf_wire(&catalog).tracks[0];
		assert_eq!((track.bitrate, track.framerate), (None, None));
		assert_eq!((track.jitter, track.delay), (None, None));

		// 25 fps in 2 s groups.
		let mut estimator = super::super::Estimator::new();
		for i in 0..=150u64 {
			let ts = moq_net::Timestamp::from_micros(i * 40_000).unwrap();
			if i % 50 == 0 {
				estimator.cut(Some(ts));
			}
			estimator.write(ts, 1_000);
			v.estimate(estimator.reading()).unwrap();
		}
		let hang = &wire(&catalog).video.renditions["v"];
		assert!(hang.bitrate.is_some());
		assert_eq!(hang.framerate, Some(25.0));
		assert_eq!(msf_sent(&catalog), Some(0), "measurements leave MSF alone");
	}

	/// MSF declares the estimates a track hinted, and keeps those it first declared when a hint
	/// changes. A field left unhinted stays absent whatever hang measures.
	#[tokio::test(start_paused = true)]
	async fn a_hinted_track_carries_its_hints() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		let reserved = catalog.reserve();
		let mut v = reserved.init::<VideoConfig>("v").unwrap();
		let mut config = VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.bitrate = Some(1_000_000);
		config.framerate = Some(30.0);
		v.set(config).unwrap();
		drop(reserved);
		assert_eq!(msf_sent(&catalog), Some(0));
		let track = &msf_wire(&catalog).tracks[0];
		assert_eq!((track.bitrate, track.framerate), (Some(1_000_000), Some(30.0)));

		v.estimate(delay(40)).unwrap();
		v.supply_bitrate(2_000_000).unwrap();
		let hang = &wire(&catalog).video.renditions["v"];
		assert_eq!(hang.delay, Some(std::time::Duration::from_millis(40)));
		assert_eq!(hang.bitrate, Some(2_000_000));
		assert_eq!(msf_sent(&catalog), Some(0));
		let track = &msf_wire(&catalog).tracks[0];
		assert_eq!((track.bitrate, track.delay), (Some(1_000_000), None));
	}
}
