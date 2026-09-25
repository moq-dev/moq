use std::collections::BTreeSet;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard};

use base64::Engine;

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
	/// and `audio["v"]` are different renditions and neither says anything about the other.
	owned: BTreeSet<(std::any::TypeId, String)>,

	/// Gates the initial catalog publish until all reservations resolve (see
	/// [`reserve`](Producer::reserve)).
	reservations: Reservations,

	/// Why the catalog tracks are closed, so [`Producer::modify`] refuses further edits with the
	/// cause: [`Producer::finish`], or a drop-time publish that failed and aborted them.
	closed: Option<crate::Error>,
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
}

/// The built-in catalog's enrollment in the broadcast timeline.
struct CatalogTimeline {
	recorder: crate::timeline::Recorder,
	last_sequence: Option<u64>,
}

impl CatalogTimeline {
	/// Report a newly published plaintext catalog group once.
	fn record(&mut self, track: &moq_net::track::Producer) {
		let Some(sequence) = track.latest() else {
			return;
		};
		if self.last_sequence == Some(sequence) {
			return;
		}

		self.last_sequence = Some(sequence);
		self.recorder.record(sequence, moq_net::Timestamp::now(), true);
	}
}

/// The catalog tracks and the timeline enrollment updated with them.
struct Outputs<E: CatalogExt> {
	hang: moq_json::snapshot::Producer<Catalog<E>>,
	hang_track: moq_net::track::Producer,
	hangz: moq_json::snapshot::Producer<Catalog<E>>,
	msf_track: moq_net::track::Producer,
	catalog_timeline: Arc<Mutex<CatalogTimeline>>,
}

impl<E: CatalogExt> Clone for Outputs<E> {
	fn clone(&self) -> Self {
		Self {
			hang: self.hang.clone(),
			hang_track: self.hang_track.clone(),
			hangz: self.hangz.clone(),
			msf_track: self.msf_track.clone(),
			catalog_timeline: self.catalog_timeline.clone(),
		}
	}
}

impl<E: CatalogExt> Outputs<E> {
	/// Emit the catalog to hang, compressed hang, MSF, and the broadcast timeline.
	fn emit(&mut self, catalog: &Catalog<E>) -> crate::Result<()> {
		// One snapshot per group while deltas are disabled; the `.z` track carries the identical catalog.
		self.hang.update(catalog)?;
		self.catalog_timeline.lock().unwrap().record(&self.hang_track);
		self.hangz.update(catalog)?;

		// The MSF catalog is derived from our own types, so a serialize failure means an extension broke
		// the shape; report it like any other JSON failure rather than panicking. `abort` below is
		// what keeps the tracks consistent if this fails after the hang tracks were updated.
		let msf = to_msf(catalog).to_json().map_err(moq_json::Error::from)?;
		let mut group = self.msf_track.append_group()?;
		group.write_frame(moq_net::Timestamp::now(), msf)?;
		group.finish()?;

		Ok(())
	}

	/// Abort every catalog track after a publish nobody could return the error for.
	///
	/// The three tracks carry one catalog, so a failure part way through `emit` must not leave some
	/// of them serving a newer catalog than the others. Consumers see the error instead.
	fn abort(&mut self, err: &crate::Error) {
		// Only a transport error has a wire code; anything else (an extension that won't serialize)
		// reaches consumers as a generic failure while the cause stays on the producer.
		let reason = match err {
			crate::Error::Moq(err) => err.clone(),
			crate::Error::Json(moq_json::Error::Net(err)) => err.clone(),
			_ => moq_net::StreamError::Internal.into(),
		};
		// Idempotent: a track that is already closed keeps its first reason.
		let _ = self.hang.clone().abort(reason.clone());
		let _ = self.hangz.clone().abort(reason.clone());
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

	/// Shared wall clock for the broadcast's tracks. Every importer on this catalog
	/// gets a copy (the same epoch), so timestamps they synthesize when
	/// a caller has none land on one timeline and audio/video stay in sync.
	///
	/// It also owns the broadcast's wall mapping, published at the catalog root as
	/// `clock: { wall, timescale }` independently of any archive timeline.
	clock: crate::Clock,

	/// The broadcast's timeline: the shared boundary list every enrolled track's groups map
	/// onto, and the track those segment records are published on. See
	/// [`timeline`](Self::timeline).
	timeline: crate::timeline::Producer,
	/// Retention override for the media tracks minted under this catalog, or `None` to keep
	/// hang's default. Fixed at construction, so every clone and every
	/// [`Reserved`](super::Reserved) mints tracks under one policy. See
	/// [`Config::with_max_age`].
	max_age: Option<std::time::Duration>,
	/// Connection allocator passthrough tracks claim their peak-hold bitrate on.
	/// See [`Config::with_bandwidth`].
	bandwidth: moq_net::bandwidth::Allocator,
}

// Manual Clone so a producer is cheaply clonable regardless of whether `E` is.
impl<E: CatalogExt> Clone for Producer<E> {
	fn clone(&self) -> Self {
		Self {
			outputs: self.outputs.clone(),
			current: self.current.clone(),
			clock: self.clock,
			timeline: self.timeline.clone(),
			max_age: self.max_age,
			bandwidth: self.bandwidth.clone(),
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
	clock: crate::Clock,
	timeline: crate::timeline::Config,
}

impl Default for Config<()> {
	fn default() -> Self {
		Self {
			catalog: Catalog::default(),
			max_age: None,
			bandwidth: moq_net::bandwidth::Allocator::unlimited(),
			clock: crate::Clock::new(),
			timeline: crate::timeline::Config::default(),
		}
	}
}

impl<E: CatalogExt> Config<E> {
	/// Start from an extended catalog rather than the media-only one, e.g. the untyped
	/// [`Extra`] for the by-name / FFI path. Set application sections through
	/// [`Producer::modify`](Producer::modify) afterwards.
	pub fn with_catalog<F: CatalogExt>(self, catalog: Catalog<F>) -> Config<F> {
		Config {
			catalog,
			max_age: self.max_age,
			bandwidth: self.bandwidth,
			clock: self.clock,
			timeline: self.timeline,
		}
	}

	/// Publish with this broadcast clock instead of a fresh one.
	///
	/// The clock's wall mapping is advertised at the catalog root, so pass a clock whose PTS
	/// zero names the content's real start when importing a recording; live publishers use the
	/// default. The mapping is fixed for the broadcast and never overwritten.
	pub fn with_clock(mut self, clock: crate::Clock) -> Self {
		self.clock = clock;
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
	/// Applies to the media tracks the catalog mints. The catalog and timeline tracks keep
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

	/// Pace the broadcast's timeline with `timeline`.
	pub fn with_timeline(mut self, timeline: crate::timeline::Config) -> Self {
		self.timeline = timeline;
		self
	}
}

impl<E: CatalogExt> Producer<E> {
	/// Create a new catalog producer from a full [`Config`].
	pub fn new(broadcast: &mut moq_net::broadcast::Producer, config: Config<E>) -> Result<Self, moq_net::Error> {
		let hang_track = broadcast.create_track(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info())?;
		let hangz_track =
			broadcast.create_track(hang::Catalog::COMPRESSED_NAME, hang::Catalog::default_track_info())?;
		// The MSF track is the same catalog in another encoding, so it takes the same
		// priority. Leaving it at the default would rank a subscriber reading MSF
		// below every media track on a relay's upstream leg.
		let msf_info = moq_net::track::Info::default().with_priority(hang::catalog::PRIORITY.catalog);
		let msf_track = broadcast.create_track(moq_msf::DEFAULT_NAME, msf_info)?;

		// Disable deltas for now to stay byte-compatible with consumers that only read snapshots.
		let mut json_config = moq_json::snapshot::Config::default();
		json_config.delta_ratio = 0;
		let hang = moq_json::snapshot::Producer::new(hang_track.clone(), json_config.clone());

		// The `.z` track carries the same catalog, DEFLATE-compressed. Deltas stay off for parity
		// with the plaintext track; only the per-group compression differs.
		json_config.compression = moq_json::Compression::Deflate;
		let hangz = moq_json::snapshot::Producer::new(hangz_track, json_config);

		let timeline = crate::timeline::Producer::new(broadcast, config.timeline);
		#[allow(clippy::arc_with_non_send_sync)]
		let catalog_timeline = Arc::new(Mutex::new(CatalogTimeline {
			recorder: timeline.track(hang::Catalog::DEFAULT_NAME),
			last_sequence: None,
		}));

		// The broadcast clock is advertised at the catalog root from the first snapshot,
		// independently of any archive timeline: a live-only publisher exposes its mapping
		// without creating a segment index.
		let mut catalog = config.catalog;
		catalog.clock = Some(config.clock.wall());

		// The contents are `Send + Sync` natively; on wasm moq-net's handles are
		// `Rc`-backed, so clippy sees a pointlessly atomic `Arc`. Keeping one type for
		// both targets is worth the unused atomics on the single-threaded one.
		#[allow(clippy::arc_with_non_send_sync)]
		Ok(Self {
			outputs: Outputs {
				hang,
				hang_track,
				hangz,
				msf_track,
				catalog_timeline,
			},
			current: Arc::new(Mutex::new(State {
				catalog,
				owned: BTreeSet::new(),
				reservations: Reservations::default(),
				closed: None,
			})),
			clock: config.clock,
			timeline,
			max_age: config.max_age,
			bandwidth: config.bandwidth,
		})
	}

	/// Track properties for a media track under this catalog: hang's media defaults, plus any
	/// [`Config::with_max_age`] override.
	///
	/// `priority` should come from [`PRIORITY`](hang::catalog::PRIORITY) for the kind of media
	/// the track carries. Chain [`with_timescale`](moq_net::track::Info::with_timescale) for a
	/// container that carries the source's own scale (CMAF and Matroska both do).
	pub fn track_info(&self, priority: u8) -> moq_net::track::Info {
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
			None => Ok(self.clock.now()),
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
		take(&self.current).catalog.clone()
	}

	/// The broadcast's shared clock: the epoch importers stamp against and the wall mapping
	/// advertised at the catalog root.
	///
	/// Copies share the epoch, so handing them to concurrent producers keeps every track on
	/// one timeline. Translate a source with its own zero through
	/// [`Clock::source`](crate::Clock::source).
	pub fn clock(&self) -> crate::Clock {
		self.clock
	}

	/// Begin reserving the initial track set, returning a clonable [`Reserved`](super::Reserved).
	///
	/// Hand it (or clones) to importers, or call its role-specific track constructors. The catalog is
	/// withheld until every reservation is dropped and every created producer either publishes its
	/// config or drops. A one-shot muxer therefore publishes the complete track list in its first
	/// snapshot instead of a half-converged one. Producers created directly on this catalog publish
	/// incrementally.
	pub fn reserve(&self) -> super::Reserved<E> {
		super::Reserved::new(self.clone())
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
		self.media(track, container, rendition, config.into())
	}

	/// Whether a live producer or published config already claims `name` in `R`'s section.
	pub fn is_claimed<R: super::RenditionConfig<E>>(&self, name: &str) -> bool {
		let mut state = take(&self.current);
		R::get_mut(&mut state.catalog, name).is_some() || state.owned.contains(&owner_key::<R>(name))
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
		if C::get_mut(&mut state.catalog, name).is_some() || !state.owned.insert(owner_key::<C>(name)) {
			return Err(hang::Error::Duplicate(name.to_string()).into());
		}
		Ok(())
	}

	/// Register a live [`Reserved`](super::Reserved) handle.
	pub(super) fn add_reserver(&self) {
		take(&self.current).reservations.reservers += 1;
	}

	/// Drop a [`Reserved`](super::Reserved) handle, flushing the initial snapshot if that was the
	/// last thing gating it.
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
		// Snapshot under the lock and emit outside it, since emitting writes to the catalog tracks.
		let catalog = {
			let mut state = take(&self.current);
			let r = &mut state.reservations;
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
			state.catalog.clone()
		};
		if let Err(err) = self.outputs.emit(&catalog) {
			tracing::error!(%err, "failed to publish the catalog, aborting its tracks");
			self.outputs.abort(&err);
			take(&self.current).closed = Some(err);
		}
	}

	/// Build the media [`container::Producer`](crate::container::Producer) for `track`,
	/// enrolling it in the broadcast's timeline so its groups are indexed into the aligned
	/// segments.
	///
	/// The broadcast's one timeline track is created (and advertised in the catalog's root
	/// `archive` entry) on first use; see [`timeline`](crate::timeline) for the whole model.
	pub(super) fn media<C, R>(
		&self,
		track: moq_net::track::Producer,
		container: C,
		rendition: super::Rendition<E, R>,
		config: Option<R>,
	) -> crate::Result<crate::container::Producer<C, R>>
	where
		C: crate::container::Container,
		R: super::RenditionConfig<E>,
		crate::Error: From<C::Error>,
	{
		let mut catalog = self.clone();
		let recorder = catalog.enroll(track.name())?;
		let mut producer = crate::container::Producer::with_rendition(track, container, rendition)
			.with_recorder(recorder)
			.with_bandwidth(self.bandwidth.clone());
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
		let mut catalog = self.clone();
		let recorder = catalog.enroll(track.name())?;
		Ok(crate::container::Producer::new(track, container)
			.with_recorder(recorder)
			.with_bandwidth(self.bandwidth.clone()))
	}

	/// The allocator passthrough tracks claim on. fMP4 writes groups by hand, so it reads this itself.
	pub(crate) fn bandwidth(&self) -> moq_net::bandwidth::Allocator {
		self.bandwidth.clone()
	}

	/// Enroll `track` in the broadcast's timeline, advertising the timeline in the catalog's
	/// root section the first time.
	///
	/// The role-specific track constructors call this for you. fMP4 passthrough calls it directly
	/// because it writes groups by hand instead of using a [`container::Producer`](crate::container::Producer).
	pub(crate) fn enroll(&mut self, track: &str) -> crate::Result<crate::timeline::Recorder> {
		let recorder = self.timeline.pacing_track(track)?;

		let section = self.timeline.section();
		let mut catalog = self.modify()?;
		if catalog.archive.is_none() {
			catalog.archive = Some(section);
		}
		catalog.commit()?;

		Ok(recorder)
	}

	/// The broadcast's [`timeline::Producer`](crate::timeline::Producer): its segment index.
	///
	/// The MoQ track behind it is created (and advertised in the catalog's root `archive`
	/// entry) when the first media track enrolls, so reading this costs nothing on a
	/// broadcast that never segments. Use it to declare boundaries
	/// ([`cut`](crate::timeline::Producer::cut)) or to hold publishing back while tracks
	/// enroll ([`reserve`](crate::timeline::Producer::reserve), the timeline's counterpart to
	/// this producer's own [`reserve`](Self::reserve)).
	pub fn timeline(&self) -> crate::timeline::Producer {
		self.timeline.clone()
	}

	/// Publish `track` as a latest-value JSON track, advertising it in the catalog.
	///
	/// The caller creates the track on the broadcast, as it does for a media track; this writes its
	/// catalog entry and removes the entry when the returned handle drops. The catalog key is
	/// [`track.name()`](moq_net::track::Producer::name) verbatim, with no `.z` suffix even when
	/// compressed, since the entry's compression flag is what a consumer reads.
	///
	/// `config` is a [`json::Config`](crate::json::Config) for the `json` section, or an
	/// application's own entry embedding a [`JsonConfig`](hang::catalog::JsonConfig) (see
	/// [`IntoRendition`](super::IntoRendition)). The producer sets its `mode`, encodes the track
	/// with its `compression`, and fills an absent `bitrate` from what it writes. A
	/// [`delta_ratio`](crate::json::Config::delta_ratio) on `json::Config` selects the snapshot
	/// encoder; it is not written into the catalog entry. The config is `'static` so that ratio
	/// can be read off the builder.
	///
	/// Errors if the entry's section already carries that name, for example an entry seeded
	/// through [`Config::with_catalog`] or one pointing at a sibling broadcast, or if the entry
	/// declares a compression this build can't write or references another broadcast.
	pub fn json_snapshot<T: serde::Serialize>(
		&self,
		track: moq_net::track::Producer,
		config: impl super::IntoRendition<E, hang::catalog::JsonConfig> + 'static,
	) -> crate::Result<crate::json::Snapshot<T, E>> {
		let rendition = self.data_entry(track.name())?;
		crate::json::Snapshot::new(track, rendition, config)
	}

	/// Publish `track` as an append-log JSON track, advertising it in the catalog.
	///
	/// See [`json_snapshot`](Self::json_snapshot) for the lifecycle; this differs only in that every
	/// record is preserved rather than superseded.
	pub fn json_stream<T: serde::Serialize>(
		&self,
		track: moq_net::track::Producer,
		config: impl super::IntoRendition<E, hang::catalog::JsonConfig>,
	) -> crate::Result<crate::json::Stream<T, E>> {
		let rendition = self.data_entry(track.name())?;
		crate::json::Stream::new(track, rendition, config.into_rendition())
	}

	/// Publish `track` as a latest-value binary track, advertising it in the catalog.
	///
	/// See [`json_snapshot`](Self::json_snapshot) for the lifecycle; this differs only in that the
	/// payloads are opaque bytes, and `config` is a [`binary::Config`](crate::binary::Config) or an
	/// entry embedding a [`BinaryConfig`](hang::catalog::BinaryConfig).
	pub fn binary_snapshot(
		&self,
		track: moq_net::track::Producer,
		config: impl super::IntoRendition<E, hang::catalog::BinaryConfig>,
	) -> crate::Result<crate::binary::Snapshot<E>> {
		let rendition = self.data_entry(track.name())?;
		crate::binary::Snapshot::new(track, rendition, config.into_rendition())
	}

	/// Publish `track` as an append-log binary track, advertising it in the catalog.
	///
	/// See [`binary_snapshot`](Self::binary_snapshot); this differs only in that every payload is
	/// preserved rather than superseded.
	pub fn binary_stream(
		&self,
		track: moq_net::track::Producer,
		config: impl super::IntoRendition<E, hang::catalog::BinaryConfig>,
	) -> crate::Result<crate::binary::Stream<E>> {
		let rendition = self.data_entry(track.name())?;
		crate::binary::Stream::new(track, rendition, config.into_rendition())
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

	/// Finish publishing to this catalog.
	pub fn finish(&mut self) -> crate::Result<()> {
		take(&self.current).closed = Some(moq_net::Error::Closed.into());
		self.outputs.hang.finish()?;
		self.outputs.hangz.finish()?;
		self.outputs.msf_track.finish()?;
		self.timeline.finish()?;
		Ok(())
	}
}

/// RAII guard for modifying a catalog with automatic publishing on drop.
///
/// Obtained via [`Producer::modify`]. Derefs to the [`Catalog<E>`](super::hang::Catalog), so base
/// sections are editable directly and application sections are editable through `ext`.
///
/// On drop, the hang, compressed-hang, and MSF catalog tracks are updated if the catalog was
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

		{
			let r = &mut self.state.reservations;
			// Withhold every emit while still buffering the initial reserved set; the mutation stays
			// in `current` and `pending` marks it for the flush once the gate opens.
			if !r.published && r.reservers != 0 {
				r.pending = true;
				return Ok(());
			}
			r.pending = false;
			r.published = true;
		}

		self.outputs.emit(&self.state.catalog)
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
fn to_msf<E: CatalogExt>(catalog: &Catalog<E>) -> moq_msf::Catalog<E> {
	let mut msf = to_msf_media(&catalog.media());
	msf.ext = catalog.ext.clone();
	msf
}

/// Convert the base media sections of a hang catalog into MSF tracks.
fn to_msf_media<E: CatalogExt>(catalog: &hang::Catalog) -> moq_msf::Catalog<E> {
	let mut tracks = Vec::new();

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
		let mut track = moq_msf::Track::new(name.clone(), packaging);
		track.is_live = true;
		track.role = Some(moq_msf::Role::Video);
		track.codec = Some(config.codec.to_string());
		track.width = config.coded_width;
		track.height = config.coded_height;
		track.framerate = config.framerate;
		track.bitrate = config.bitrate;
		track.stalled = config.stalled;
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

	moq_msf::Catalog::with_ext(tracks, E::default())
}

#[cfg(test)]
mod test {
	use std::collections::BTreeMap;

	use std::task::Poll;

	use bytes::Bytes;
	use hang::catalog::{AudioCodec, AudioConfig, Container, H264, VideoConfig};

	use super::*;

	/// The catalog, its MSF twin, and the timeline all rank above media. They're the
	/// index a player reads before any media is useful, and they're small enough that
	/// sitting above media can't starve it. Regression: the MSF and timeline tracks
	/// were minted with no `Info` at all, so one broadcast advertised its hang catalog
	/// at 100 and the same catalog in MSF at 0.
	#[tokio::test]
	async fn non_media_tracks_rank_above_media() {
		use hang::catalog::PRIORITY;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		// Pacing enrollment is what mints the timeline track; a passive one publishes none.
		catalog.timeline().pacing_track("video").unwrap();

		let consumer = broadcast.consume();
		for name in [
			hang::Catalog::DEFAULT_NAME,
			hang::Catalog::COMPRESSED_NAME,
			moq_msf::DEFAULT_NAME,
			hang::timeline::DEFAULT_NAME,
		] {
			let track = consumer.track(name).expect("track");
			let info = track.query().await.expect("info");
			assert_eq!(info.priority, PRIORITY.catalog, "{name} should rank with the catalog");
		}
	}

	#[test]
	fn media_tracks_inherit_the_catalogs_declared_retention() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();

		// Unset, a catalog mints hang's media defaults, sized so a segmented egress can serve a
		// full playlist window rather than moq-net's live-edge default.
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		assert_eq!(
			catalog.track_info(hang::catalog::PRIORITY.video).max_age,
			hang::container::track_info(hang::catalog::PRIORITY.video).max_age
		);
		assert!(catalog.track_info(hang::catalog::PRIORITY.video).max_age > moq_net::track::DEFAULT_MAX_AGE);

		// An override reaches every media track this catalog mints, and does NOT disturb the
		// timescale hang pins (or survive a retimescale for a source-scale container).
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let config = Config::default().with_max_age(std::time::Duration::from_secs(3));
		let catalog = Producer::new(&mut broadcast, config).unwrap();

		let info = catalog.track_info(hang::catalog::PRIORITY.video);
		assert_eq!(info.max_age, std::time::Duration::from_secs(3));
		assert_eq!(info.timescale, hang::container::TIMESCALE);

		let at = info.with_timescale(moq_net::Timescale::MILLI);
		assert_eq!(at.max_age, std::time::Duration::from_secs(3));
		assert_eq!(at.timescale, moq_net::Timescale::MILLI);

		// Every handle mints under the same policy, whatever order it was taken in: the codec
		// paths hold a reservation and the container paths hold a clone.
		assert_eq!(
			catalog.reserve().track_info(hang::catalog::PRIORITY.video).max_age,
			std::time::Duration::from_secs(3)
		);
		assert_eq!(
			catalog.clone().track_info(hang::catalog::PRIORITY.video).max_age,
			std::time::Duration::from_secs(3)
		);
	}

	#[test]
	fn publishes_plain_and_compressed_tracks() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = Producer::new(&mut broadcast, Config::default()).unwrap();

		let mut plain = Consumer::new(catalog.outputs.hang.consume());
		let mut compressed = Consumer::compressed(catalog.outputs.hangz.consume());

		{
			let mut guard = catalog.modify().unwrap();
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
		assert_eq!(catalog.outputs.hang_track.latest(), Some(1));

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
			catalog.enroll("audio0"),
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
		let track = catalog.outputs.hang.consume();

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
		assert_eq!(track.latest(), Some(0));
	}

	#[test]
	fn commit_publishes_once() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = Producer::new(&mut broadcast, Config::default()).unwrap();
		let track = catalog.outputs.hang.consume();

		let mut guard = catalog.modify().unwrap();
		guard
			.audio
			.renditions
			.insert("audio0".to_string(), AudioConfig::new(AudioCodec::Opus, 48_000, 2));
		guard.commit().unwrap();

		// The drop that follows `commit` must not publish a second snapshot.
		catalog.finish().unwrap();
		assert_eq!(track.latest(), Some(0));
	}

	#[test]
	fn timeline_reports_a_track_collision() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = Producer::new(&mut broadcast, Config::default()).unwrap();

		// Something else already took the name the timeline track wants.
		let _taken = broadcast.create_track(hang::timeline::DEFAULT_NAME, None).unwrap();
		assert!(catalog.enroll("video0").is_err());
	}

	#[test]
	fn enrolling_advertises_the_catalog_section() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = Producer::new(&mut broadcast, Config::default()).unwrap();

		// A broadcast that never segments never advertises an archive.
		assert_eq!(catalog.snapshot().archive, None);
		let _recorder = catalog.enroll("video0").unwrap();
		assert_eq!(
			catalog.snapshot().archive,
			Some(catalog.timeline().section()),
			"the root archive should advertise the timeline track"
		);
	}

	#[test]
	fn clock_is_advertised_without_an_archive() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = Producer::new(&mut broadcast, Config::default()).unwrap();

		// A live-only broadcast exposes its clock without creating a segment index.
		let snapshot = catalog.snapshot();
		assert_eq!(snapshot.archive, None);
		assert_eq!(snapshot.clock, Some(catalog.clock().wall()));
	}

	#[test]
	fn with_clock_names_the_contents_real_start() {
		use std::time::{Duration, SystemTime};

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let wall = SystemTime::UNIX_EPOCH + Duration::from_millis(hang::catalog::MOQ_EPOCH_UNIX_MILLIS + 60_000);
		let config = Config::default()
			.with_clock(crate::Clock::at(std::time::Instant::now(), wall).expect("a representable wall"));
		let catalog = Producer::new(&mut broadcast, config).unwrap();

		// A recording import advertises the content's start, not the construction instant.
		assert_eq!(
			catalog.snapshot().clock.map(|clock| clock.wall),
			Some(moq_net::Timestamp::from_micros(60_000_000).unwrap())
		);
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
		video_config.stalled = Some(true);
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

		let msf = to_msf_media::<()>(&catalog);

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
		assert_eq!(video.stalled, Some(true));
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

		let msf = to_msf_media::<()>(&catalog);
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

		let msf = to_msf_media::<()>(&catalog);
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

		let msf = to_msf_media::<()>(&catalog);
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

		let msf = to_msf_media::<()>(&catalog);
		let video = &msf.tracks[0];
		assert_eq!(video.init_data, Some("AQID".to_string()));
	}

	#[test]
	fn convert_empty() {
		let catalog = hang::Catalog::default();
		let msf = to_msf_media::<()>(&catalog);
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

		let msf = to_msf_media::<()>(&catalog);
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

		let msf = to_msf_media::<()>(&catalog);

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

		let msf = to_msf_media::<()>(&catalog);
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

		let msf = to_msf_media::<()>(&catalog);
		let video = &msf.tracks[0];
		assert_eq!(video.max_grp_sap_starting_type, None);
		assert_eq!(video.max_obj_sap_starting_type, None);
	}
}
