use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use tokio::sync::watch;

use hang::moq_net;

static CAT: LazyLock<gst::DebugCategory> =
	LazyLock::new(|| gst::DebugCategory::new("moq-src", gst::DebugColorFlags::empty(), Some("MoQ Source Element")));

static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
	tokio::runtime::Builder::new_multi_thread()
		.enable_all()
		.build()
		.expect("spawn tokio runtime")
});

/// Process-wide pad id counters, one per pad kind. Kept global (not per-session) so a pad
/// created by a restarted element can't collide with one still being torn down by the
/// previous session, and split per kind so the *first* video pad is reliably `video_0` and the
/// first audio pad `audio_0`. That predictability matters because `gst-launch` links a
/// source's sometimes-pads by name (`moqsrc name=s s.video_0 ! ...`); a single shared counter
/// made the first pad's number depend on catalog arrival order (audio could claim `0`),
/// silently breaking those pipelines. A session keeps each pad by rendition across restarts
/// and format changes, so only a new or renamed rendition takes another id.
///
/// An id is claimed where the pad is created, not where its pump is spawned. A rendition whose
/// subscription never resolves therefore reserves nothing, so it can't leave `video_0` pointing
/// at a pad that will never exist while the rendition that does arrive lands on `video_1`.
static NEXT_VIDEO_PAD_ID: AtomicU64 = AtomicU64::new(0);
static NEXT_AUDIO_PAD_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Default)]
struct Settings {
	url: Option<String>,
	broadcast: Option<String>,
	tls_disable_verify: bool,
}

#[derive(Debug, Clone)]
struct ResolvedSettings {
	url: url::Url,
	broadcast: String,
	tls_disable_verify: bool,
}

impl TryFrom<Settings> for ResolvedSettings {
	type Error = anyhow::Error;

	fn try_from(value: Settings) -> Result<Self> {
		Ok(Self {
			url: url::Url::parse(value.url.as_ref().context("url property is required")?)?,
			broadcast: value
				.broadcast
				.as_ref()
				.context("broadcast property is required")?
				.clone(),
			tls_disable_verify: value.tls_disable_verify,
		})
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum TrackKind {
	Video,
	Audio,
}

impl TrackKind {
	fn template_name(&self) -> &'static str {
		match self {
			TrackKind::Video => "video_%u",
			TrackKind::Audio => "audio_%u",
		}
	}

	/// Claim the next pad name of this kind, matching its `%u` template.
	fn next_pad_name(&self) -> String {
		match self {
			TrackKind::Video => format!("video_{}", NEXT_VIDEO_PAD_ID.fetch_add(1, Ordering::Relaxed)),
			TrackKind::Audio => format!("audio_{}", NEXT_AUDIO_PAD_ID.fetch_add(1, Ordering::Relaxed)),
		}
	}
}

tokio::task_local! {
	/// The element whose session task or pump is running, so [`SessionController::stop`] can tell
	/// it was reached from that element's own streaming context.
	static STREAMING: glib::WeakRef<super::MoqSrc>;
}

/// The session task drives everything: it connects, follows the catalog, and
/// runs one [`Pump`] per active rendition. The element just starts and
/// stops it. No control-plane channel is needed because pumps push to their pads
/// directly from their own task (a source pad's push *is* its streaming thread),
/// so there's nothing to marshal back onto the element.
struct SessionController {
	shutdown: watch::Sender<bool>,
	join: tokio::task::JoinHandle<()>,
	/// The one-shot connection, held so [`stop`](Self::stop) can end it and wait for it to close.
	connection: moq_tokio::Connection,
}

impl SessionController {
	fn start(settings: ResolvedSettings, element: glib::WeakRef<super::MoqSrc>) -> Result<Self> {
		let (connection, origin) = connect(&settings)?;
		let task_connection = connection.clone();
		let task_element = element.clone();
		Ok(Self::spawn(connection, element, move |shutdown| async move {
			run_session(&task_connection, origin, settings.broadcast, task_element, shutdown).await
		}))
	}

	/// Run `session` as this element's session task, reporting its error on the bus.
	fn spawn<F, Fut>(connection: moq_tokio::Connection, element: glib::WeakRef<super::MoqSrc>, session: F) -> Self
	where
		F: FnOnce(watch::Receiver<bool>) -> Fut,
		Fut: Future<Output = Result<()>> + Send + 'static,
	{
		let (shutdown_tx, shutdown_rx) = watch::channel(false);
		let task = session(shutdown_rx.clone());
		let task_connection = connection.clone();
		let join = RUNTIME.spawn(STREAMING.scope(element.clone(), async move {
			let result = task.await;
			// A session that ends on its own releases the transport now rather than at stop.
			task_connection.abort(moq_net::Error::Cancel);
			// Stopping cuts the session short, which is not a failure.
			if let Err(err) = result
				&& !*shutdown_rx.borrow()
				&& let Some(obj) = element.upgrade()
			{
				gst::element_error!(obj, gst::CoreError::Failed, ("session error"), ["{err:?}"]);
			}
		}));

		Self {
			shutdown: shutdown_tx,
			join,
			connection,
		}
	}

	/// Stop the session, returning once its pumps have removed their pads and the connection closed.
	///
	/// A dial still running once the element reached NULL can outlive `main` (`gst-launch` exits right
	/// after), and aws-lc aborts the process when a thread asks it for randomness after its exit
	/// destructors ran.
	fn stop(self, element: &super::MoqSrc) {
		let _ = self.shutdown.send(true);

		// A pump blocked in a push returns once its pad flushes, the same unlock a GStreamer source
		// gives its streaming thread on stop. A push held up inside a downstream element is released
		// by that element leaving PAUSED, which a pipeline does before it reaches its source.
		for pad in element.src_pads() {
			let _ = pad.set_active(false);
		}

		// Reached from this element's own session task or pump (a bus sync or pad handler), waiting
		// for the session would wait on the caller's own stack. GStreamer refuses the same for its
		// sources; the connection still closes before this returns.
		let own = STREAMING
			.try_with(|streaming| streaming.upgrade().as_ref() == Some(element))
			.unwrap_or(false);
		if own {
			gst::warning!(
				CAT,
				obj = element,
				"stopped from its own streaming thread, not waiting for the session to end"
			);
		}

		let Self { join, connection, .. } = self;
		crate::block_on(async move {
			// The connection outlives the session so a stop never reads as a dropped connection.
			if !own && let Err(err) = join.await {
				gst::warning!(CAT, "session task ended with error: {err:?}");
			}
			connection.abort(moq_net::Error::Cancel);
			let _ = connection.closed().await;
		});
	}
}

/// Start the one-shot dial, returning it with the origin its announcements land in.
fn connect(settings: &ResolvedSettings) -> Result<(moq_tokio::Connection, moq_net::origin::Consumer)> {
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(settings.tls_disable_verify);

	// The origin and the connection loop are both spawned tasks.
	let _rt = RUNTIME.enter();
	let origin = moq_tokio::origin::spawn();
	let consumer = origin.consume();
	// One-shot: a drop ends the session with an error rather than redialing.
	let connection = config
		.init(Default::default())?
		.with_subscriber(origin)
		.with_reconnect(false)
		.connect(settings.url.clone());
	Ok((connection, consumer))
}

#[derive(Default)]
pub struct MoqSrc {
	settings: Mutex<Settings>,
	session: Mutex<Option<SessionController>>,
}

#[glib::object_subclass]
impl ObjectSubclass for MoqSrc {
	const NAME: &'static str = "MoqSrc";
	type Type = super::MoqSrc;
	type ParentType = gst::Element;

	fn new() -> Self {
		Self::default()
	}
}

impl ObjectImpl for MoqSrc {
	fn properties() -> &'static [glib::ParamSpec] {
		static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
			vec![
				glib::ParamSpecString::builder("url")
					.nick("Source URL")
					.blurb("Connect to the given URL")
					.mutable_ready()
					.build(),
				glib::ParamSpecString::builder("broadcast")
					.nick("Broadcast")
					.blurb("The broadcast name to subscribe to")
					.mutable_ready()
					.build(),
				glib::ParamSpecBoolean::builder("tls-disable-verify")
					.nick("TLS Disable Verify")
					.blurb("Disable TLS certificate verification")
					.default_value(false)
					.mutable_ready()
					.build(),
			]
		});
		PROPS.as_ref()
	}

	fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
		// The session is built from these once, on READY -> PAUSED. Storing a later
		// write would leave a value that reads back but never took effect. The
		// pending state covers the transition itself, where the session is already
		// built while the current state still reads READY.
		//
		// The lock is taken before the state is read, and start_session takes the
		// same one to copy the settings: either this write lands in that copy, or
		// it runs afterwards and finds the state above READY.
		let mut settings = self.settings.lock().unwrap();
		let obj = self.obj();
		if obj.current_state() > gst::State::Ready || obj.pending_state() > gst::State::Ready {
			gst::warning!(
				CAT,
				obj = obj,
				"{} ignored: the element is already started",
				pspec.name()
			);
			return;
		}
		match pspec.name() {
			"url" => settings.url = value.get().unwrap(),
			"broadcast" => settings.broadcast = value.get().unwrap(),
			"tls-disable-verify" => settings.tls_disable_verify = value.get().unwrap(),
			_ => unreachable!(),
		}
	}

	fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
		let settings = self.settings.lock().unwrap();
		match pspec.name() {
			"url" => settings.url.to_value(),
			"broadcast" => settings.broadcast.to_value(),
			"tls-disable-verify" => settings.tls_disable_verify.to_value(),
			_ => unreachable!(),
		}
	}
}

impl GstObjectImpl for MoqSrc {}
impl ElementImpl for MoqSrc {
	fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
		static META: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
			gst::subclass::ElementMetadata::new(
				"MoQ Src",
				"Source/Network/MoQ",
				"Receives media over the network via MoQ",
				"Luke Curley <kixelated@gmail.com>, Steve McFarlin <steve@stevemcfarlin.com>",
			)
		});
		Some(&*META)
	}

	fn pad_templates() -> &'static [gst::PadTemplate] {
		static PAD_TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
			vec![
				gst::PadTemplate::new(
					"video_%u",
					gst::PadDirection::Src,
					gst::PadPresence::Sometimes,
					&gst::Caps::new_any(),
				)
				.unwrap(),
				gst::PadTemplate::new(
					"audio_%u",
					gst::PadDirection::Src,
					gst::PadPresence::Sometimes,
					&gst::Caps::new_any(),
				)
				.unwrap(),
			]
		});
		PAD_TEMPLATES.as_ref()
	}

	fn change_state(&self, transition: gst::StateChange) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
		match transition {
			gst::StateChange::ReadyToPaused => {
				if let Err(err) = self.start_session() {
					gst::error!(CAT, obj = self.obj(), "failed to start session: {err:?}");
					return Err(gst::StateChangeError);
				}
				// Roll back the session we just started if the parent transition fails,
				// otherwise it would keep running while the element stays in READY.
				let Ok(success) = self.parent_change_state(transition) else {
					self.stop_session();
					return Err(gst::StateChangeError);
				};
				// A live source never prerolls.
				Ok(match success {
					gst::StateChangeSuccess::Async => gst::StateChangeSuccess::Async,
					_ => gst::StateChangeSuccess::NoPreroll,
				})
			}
			gst::StateChange::PausedToReady => {
				self.stop_session();
				self.parent_change_state(transition)
			}
			_ => self.parent_change_state(transition),
		}
	}
}

impl MoqSrc {
	fn start_session(&self) -> Result<()> {
		let settings = ResolvedSettings::try_from(self.settings.lock().unwrap().clone())?;
		let session = SessionController::start(settings, self.obj().downgrade())?;
		*self.session.lock().unwrap() = Some(session);
		Ok(())
	}

	fn stop_session(&self) {
		let session = self.session.lock().unwrap().take();
		if let Some(session) = session {
			session.stop(&self.obj());
		}
	}
}

/// The identity we reconcile a rendition on: a change to either field tears the pad down and
/// recreates it. Caps cover codec/resolution; the container descriptor covers the wire framing
/// (e.g. legacy -> cmaf).
#[derive(Clone, PartialEq)]
struct Shape {
	caps: gst::Caps,
	container: hang::catalog::Container,
}

/// A pump's progress, shared with its [`ActiveTrack`] so teardown and pad creation can't both
/// win. A pump is torn down two different ways depending on how far it got: before it owns a pad,
/// stopping it means it must never create one; after, it owns a pad and has to drop it. One
/// compare-exchange settles which of the two happened, so a pad can't slip out between a
/// teardown's check and the pump's creation.
struct PumpState(AtomicU8);

impl PumpState {
	const SUBSCRIBING: u8 = 0;
	const LIVE: u8 = 1;
	const CANCELLED: u8 = 2;

	fn new() -> Self {
		Self(AtomicU8::new(Self::SUBSCRIBING))
	}

	/// Claim the right to create a pad, false once a teardown got here first.
	fn go_live(&self) -> bool {
		self.0
			.compare_exchange(Self::SUBSCRIBING, Self::LIVE, Ordering::AcqRel, Ordering::Acquire)
			.is_ok()
	}

	/// Stop a pump that hasn't taken a pad, false if it already has one and must be torn down
	/// through its cancel watch instead.
	fn cancel_before_live(&self) -> bool {
		self.0
			.compare_exchange(Self::SUBSCRIBING, Self::CANCELLED, Ordering::AcqRel, Ordering::Acquire)
			.is_ok()
	}
}

/// A rendition by kind and moq track name: what a pad is kept by.
type Rendition = (TrackKind, String);

/// A rendition a run is serving, keyed in the run by moq track name.
struct ActiveTrack {
	kind: TrackKind,
	/// Identity we diff against on each catalog update; a change hands the pad to a new pump.
	shape: Shape,
	/// Tells the pump to hand its pad back and exit (set when the run ends or reconcile
	/// replaces the rendition).
	cancel: watch::Sender<bool>,
	/// Handle to the pump task in the run's `JoinSet`. We only read
	/// `is_finished()` to prune this entry once the pump ends (the `JoinSet` owns
	/// the task and reaps it); teardown goes through `cancel`, never `abort()`.
	task: tokio::task::AbortHandle,
	/// Shared with the pump, so teardown and pad creation agree on which of them happened.
	state: Arc<PumpState>,
}

impl ActiveTrack {
	/// Tear the pump down whatever stage it reached: one still subscribing never takes a pad,
	/// one that has hands it back when it sees the watch.
	///
	/// Terminal, so it consumes the handle: a cancelled rendition is removed from the run's
	/// active set, and respawns as a fresh pump if the catalog names it again.
	fn cancel(self) {
		self.state.cancel_before_live();
		let _ = self.cancel.send(true);
	}
}

/// The session's pads by rendition. They outlive the pumps and runs that stream to them, so a
/// pipeline linked by name (`s.video_0 ! ...`) keeps flowing across a restart.
#[derive(Clone, Default)]
struct Pads(Arc<Mutex<HashMap<Rendition, Slot>>>);

struct Slot {
	pad: gst::Pad,
	/// Held by the pump streaming to the pad, so its replacement waits for it to let go.
	owner: Arc<tokio::sync::Mutex<()>>,
}

/// A pad for a pump to stream to, from [`Pads::claim`].
enum Claim {
	/// A new pad, already owned, for the pump to add to the element.
	New(gst::Pad, tokio::sync::OwnedMutexGuard<()>),
	/// A kept pad, owned once its previous pump lets go.
	Kept(gst::Pad, Arc<tokio::sync::Mutex<()>>),
}

impl Pads {
	/// The rendition's pad, created if the session has none. `None` once `cancel` fired or the
	/// element is gone.
	fn claim(
		&self,
		element: &glib::WeakRef<super::MoqSrc>,
		rendition: Rendition,
		cancel: &watch::Receiver<bool>,
	) -> Option<Claim> {
		let obj = element.upgrade()?;
		let mut slots = self.0.lock().unwrap();
		// Checked under the lock: a run cancels a pump before it retires the pump's pad, so a
		// cancelled pump never claims one behind the retirement.
		if *cancel.borrow() {
			return None;
		}
		if let Some(slot) = slots.get(&rendition) {
			return Some(Claim::Kept(slot.pad.clone(), slot.owner.clone()));
		}

		let kind = rendition.0;
		let templ = obj.element_class().pad_template(kind.template_name())?;
		let pad = gst::Pad::builder_from_template(&templ)
			.name(kind.next_pad_name())
			.build();
		let owner = Arc::new(tokio::sync::Mutex::new(()));
		let owned = owner.clone().try_lock_owned().expect("a new lock is free");
		slots.insert(
			rendition,
			Slot {
				pad: pad.clone(),
				owner,
			},
		);
		Some(Claim::New(pad, owned))
	}

	/// Take the rendition's pad out of the session, returning the task that ends it with EOS once
	/// its pump lets go.
	fn retire(&self, rendition: &Rendition) -> Option<impl Future<Output = Option<Rendition>> + Send + 'static> {
		let slot = self.0.lock().unwrap().remove(rendition)?;
		Some(async move {
			let _owned = slot.owner.clone().lock_owned().await;
			slot.end(true);
			None
		})
	}

	/// The renditions with a pad that `keep` rejects.
	fn unwanted(&self, keep: impl Fn(&Rendition) -> bool) -> Vec<Rendition> {
		let slots = self.0.lock().unwrap();
		slots.keys().filter(|rendition| !keep(rendition)).cloned().collect()
	}

	/// Remove every pad once nothing streams to them any more, after an EOS if `eos`.
	fn release(&self, eos: bool) {
		let slots = std::mem::take(&mut *self.0.lock().unwrap());
		for slot in slots.into_values() {
			slot.end(eos);
		}
	}
}

impl Slot {
	fn end(self, eos: bool) {
		if eos {
			let _ = tokio::task::block_in_place(|| self.pad.push_event(gst::event::Eos::new()));
		}
		let _ = self.pad.set_active(false);
		if let Some(parent) = self.pad.parent_element() {
			let _ = parent.remove_pad(&self.pad);
		}
	}
}

async fn run_session(
	connection: &moq_tokio::Connection,
	origin: moq_net::origin::Consumer,
	broadcast: String,
	element: glib::WeakRef<super::MoqSrc>,
	mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
	// Stop closes the connection only after the session ends, so the dial races shutdown here.
	tokio::select! {
		established = moq_net::kio::wait(|waiter| connection.poll_established(waiter)) => established?,
		_ = shutdown.changed() => return Ok(()),
	}

	// The dial is one-shot, so once it closes nothing can announce the path again.
	let lost = async {
		match connection.closed().await {
			Ok(()) => anyhow::anyhow!("connection closed"),
			Err(err) => anyhow::Error::from(err).context("connection closed"),
		}
	};
	follow_path(&origin, &broadcast, element, shutdown, lost).await
}

/// Follow the path's announcements for the whole session: play on a start, switch to the new
/// broadcast on a restart, and hold the pads from an end until the next start. Returns once
/// `shutdown` fires, `lost` resolves, or the origin closes.
async fn follow_path(
	origin: &moq_net::origin::Consumer,
	path: &str,
	element: glib::WeakRef<super::MoqSrc>,
	mut shutdown: watch::Receiver<bool>,
	lost: impl Future<Output = anyhow::Error>,
) -> Result<()> {
	use moq_net::announce::Event;

	let mut follow = origin.follow(path)?;
	let pads = Pads::default();
	let mut runs = tokio::task::JoinSet::new();
	// Cancels the latest run.
	let mut latest: Option<watch::Sender<bool>> = None;
	let mut lost = std::pin::pin!(lost);

	tracing::info!(%path, "waiting for broadcast to be announced");
	let result = loop {
		let start = tokio::select! {
			_ = shutdown.changed() => break Ok(()),
			err = &mut lost => break Err(err),
			event = follow.next() => match event {
				// The origin is gone, so nothing can announce the path again.
				None => break Ok(()),
				Some(Event::Start(announce) | Event::Restart(announce)) => {
					tracing::info!(%path, epoch = announce.route.epoch.as_ref().map(tracing::field::display), "online");
					true
				}
				// The same instance, re-priced or failed over, which the run rides out.
				Some(Event::Update(_)) => false,
				Some(Event::End(_)) => {
					tracing::info!(%path, "offline, holding pads until it returns");
					false
				}
			},
			joined = runs.join_next(), if !runs.is_empty() => match joined.expect("guarded by is_empty") {
				Ok(Ok(())) => false,
				Ok(Err(err)) => break Err(err),
				Err(err) => break Err(anyhow::Error::from(err).context("run panicked")),
			},
		};

		if start {
			// Cut over at once: the old run's pumps hand their pads back without waiting for their
			// subscriptions to end.
			if let Some(cancel) = latest.take() {
				let _ = cancel.send(true);
			}
			latest = Some(play(&mut runs, origin, path, &pads, &element, &shutdown));
		}
	};

	if let Some(cancel) = latest.take() {
		let _ = cancel.send(true);
	}
	while runs.join_next().await.is_some() {}
	// A stop removes the pads; anything else ends the stream downstream first.
	pads.release(!*shutdown.borrow());
	result
}

/// Start a run: one request for the path and the catalog it serves, until it ends or the next
/// start or restart replaces it. Returns what cancels it.
fn play(
	runs: &mut tokio::task::JoinSet<Result<()>>,
	origin: &moq_net::origin::Consumer,
	path: &str,
	pads: &Pads,
	element: &glib::WeakRef<super::MoqSrc>,
	shutdown: &watch::Receiver<bool>,
) -> watch::Sender<bool> {
	let (cancel, mut cancelled) = watch::channel(false);
	let (origin, path, pads, task_element, shutdown) = (
		origin.clone(),
		path.to_string(),
		pads.clone(),
		element.clone(),
		shutdown.clone(),
	);
	runs.spawn_on(
		STREAMING.scope(element.clone(), async move {
			let broadcast = tokio::select! {
				_ = cancelled.changed() => return Ok(()),
				broadcast = origin.request_broadcast(&path, None) => match broadcast {
					Ok(broadcast) => broadcast,
					// The route went between the announcement and the request.
					Err(err) if source_lost(&err) => {
						tracing::warn!(%path, %err, "broadcast unavailable, holding pads");
						return Ok(());
					}
					Err(err) => return Err(anyhow::Error::from(err).context("broadcast refused")),
				},
			};
			follow_catalog(broadcast, &pads, task_element, &shutdown, &mut cancelled).await
		}),
		RUNTIME.handle(),
	);
	cancel
}

/// Whether a failure is the source going away (its session closing, or its route going), which
/// holds the pads for the next start. A refusal is not: no restart answers a path that names
/// nothing or a token that doesn't grant it, so it fails the session, as a malformed catalog does.
fn source_lost(err: &moq_net::Error) -> bool {
	!matches!(err, moq_net::Error::NotFound | moq_net::Error::Unauthorized)}

/// [`source_lost`] for a catalog read, where anything but a transport failure is malformed.
fn catalog_lost(err: &moq_mux::Error) -> bool {
	match err {
		moq_mux::Error::Moq(err) | moq_mux::Error::Json(moq_json::Error::Net(err)) => source_lost(err),
		_ => false,
	}
}

/// Follow one broadcast's catalog, keeping one [`Pump`] per announced rendition in sync with it.
/// Returns once the catalog closes and the last pump drains, or `cancel` fires. The pads stay with
/// the session, except those of renditions the catalog retired.
async fn follow_catalog(
	broadcast: moq_net::broadcast::Consumer,
	pads: &Pads,
	element: glib::WeakRef<super::MoqSrc>,
	shutdown: &watch::Receiver<bool>,
	cancel: &mut watch::Receiver<bool>,
) -> Result<()> {
	let catalog_track = broadcast.track(hang::catalog::Catalog::DEFAULT_NAME)?;
	// A publisher that never answers would otherwise hold the run until it is cancelled.
	let catalog_track = tokio::select! {
		track = catalog_track.subscribe(hang::catalog::Catalog::default_subscription()) => match track {
			Ok(track) => track,
			Err(err) if source_lost(&err) => {
				tracing::warn!(%err, "catalog unavailable, holding pads");
				return Ok(());
			}
			Err(err) => return Err(anyhow::Error::from(err).context("catalog refused")),
		},
		_ = cancel.changed() => return Ok(()),
	};
	let mut catalog_consumer = moq_mux::catalog::hang::Consumer::new(catalog_track);

	// Follow the catalog for the whole run and reconcile our pumps against every update,
	// rather than building them once from the first frame. This covers reactive publishers
	// (the browser via @moq/hang) that announce an empty catalog before their encoder
	// configures, then add renditions a beat later, as well as renditions appearing,
	// disappearing, or changing codec/resolution mid-stream.
	let mut active: HashMap<String, ActiveTrack> = HashMap::new();
	let mut pumps: tokio::task::JoinSet<Option<Rendition>> = tokio::task::JoinSet::new();
	// The renditions the latest catalog lists.
	let mut listed: HashSet<Rendition> = HashSet::new();
	let mut catalog_closed = false;
	let mut result = Ok(());

	loop {
		// Prune metadata for pumps that have ended (the JoinSet has already reaped the
		// tasks). Once the catalog is closed and the last pump drains, the run is done.
		active.retain(|_, track| !track.task.is_finished());
		if catalog_closed && pumps.is_empty() {
			break;
		}

		tokio::select! {
			biased;
			// Ended by a restart, or the session ending.
			_ = cancel.changed() => break,
			// A pump finished; loop back so the `retain` above prunes its entry and the
			// break condition sees the drained set.
			joined = pumps.join_next(), if !pumps.is_empty() => {
				// A rendition the catalog retired ends with EOS once its track does. One still
				// listed keeps its pad, since the track, the catalog, and the announcements arrive
				// on different streams in either order.
				if let Some(Ok(Some(rendition))) = joined
					&& !listed.contains(&rendition)
					&& let Some(retire) = pads.retire(&rendition)
				{
					pumps.spawn_on(STREAMING.scope(element.clone(), retire), RUNTIME.handle());
				}
			}
			// The guard stops us polling a closed catalog track (which would spin the loop
			// returning None) while we wait for the remaining pumps to drain.
			next = catalog_consumer.next(), if !catalog_closed => {
				match next {
					Ok(Some(catalog)) => {
						listed = reconcile(&catalog, &mut active, &mut pumps, &broadcast, pads, &element, shutdown);
					}
					// Catalog track closed. Don't cancel the pumps: let each reach its natural end,
					// and just stop reconciling while they drain.
					//
					// That includes a pump still resolving its subscription, which is
					// indistinguishable here from one that has simply not been polled yet:
					// a final snapshot naming a track the publisher does serve arrives this
					// way, and cancelling on "not live yet" would drop it. Whether a
					// rendition will ever be served is the publisher's to answer, and
					// ending the broadcast is where it does: every name it never served
					// resolves with an error, which fails that pump's subscribe and ends
					// it here.
					Ok(None) => catalog_closed = true,
					Err(err) if catalog_lost(&err) => {
						tracing::warn!(%err, "catalog lost, holding pads");
						catalog_closed = true;
					}
					Err(err) => {
						result = Err(err.into());
						break;
					}
				}
			}
		}
	}

	// Cancel every pump, then wait for them all to hand their pads back. Cancel all up front
	// (pumps only exit on their own `cancel`), or the not-yet-cancelled ones would keep streaming
	// while we await the rest. On the clean catalog-closed exit `active`/`pumps` are already
	// drained, so this is a no-op.
	for (_, track) in active.drain() {
		track.cancel();
	}
	while pumps.join_next().await.is_some() {}

	result
}

/// Bring the run's pumps in line with `catalog`: spawn pumps for newly announced renditions,
/// hand the pad of any whose caps or container changed to a new pump, leave ones that vanished to
/// end with their track, and end the kept pads of renditions it no longer lists. Returns the
/// renditions it lists.
///
/// Infallible by design: every way a single rendition can be unusable (unsupported codec,
/// malformed init, a name the broadcast refuses) skips just that rendition, so one bad entry in
/// the catalog can never tear down the ones already streaming.
fn reconcile(
	catalog: &moq_mux::catalog::hang::Catalog,
	active: &mut HashMap<String, ActiveTrack>,
	pumps: &mut tokio::task::JoinSet<Option<Rendition>>,
	broadcast: &moq_net::broadcast::Consumer,
	pads: &Pads,
	element: &glib::WeakRef<super::MoqSrc>,
	shutdown: &watch::Receiver<bool>,
) -> HashSet<Rendition> {
	struct Desired {
		kind: TrackKind,
		shape: Shape,
	}

	// Build the desired shape for each rendition. This is deliberately cheap: caps come from the
	// catalog config and the container is just the hang descriptor. We defer parsing the wire
	// container (which re-parses the CMAF init) to spawn time below, so an unchanged rendition
	// costs nothing here. A rendition whose caps we can't build (unsupported codec) is logged and
	// skipped rather than failing the whole run, so one bad rendition can't tear down the
	// others we're already serving.
	let mut desired: HashMap<String, Desired> = HashMap::new();
	let mut insert = |name: &String, kind, caps: Result<gst::Caps>, container: &hang::catalog::Container| match caps {
		Ok(caps) => {
			let shape = Shape {
				caps,
				container: container.clone(),
			};
			desired.insert(name.clone(), Desired { kind, shape });
		}
		Err(err) => gst::warning!(CAT, "ignoring {kind:?} rendition {name}: {err:?}"),
	};
	for (name, config) in &catalog.video.renditions {
		insert(name, TrackKind::Video, video_caps(config), &config.container);
	}
	for (name, config) in &catalog.audio.renditions {
		insert(name, TrackKind::Audio, audio_caps(config), &config.container);
	}

	// Pure set math: which pumps to tear down, which renditions to spawn.
	let plan = plan_reconcile(
		&desired
			.iter()
			.map(|(name, d)| (name.clone(), (d.kind, d.shape.clone())))
			.collect(),
		&active
			.iter()
			.map(|(name, t)| (name.clone(), (t.kind, t.shape.clone())))
			.collect(),
	);

	// Cancel anything that changed shape; each cancelled pump hands its pad back. Changed
	// renditions also land in `plan.add`, so they respawn below on the same pad.
	//
	// A rendition that vanished is only no longer selectable: a publisher retires one by
	// delisting it and finishing its track, and the two arrive in either order. A pump that owns
	// a pad stays in `active` and ends with its track, where `follow_catalog` ends its pad. One
	// that never took a pad is stopped.
	for name in plan.remove {
		if !desired.contains_key(&name) && active.get(&name).is_some_and(|track| !track.state.cancel_before_live()) {
			continue;
		}
		if let Some(track) = active.remove(&name) {
			track.cancel();
		}
	}

	// A kept pad the catalog no longer lists ends with EOS, unless a pump still drains its track.
	let unwanted = pads.unwanted(|(kind, name)| {
		desired.get(name).is_some_and(|d| d.kind == *kind) || active.get(name).is_some_and(|t| t.kind == *kind)
	});
	for rendition in unwanted {
		if let Some(retire) = pads.retire(&rendition) {
			pumps.spawn_on(STREAMING.scope(element.clone(), retire), RUNTIME.handle());
		}
	}

	// Spawn pumps for new or changed renditions. The wire container is parsed here, lazily and
	// only for renditions we're actually starting, since parsing a CMAF init is wasted work for
	// renditions that didn't change. A parse failure (malformed init) skips just this rendition.
	for name in plan.add {
		let d = &desired[&name];
		let container = match moq_mux::catalog::hang::Container::new(
			&d.shape.container,
			match d.kind {
				TrackKind::Video => moq_mux::container::Kind::Video,
				TrackKind::Audio => moq_mux::container::Kind::Audio,
			},
		) {
			Ok(container) => container,
			Err(err) => {
				gst::warning!(CAT, "ignoring rendition {name}: {err:?}");
				continue;
			}
		};

		// Only the handle is resolved here; the pump awaits the subscription itself. That wait
		// ends when the publisher answers with the track info, which for a rendition nobody
		// serves is never, so doing it here would park the catalog loop and leave every other
		// rendition unstarted. A name the broadcast refuses outright skips just this rendition,
		// same as an unsupported codec or a malformed init above.
		let track = match broadcast.track(&name) {
			Ok(track) => track,
			Err(err) => {
				gst::warning!(CAT, "ignoring rendition {name}: {err:?}");
				continue;
			}
		};

		let (cancel_tx, cancel_rx) = watch::channel(false);
		let state = Arc::new(PumpState::new());
		let task = pumps.spawn_on(
			STREAMING.scope(
				element.clone(),
				Pump {
					element: element.clone(),
					kind: d.kind,
					name: name.clone(),
					caps: d.shape.caps.clone(),
					track,
					container,
					pads: pads.clone(),
					state: state.clone(),
					cancel: cancel_rx,
					shutdown: shutdown.clone(),
				}
				.run(),
			),
			RUNTIME.handle(),
		);

		active.insert(
			name,
			ActiveTrack {
				kind: d.kind,
				shape: d.shape.clone(),
				cancel: cancel_tx,
				task,
				state,
			},
		);
	}

	desired.into_iter().map(|(name, d)| (d.kind, name)).collect()
}

/// Tear-down / spawn decisions for one catalog update, computed purely from the desired and
/// active rendition sets. A name present in both with an equal shape is left untouched; a name
/// whose shape changed lands in both lists (cancel the old pump, spawn a fresh one).
struct ReconcilePlan {
	remove: Vec<String>,
	add: Vec<String>,
}

fn plan_reconcile<S: PartialEq>(desired: &HashMap<String, S>, active: &HashMap<String, S>) -> ReconcilePlan {
	let remove = active
		.iter()
		.filter(|(name, shape)| desired.get(*name) != Some(*shape))
		.map(|(name, _)| name.clone())
		.collect();
	let add = desired
		.iter()
		.filter(|(name, shape)| active.get(*name) != Some(*shape))
		.map(|(name, _)| name.clone())
		.collect();
	ReconcilePlan { remove, add }
}

/// One rendition's pump: everything [`reconcile`] hands a task it spawns.
struct Pump {
	element: glib::WeakRef<super::MoqSrc>,
	kind: TrackKind,
	/// The moq track name, which is also the pad's stream id. The pad's own name comes from a
	/// per-kind counter instead, and isn't known until the subscription resolves.
	name: String,
	caps: gst::Caps,
	track: moq_net::track::Consumer,
	container: moq_mux::catalog::hang::Container,
	pads: Pads,
	/// Shared with this rendition's [`ActiveTrack::state`].
	state: Arc<PumpState>,
	cancel: watch::Receiver<bool>,
	/// The session's shutdown, which [`SessionController::stop`] sets before it flushes the pads.
	shutdown: watch::Receiver<bool>,
}

impl Pump {
	/// Subscribe to the track, then stream its frames to the rendition's pad, owning it until the
	/// track ends, errors, or `cancel` fires. The pad goes back to the session without an EOS
	/// either way. Returns the rendition if its track ended rather than being cancelled, so the
	/// run can end the pad of one the catalog retired.
	async fn run(self) -> Option<Rendition> {
		let Pump {
			element,
			kind,
			name,
			caps,
			track,
			container,
			pads,
			state,
			mut cancel,
			shutdown,
		} = self;
		// Resolves once the publisher answers, with the track info or with an error (which is
		// what ending a broadcast produces for a name nobody served). A publisher that answers
		// neither leaves this waiting, so racing `cancel` keeps such a pump reapable, and
		// holding the wait here rather than in `reconcile` keeps it off every other
		// rendition.
		let subscriber = tokio::select! {
			_ = cancel.changed() => return None,
			subscriber = track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(1))) => match subscriber {
				Ok(subscriber) => subscriber,
				Err(err) => {
					gst::warning!(CAT, "track {name} failed to subscribe: {err:?}");
					return Some((kind, name));
				}
			}
		};
		let mut track = moq_mux::container::Consumer::new(subscriber, container);

		// Winning this is what earns a pad. Losing means a teardown got here while the
		// subscription was still resolving (this rendition was removed, reshaped, or outlived by
		// a closing catalog), and it must not publish a pad at all: the watch alone can't say
		// that, since a cancel landing just after we read it would leave a pad exposed and then
		// yanked without an EOS.
		if !state.go_live() {
			return None;
		}

		// A pad appears only once its track is live, so a pad downstream can link to is a promise
		// that the rendition is actually flowing, and only a rendition that gets this far claims a
		// pad id. A kept pad starts a new stream on it, with caps pushed even when they differ, so
		// a format downstream can't take fails loudly as not-negotiated rather than going quiet.
		let (pad, _owned) = match pads.claim(&element, (kind, name.clone()), &cancel)? {
			Claim::New(pad, owned) => {
				let obj = element.upgrade()?;
				pad.set_active(true).ok()?;
				begin(&pad, &name, &caps);
				obj.add_pad(&pad).ok()?;
				(pad, owned)
			}
			Claim::Kept(pad, owner) => {
				let owned = tokio::select! {
					biased;
					_ = cancel.changed() => return None,
					owned = owner.lock_owned() => owned,
				};
				tokio::task::block_in_place(|| begin(&pad, &name, &caps));
				(pad, owned)
			}
		};
		// Stop flushes only the pads it finds, and this one may have been added just after, while
		// this pump's cancel is still on its way. Flushing it here keeps a push from blocking stop.
		if *shutdown.borrow() {
			let _ = pad.set_active(false);
		}

		let mut reference_ts = None;
		loop {
			tokio::select! {
				// This rendition is being torn down (its run ended, or a catalog update replaced it).
				_ = cancel.changed() => return None,
				frame = track.read() => match frame {
					Ok(Some(frame)) => {
						// PTS restarts at zero with every pump, so the segment places that zero at the
						// current running time, or a synced sink would drop a restarted run as late.
						let segment = reference_ts.is_none().then(|| {
							let mut segment = gst::FormattedSegment::<gst::ClockTime>::new();
							segment.set_base(
								element
									.upgrade()
									.and_then(|obj| obj.current_running_time())
									.unwrap_or(gst::ClockTime::ZERO),
							);
							gst::event::Segment::new(&segment)
						});
						let buffer = build_buffer(frame, &mut reference_ts, kind);
						// pad.push() blocks until downstream accepts the buffer (full queues, a
						// clock-synced sink). block_in_place hands our sibling tasks to another
						// worker so a stalled downstream can't pin a runtime thread and starve
						// the session loop or other pumps.
						let pushed = tokio::task::block_in_place(|| {
							if let Some(segment) = segment {
								pad.push_event(segment);
							}
							pad.push(buffer)
						});
						match pushed {
							Ok(_) => {}
							Err(gst::FlowError::NotNegotiated) => {
								if let Some(obj) = element.upgrade() {
									gst::element_error!(obj, gst::StreamError::Format, ("track {name} was not negotiated"), ["caps {caps}"]);
								}
								return Some((kind, name));
							}
							Err(_) => return Some((kind, name)),
						}
					}
					Ok(None) => return Some((kind, name)),
					Err(err) => {
						gst::warning!(CAT, "track {name} failed: {err:?}");
						return Some((kind, name));
					}
				}
			}
		}
	}
}

/// Start a new stream on the pad: a fresh stream-start and the rendition's caps. The segment
/// waits for the first buffer, which fixes its running time.
fn begin(pad: &gst::Pad, name: &str, caps: &gst::Caps) {
	pad.push_event(
		gst::event::StreamStart::builder(name)
			.group_id(gst::GroupId::next())
			.build(),
	);
	pad.push_event(gst::event::Caps::new(caps));
}

/// Wrap a decoded frame in a gst buffer, assigning a pts relative to the track's first frame.
fn build_buffer(
	frame: moq_mux::container::Frame,
	reference_ts: &mut Option<moq_net::Timestamp>,
	kind: TrackKind,
) -> gst::Buffer {
	let mut buffer = gst::Buffer::from_slice(frame.payload);
	let buffer_mut = buffer.get_mut().unwrap();

	let pts = match *reference_ts {
		Some(reference) => relative_pts(frame.timestamp, reference),
		None => {
			*reference_ts = Some(frame.timestamp);
			gst::ClockTime::ZERO
		}
	};
	buffer_mut.set_pts(Some(pts));

	let mut flags = buffer_mut.flags();
	match kind {
		// Video carries the keyframe bit per frame; audio frames are all keyframes.
		TrackKind::Video if frame.keyframe => flags.remove(gst::BufferFlags::DELTA_UNIT),
		TrackKind::Video => flags.insert(gst::BufferFlags::DELTA_UNIT),
		TrackKind::Audio => flags.remove(gst::BufferFlags::DELTA_UNIT),
	}
	buffer_mut.set_flags(flags);

	buffer
}

/// PTS of `timestamp` relative to the track's first frame (`reference`).
///
/// Frames arrive in decode order, so a B-frame's presentation timestamp can fall before
/// the reference. `Timestamp` subtraction panics on underflow, so clamp to zero rather
/// than crash the pump (which would leak its pad).
fn relative_pts(timestamp: moq_net::Timestamp, reference: moq_net::Timestamp) -> gst::ClockTime {
	match timestamp.checked_sub(reference) {
		Ok(delta) => gst::ClockTime::from_nseconds(Duration::from(delta).as_nanos() as u64),
		Err(_) => gst::ClockTime::ZERO,
	}
}

fn video_caps(config: &hang::catalog::VideoConfig) -> Result<gst::Caps> {
	use hang::catalog::VideoCodec;

	let caps = match &config.codec {
		VideoCodec::H264(_) => {
			let mut builder = gst::Caps::builder("video/x-h264").field("alignment", "au");
			if let Some(description) = &config.description {
				builder = builder
					.field("stream-format", "avc")
					.field("codec_data", gst::Buffer::from_slice(description.clone()));
			} else {
				builder = builder.field("stream-format", "annexb");
			}
			builder.build()
		}
		VideoCodec::H265(h265) => {
			let mut builder = gst::Caps::builder("video/x-h265").field("alignment", "au");
			match &config.description {
				Some(description) => {
					let format = if h265.in_band { "hev1" } else { "hvc1" };
					builder = builder
						.field("stream-format", format)
						.field("codec_data", gst::Buffer::from_slice(description.clone()));
				}
				None => {
					let format = if h265.in_band { "hev1" } else { "byte-stream" };
					builder = builder.field("stream-format", format);
				}
			}
			builder.build()
		}
		VideoCodec::AV1(_) => {
			let mut builder = gst::Caps::builder("video/x-av1");
			if let Some(description) = &config.description {
				builder = builder.field("codec_data", gst::Buffer::from_slice(description.clone()));
			}
			builder.build()
		}
		// VP8/VP9 are raw frame streams: gstreamer carries each frame as one buffer
		// and the decoders read configuration inline, so no codec_data is attached.
		VideoCodec::VP8 => gst::Caps::builder("video/x-vp8").build(),
		VideoCodec::VP9(_) => gst::Caps::builder("video/x-vp9").build(),
		other => bail!("unsupported video codec: {other:?}"),
	};
	Ok(caps)
}

fn audio_caps(config: &hang::catalog::AudioConfig) -> Result<gst::Caps> {
	let caps = match &config.codec {
		hang::catalog::AudioCodec::AAC(_) => {
			let mut builder = gst::Caps::builder("audio/mpeg")
				.field("mpegversion", 4)
				.field("rate", config.sample_rate)
				.field("channels", config.channel_count);
			if let Some(description) = &config.description {
				builder = builder
					.field("codec_data", gst::Buffer::from_slice(description.clone()))
					.field("stream-format", "aac");
			} else {
				builder = builder.field("stream-format", "adts");
			}
			builder.build()
		}
		hang::catalog::AudioCodec::Opus => {
			let mut builder = gst::Caps::builder("audio/x-opus")
				.field("rate", config.sample_rate)
				.field("channels", config.channel_count);
			if let Some(description) = &config.description {
				builder = builder
					.field("codec_data", gst::Buffer::from_slice(description.clone()))
					.field("stream-format", "ogg");
			}
			builder.build()
		}
		hang::catalog::AudioCodec::Mp3 => gst::Caps::builder("audio/mpeg")
			.field("mpegversion", 1)
			.field("layer", 3)
			.field("rate", config.sample_rate)
			.field("channels", config.channel_count)
			.build(),
		other => bail!("unsupported audio codec: {other:?}"),
	};
	Ok(caps)
}

#[cfg(test)]
mod tests {
	use super::{PumpState, plan_reconcile, relative_pts};
	use moq_net::Timestamp;
	use std::collections::HashMap;

	// The shape type is generic, so the set math can be exercised with a plain integer standing
	// in for (caps, container): equal value == unchanged rendition, different value == reshape.
	fn renditions(pairs: &[(&str, u32)]) -> HashMap<String, u32> {
		pairs.iter().map(|(name, shape)| (name.to_string(), *shape)).collect()
	}

	fn sorted(mut names: Vec<String>) -> Vec<String> {
		names.sort();
		names
	}

	#[test]
	fn plan_reconcile_diffs_by_name_and_shape() {
		// keep: same shape (untouched). gone: removed. added: new. changed: same name, new
		// shape, so it must be both torn down and respawned.
		let active = renditions(&[("keep", 1), ("gone", 1), ("changed", 1)]);
		let desired = renditions(&[("keep", 1), ("changed", 2), ("added", 9)]);

		let plan = plan_reconcile(&desired, &active);
		assert_eq!(sorted(plan.remove), vec!["changed", "gone"]);
		assert_eq!(sorted(plan.add), vec!["added", "changed"]);
	}

	#[test]
	fn plan_reconcile_noops_on_identical_sets() {
		let set = renditions(&[("a", 1), ("b", 2)]);
		let plan = plan_reconcile(&set, &set);
		assert!(plan.remove.is_empty());
		assert!(plan.add.is_empty());
	}

	#[test]
	fn plan_reconcile_empty_desired_removes_all() {
		let active = renditions(&[("a", 1), ("b", 2)]);
		let plan = plan_reconcile(&HashMap::new(), &active);
		assert_eq!(sorted(plan.remove), vec!["a", "b"]);
		assert!(plan.add.is_empty());
	}

	#[test]
	fn plan_reconcile_empty_active_adds_all() {
		let desired = renditions(&[("a", 1), ("b", 2)]);
		let plan = plan_reconcile(&desired, &HashMap::new());
		assert!(plan.remove.is_empty());
		assert_eq!(sorted(plan.add), vec!["a", "b"]);
	}

	#[test]
	fn relative_pts_clamps_backwards_timestamps() {
		let reference = Timestamp::from_millis(2000).unwrap();

		// A frame presenting before the reference (a decode-order B-frame) must clamp to
		// zero, not underflow and panic.
		assert_eq!(
			relative_pts(Timestamp::from_millis(1000).unwrap(), reference),
			gst::ClockTime::ZERO
		);
		assert_eq!(relative_pts(reference, reference), gst::ClockTime::ZERO);

		// A forward timestamp yields the delta.
		assert_eq!(
			relative_pts(Timestamp::from_millis(2500).unwrap(), reference),
			gst::ClockTime::from_mseconds(500)
		);
	}

	/// The pad claim and the teardown race for every pump, and only one may win: a subscription
	/// resolving after a teardown must not publish a rendition the session has finished with and
	/// then yank it without an EOS. The cancel watch alone cannot say which happened, so this is
	/// the state that does.
	#[test]
	fn a_pump_either_goes_live_or_is_cancelled() {
		let cancelled = PumpState::new();
		assert!(cancelled.cancel_before_live());
		assert!(!cancelled.go_live(), "a cancelled pump still claimed a pad");
		assert!(
			!cancelled.cancel_before_live(),
			"a second teardown claimed the same transition"
		);

		let live = PumpState::new();
		assert!(live.go_live());
		assert!(
			!live.cancel_before_live(),
			"a live pump was dropped without its cancel watch"
		);
		assert!(!live.go_live(), "a live pump claimed a second pad");
	}
}

#[cfg(test)]
mod session_tests {
	use std::collections::BTreeMap;
	use std::sync::Mutex;
	use std::sync::atomic::Ordering;
	use std::time::Duration;

	use gst::glib;
	use gst::prelude::*;
	use gst::subclass::prelude::*;
	use hang::catalog::{AudioCodec, AudioConfig, Container, H264, VideoCodec, VideoConfig};
	use tokio::sync::watch;

	use super::{NEXT_VIDEO_PAD_ID, Pads, ResolvedSettings, SessionController, follow_catalog, follow_path};

	/// The pad-id counters are process-global, so a test reading one has to be the only test
	/// allocating while it runs. `cargo test` shares a process across tests (nextest doesn't),
	/// and a panic elsewhere shouldn't cascade, hence the poison recovery.
	static PAD_IDS: Mutex<()> = Mutex::new(());

	fn pad_ids() -> std::sync::MutexGuard<'static, ()> {
		PAD_IDS.lock().unwrap_or_else(|err| err.into_inner())
	}

	/// The pumps push from their own tasks, so the element only has to exist and own pads.
	fn element() -> super::super::MoqSrc {
		gst::init().unwrap();
		glib::Object::new()
	}

	/// Follow `broadcast`'s catalog as one run of a session would, on pads of its own.
	fn run(
		broadcast: moq_net::broadcast::Consumer,
		element: glib::WeakRef<super::super::MoqSrc>,
		mut shutdown: watch::Receiver<bool>,
	) -> tokio::task::JoinHandle<anyhow::Result<()>> {
		super::RUNTIME.spawn(async move {
			let stopping = shutdown.clone();
			follow_catalog(broadcast, &Pads::default(), element, &stopping, &mut shutdown).await
		})
	}

	fn video_rendition() -> VideoConfig {
		let mut config = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0x00,
			level: 0x1f,
			inline: false,
		});
		config.container = Container::Legacy;
		config
	}

	fn audio_rendition() -> AudioConfig {
		let mut config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		config.container = Container::Legacy;
		config
	}

	/// The element's pads of one kind. Matched by prefix because the `%u` suffix comes from a
	/// process-global counter, so a pad's number depends on what else the test binary has run.
	fn pads(element: &super::super::MoqSrc, kind: &str) -> Vec<gst::Pad> {
		element
			.pads()
			.into_iter()
			.filter(|pad| pad.name().starts_with(kind))
			.collect()
	}

	/// Block until a consumer asks the broadcast for a track, returning the request unanswered so
	/// its subscriber stays parked. Bounded so a session that never subscribes fails the test.
	fn await_request(dynamic: &mut moq_net::broadcast::Dynamic) -> moq_net::track::Request {
		super::RUNTIME
			.block_on(async { tokio::time::timeout(Duration::from_secs(10), dynamic.requested_track()).await })
			.expect("no track was ever requested")
			.expect("broadcast closed")
	}

	/// Poll for a pad rather than sleeping a fixed beat: the pumps run on another runtime, so
	/// the only ordering we have is "eventually". Fails the test if it never shows up.
	fn await_pad(element: &super::super::MoqSrc, kind: &str) -> gst::Pad {
		for _ in 0..100 {
			if let Some(pad) = pads(element, kind).into_iter().next() {
				return pad;
			}
			std::thread::sleep(Duration::from_millis(50));
		}
		panic!("no {kind} pad ever appeared");
	}

	/// A catalog can name a rendition its publisher never serves: the browser announces audio a
	/// beat before its video encoder configures, and a subscription only resolves once the track
	/// info arrives. Such a rendition must not hold up the ones that do arrive, nor the catalog
	/// updates that announce them.
	#[test]
	fn a_rendition_nobody_serves_does_not_block_the_others() {
		let _pad_ids = pad_ids();
		let element = element();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		// A live handler is what makes an unserved name park rather than resolve `NotFound`,
		// which is how it behaves over the wire: the publisher just never answers.
		let mut dynamic = broadcast.dynamic();
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();

		// First update announces audio only, and no producer ever answers for it.
		{
			let mut guard = catalog.modify().unwrap();
			guard.audio.renditions = BTreeMap::from([("audio".to_string(), audio_rendition())]);
		}

		let (shutdown, shutdown_rx) = watch::channel(false);
		let consumer = broadcast.consume();
		let weak = element.downgrade();
		let session = run(consumer, weak, shutdown_rx);

		// Wait for the audio subscription before announcing video, and hold the request
		// unanswered. A catalog consumer skips to the newest snapshot, so without this the
		// session could read one update carrying both renditions, and then whether it reached
		// video before parking on audio would come down to `plan.add` ordering.
		let pending = await_request(&mut dynamic);
		assert_eq!(pending.name(), "audio");

		// Second update adds video, backed by a real track so its subscription resolves.
		let _video = broadcast.create_track("video", None).unwrap();
		{
			let mut guard = catalog.modify().unwrap();
			guard.video.renditions = BTreeMap::from([("video".to_string(), video_rendition())]);
		}

		await_pad(&element, "video_");
		assert!(pads(&element, "audio_").is_empty(), "the unserved rendition got a pad");

		let _ = shutdown.send(true);
		super::RUNTIME.block_on(session).unwrap().unwrap();
	}

	/// The same isolation for a rendition the broadcast refuses by name (no handler will ever
	/// serve it) rather than one that merely never answers.
	#[test]
	fn a_refused_rendition_does_not_end_the_session() {
		let _pad_ids = pad_ids();
		let element = element();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();

		// Both renditions in one snapshot, so the result can't hinge on which update the
		// session read: with no handler alive, `audio` resolves `NotFound` rather than parking,
		// and whichever order `plan.add` visits them in, `video` still has to reach a pad and
		// the session still has to end cleanly.
		let _video = broadcast.create_track("video", None).unwrap();
		{
			let mut guard = catalog.modify().unwrap();
			guard.audio.renditions = BTreeMap::from([("audio".to_string(), audio_rendition())]);
			guard.video.renditions = BTreeMap::from([("video".to_string(), video_rendition())]);
		}

		let (shutdown, shutdown_rx) = watch::channel(false);
		let consumer = broadcast.consume();
		let weak = element.downgrade();
		let session = run(consumer, weak, shutdown_rx);

		await_pad(&element, "video_");

		let _ = shutdown.send(true);
		super::RUNTIME.block_on(session).unwrap().unwrap();
	}

	/// A rendition delisted while its pump is still subscribing ends that pump, and answering
	/// the subscription afterwards must not resurrect it into a pad. Which of the two the pump
	/// sees first is the runtime's to decide, so the state machine that refuses the losing side
	/// is covered by `a_pump_either_goes_live_or_is_cancelled` instead.
	#[test]
	fn a_rendition_delisted_while_subscribing_takes_no_pad() {
		let _pad_ids = pad_ids();
		let element = element();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut dynamic = broadcast.dynamic();
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();

		{
			let mut guard = catalog.modify().unwrap();
			guard.video.renditions = BTreeMap::from([("stalled".to_string(), video_rendition())]);
		}

		let (shutdown, shutdown_rx) = watch::channel(false);
		let consumer = broadcast.consume();
		let weak = element.downgrade();
		let session = run(consumer, weak, shutdown_rx);

		// Hold the subscription pending, then drop the rendition from the catalog so its pump
		// is cancelled while it is still waiting.
		let request = await_request(&mut dynamic);
		{
			let mut guard = catalog.modify().unwrap();
			guard.video.renditions.clear();
		}

		// A cancelled pump returns out of its subscribe, dropping the only consumer this request
		// has: that edge says the session reconciled the removal, which a fixed beat can only
		// guess at. Answering before it lands is a pump that legitimately goes live, so the
		// wait is what the assertion below is about.
		super::RUNTIME
			.block_on(async {
				tokio::time::timeout(
					Duration::from_secs(10),
					moq_net::kio::wait(|waiter| request.demand().poll_unused(waiter)),
				)
				.await
			})
			.expect("the cancelled pump never dropped its subscription")
			.expect("the pending request is still open");

		// Only now answer it. The pump is gone and its state is terminal, so no later scheduling
		// can produce a pad.
		let _serving = request.accept(moq_net::track::Info::default());
		assert!(pads(&element, "video_").is_empty(), "a cancelled pump still took a pad");

		let _ = shutdown.send(true);
		super::RUNTIME.block_on(session).unwrap().unwrap();
	}

	/// A publisher can name its tracks and then finish the catalog, which reaches the session as
	/// a snapshot immediately followed by the track closing. The renditions that snapshot named
	/// still have to stream: "hasn't taken a pad yet" says nothing about whether a subscription
	/// is about to resolve, so a closing catalog must not be read as a reason to drop them.
	#[test]
	fn a_closing_catalog_keeps_the_renditions_it_named() {
		let _pad_ids = pad_ids();
		let element = element();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();

		let _video = broadcast.create_track("video", None).unwrap();
		{
			let mut guard = catalog.modify().unwrap();
			guard.video.renditions = BTreeMap::from([("video".to_string(), video_rendition())]);
		}
		catalog.finish().unwrap();

		let (shutdown, shutdown_rx) = watch::channel(false);
		let consumer = broadcast.consume();
		let weak = element.downgrade();
		let session = run(consumer, weak, shutdown_rx);

		await_pad(&element, "video_");

		let _ = shutdown.send(true);
		super::RUNTIME.block_on(session).unwrap().unwrap();
	}

	/// The flip side of the test above: a rendition the publisher reserved and never served is
	/// answerable once the publisher ends the broadcast, which resolves every name it never
	/// filled. That pump must end on its own so the run ends on the media it was serving, and the
	/// session can follow the path to its next start. The pad it served is held, not ended: the
	/// broadcast is offline, not retired.
	#[test]
	fn a_rendition_nobody_served_ends_with_the_broadcast() {
		let _pad_ids = pad_ids();
		let element = element();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();

		// One rendition that streams, and one reserved by name that nobody ever accepts.
		let video = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let _reserved = broadcast.reserve_track("audio").unwrap();
		{
			let mut guard = catalog.modify().unwrap();
			guard.video.renditions = BTreeMap::from([("video".to_string(), video_rendition())]);
			guard.audio.renditions = BTreeMap::from([("audio".to_string(), audio_rendition())]);
		}

		let (shutdown, shutdown_rx) = watch::channel(false);
		let consumer = broadcast.consume();
		let weak = element.downgrade();
		let session = run(consumer, weak, shutdown_rx);

		// Watch for the EOS on the served rendition, before anything can end its track.
		let pad = await_pad(&element, "video_");
		let eos = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
		let seen = eos.clone();
		pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
			if let Some(gst::PadProbeData::Event(event)) = &info.data
				&& event.type_() == gst::EventType::Eos
			{
				seen.store(true, Ordering::Relaxed);
			}
			gst::PadProbeReturn::Ok
		});

		// End the served rendition and the broadcast, which answers for the reserved one.
		catalog.finish().unwrap();
		video.finish().unwrap();
		broadcast.close();

		// No shutdown is sent: the run has to end on the media draining alone.
		super::RUNTIME
			.block_on(async { tokio::time::timeout(Duration::from_secs(10), session).await })
			.expect("the run outlived the renditions it was serving")
			.unwrap()
			.unwrap();
		assert!(!eos.load(Ordering::Relaxed), "an ended broadcast sent EOS");
		assert!(pad.parent().is_some(), "an ended broadcast dropped its pad");
		drop(shutdown);
	}

	/// A publisher that ends one rendition finishes its track and retires it from the catalog,
	/// which is what `moqsink` does on EOS. The two travel on different tracks, so the catalog
	/// update can reach the subscriber before the tail of the media does. It must not cut the pump
	/// short: the pad owes downstream every frame of the track and an EOS.
	///
	/// The subscriber's view of that arrival order is reproduced exactly: the head of the track,
	/// then the catalog update, then the tail and the clean end.
	#[test]
	fn a_retired_rendition_drains_to_eos() {
		const HEAD: u64 = 3;
		const TAIL: u64 = 2;

		let _pad_ids = pad_ids();
		let element = element();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let video = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let _audio = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		{
			let mut guard = catalog.modify().unwrap();
			guard.video.renditions = BTreeMap::from([("video".to_string(), video_rendition())]);
		}

		let (shutdown, shutdown_rx) = watch::channel(false);
		let consumer = broadcast.consume();
		let weak = element.downgrade();
		let session = run(consumer, weak, shutdown_rx);

		// Count what reaches the pad. The pad has no peer, so the probe swallows each buffer, which
		// reports the push as OK.
		let pad = await_pad(&element, "video_");
		let buffers = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
		let eos = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
		let (counted, seen) = (buffers.clone(), eos.clone());
		pad.add_probe(
			gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM,
			move |_, info| match &info.data {
				Some(gst::PadProbeData::Buffer(_)) => {
					counted.fetch_add(1, Ordering::Relaxed);
					gst::PadProbeReturn::Drop
				}
				Some(gst::PadProbeData::Event(event)) if event.type_() == gst::EventType::Eos => {
					seen.store(true, Ordering::Relaxed);
					gst::PadProbeReturn::Ok
				}
				_ => gst::PadProbeReturn::Ok,
			},
		);

		let mut producer = moq_mux::container::Producer::new(
			video,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
		);
		let mut write = |i: u64| {
			producer
				.write(moq_mux::container::Frame {
					timestamp: moq_net::Timestamp::from_micros(i * 33_000).unwrap(),
					payload: bytes::Bytes::from(vec![i as u8; 64]),
					keyframe: i == 0,
					duration: None,
				})
				.unwrap();
		};

		// The head of the track arrives and is delivered.
		for i in 0..HEAD {
			write(i);
		}
		for _ in 0..100 {
			if buffers.load(Ordering::Relaxed) == HEAD {
				break;
			}
			std::thread::sleep(Duration::from_millis(50));
		}
		assert_eq!(
			buffers.load(Ordering::Relaxed),
			HEAD,
			"the head of the track never arrived"
		);

		// The catalog update retiring the rendition arrives next. The catalog and the broadcast
		// stay open, so nothing else can end the pump. The same update lists an audio rendition:
		// its pad appearing proves the session acted on the update before the tail is written.
		{
			let mut guard = catalog.modify().unwrap();
			guard.video.renditions.clear();
			guard.audio.renditions = BTreeMap::from([("audio".to_string(), audio_rendition())]);
		}
		await_pad(&element, "audio_");

		// A pump the update cancelled still has to notice. It takes its pad with it when it does,
		// so the wait is only ever served in full by one that is still reading.
		for _ in 0..10 {
			if pads(&element, "video_").is_empty() {
				break;
			}
			std::thread::sleep(Duration::from_millis(50));
		}

		// The tail of the track and its clean end arrive last.
		for i in HEAD..HEAD + TAIL {
			write(i);
		}
		producer.finish().unwrap();

		// The pump removes its pad on the way out, whichever way it goes.
		for _ in 0..100 {
			if pads(&element, "video_").is_empty() {
				break;
			}
			std::thread::sleep(Duration::from_millis(50));
		}
		assert!(pads(&element, "video_").is_empty(), "the pump never ended");

		let _ = shutdown.send(true);
		super::RUNTIME.block_on(session).unwrap().unwrap();

		assert_eq!(
			buffers.load(Ordering::Relaxed),
			HEAD + TAIL,
			"the retired rendition lost the tail of its track"
		);
		assert!(eos.load(Ordering::Relaxed), "the retired rendition never emitted EOS");
	}

	/// A publisher that delists a rendition without finishing its track and lists it again later
	/// (the browser toggling a source) is resuming the same track. The pump that is still reading
	/// it carries on under the same pad, and a track that ends while listed keeps its pad without
	/// an EOS, since the announcements say whether the broadcast comes back.
	#[test]
	fn a_relisted_rendition_keeps_its_pad() {
		const HEAD: u64 = 3;
		const TAIL: u64 = 2;

		let _pad_ids = pad_ids();
		let element = element();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let video = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let audio = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		{
			let mut guard = catalog.modify().unwrap();
			guard.video.renditions = BTreeMap::from([("video".to_string(), video_rendition())]);
		}

		let added = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
		let count = added.clone();
		element.connect_pad_added(move |_, pad| {
			if pad.name().starts_with("video_") {
				count.fetch_add(1, Ordering::Relaxed);
			}
		});

		let (shutdown, shutdown_rx) = watch::channel(false);
		let consumer = broadcast.consume();
		let weak = element.downgrade();
		let session = run(consumer, weak, shutdown_rx);

		let pad = await_pad(&element, "video_");
		let buffers = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
		let eos = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
		let (counted, seen) = (buffers.clone(), eos.clone());
		pad.add_probe(
			gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM,
			move |_, info| match &info.data {
				Some(gst::PadProbeData::Buffer(_)) => {
					counted.fetch_add(1, Ordering::Relaxed);
					gst::PadProbeReturn::Drop
				}
				Some(gst::PadProbeData::Event(event)) if event.type_() == gst::EventType::Eos => {
					seen.store(true, Ordering::Relaxed);
					gst::PadProbeReturn::Ok
				}
				_ => gst::PadProbeReturn::Ok,
			},
		);

		let mut producer = moq_mux::container::Producer::new(
			video,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
		);
		let mut write = |i: u64| {
			producer
				.write(moq_mux::container::Frame {
					timestamp: moq_net::Timestamp::from_micros(i * 33_000).unwrap(),
					payload: bytes::Bytes::from(vec![i as u8; 64]),
					keyframe: i == 0 || i == HEAD,
					duration: None,
				})
				.unwrap();
		};
		let wait_for = |n: u64| {
			for _ in 0..100 {
				if buffers.load(Ordering::Relaxed) == n {
					break;
				}
				std::thread::sleep(Duration::from_millis(50));
			}
		};

		for i in 0..HEAD {
			write(i);
		}
		wait_for(HEAD);

		// Delist it. The catalog consumer only yields the newest snapshot, so the same update
		// lists an audio rendition: its pad appearing proves the session acted on the delist
		// rather than skipping straight to the relist below.
		{
			let mut guard = catalog.modify().unwrap();
			guard.video.renditions.clear();
			guard.audio.renditions = BTreeMap::from([("audio".to_string(), audio_rendition())]);
		}
		await_pad(&element, "audio_");

		// List it again, unchanged.
		{
			let mut guard = catalog.modify().unwrap();
			guard.video.renditions = BTreeMap::from([("video".to_string(), video_rendition())]);
		}

		// The same track resumes with a new group, then ends.
		for i in HEAD..HEAD + TAIL {
			write(i);
		}
		wait_for(HEAD + TAIL);

		// Everything ends while listed, so the run ends on its own with the pads held.
		producer.finish().unwrap();
		audio.finish().unwrap();
		catalog.finish().unwrap();
		super::RUNTIME
			.block_on(async { tokio::time::timeout(Duration::from_secs(10), session).await })
			.expect("the run outlived its broadcast")
			.unwrap()
			.unwrap();

		assert_eq!(
			added.load(Ordering::Relaxed),
			1,
			"the relisted rendition took a second pad"
		);
		assert_eq!(
			buffers.load(Ordering::Relaxed),
			HEAD + TAIL,
			"the first pad lost frames"
		);
		assert!(!eos.load(Ordering::Relaxed), "a listed rendition's end sent EOS");
		assert!(pad.parent().is_some(), "a listed rendition's end dropped its pad");
		drop(shutdown);
	}

	/// Pipelines link `moqsrc`'s pads by name, so the first video rendition that actually
	/// arrives has to be `video_0`. A rendition announced but never served must not claim that
	/// name and leave the real one on `video_1`, where `s.video_0 ! ...` never links.
	#[test]
	fn an_unserved_rendition_does_not_claim_the_first_pad_name() {
		let _pad_ids = pad_ids();
		let element = element();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let _dynamic = broadcast.dynamic();
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();

		// A video rendition nobody serves, alongside an audio one that arrives. The audio pad
		// is the signal that this update was reconciled, so the second update below is a
		// separate one and the stalled rendition had its chance to claim an id first.
		let _audio = broadcast.create_track("audio", None).unwrap();
		{
			let mut guard = catalog.modify().unwrap();
			guard.video.renditions = BTreeMap::from([("stalled".to_string(), video_rendition())]);
			guard.audio.renditions = BTreeMap::from([("audio".to_string(), audio_rendition())]);
		}

		let first = NEXT_VIDEO_PAD_ID.load(Ordering::Relaxed);
		let (shutdown, shutdown_rx) = watch::channel(false);
		let consumer = broadcast.consume();
		let weak = element.downgrade();
		let session = run(consumer, weak, shutdown_rx);

		await_pad(&element, "audio_");

		let _video = broadcast.create_track("video", None).unwrap();
		{
			let mut guard = catalog.modify().unwrap();
			guard.video.renditions.insert("video".to_string(), video_rendition());
		}

		let pad = await_pad(&element, "video_");
		assert_eq!(pad.name(), format!("video_{first}"));

		let _ = shutdown.send(true);
		super::RUNTIME.block_on(session).unwrap().unwrap();
	}

	/// Settings whose dial goes nowhere, so a stop catches it in flight.
	fn unreachable() -> ResolvedSettings {
		ResolvedSettings {
			url: "https://127.0.0.1:1".parse().unwrap(),
			broadcast: "test".into(),
			tls_disable_verify: false,
		}
	}

	fn is_closed(connection: &moq_tokio::Connection) -> bool {
		connection.poll_closed(&moq_net::kio::Waiter::noop()).is_ready()
	}

	fn started(element: &super::super::MoqSrc) -> (SessionController, moq_tokio::Connection) {
		let session = SessionController::start(unreachable(), element.downgrade()).unwrap();
		let connection = session.connection.clone();
		(session, connection)
	}

	/// A session following `room` on `origin` in place of a relay's, beside a dial for stop to end.
	fn serve(
		element: &super::super::MoqSrc,
		origin: &moq_net::origin::Producer,
	) -> (SessionController, moq_tokio::Connection) {
		let (connection, _) = super::connect(&unreachable()).unwrap();
		let weak = element.downgrade();
		let origin = origin.consume();
		let session = SessionController::spawn(connection.clone(), element.downgrade(), move |shutdown| async move {
			follow_path(&origin, "room", weak, shutdown, std::future::pending()).await
		});
		(session, connection)
	}

	/// An origin on the element's runtime, standing in for a relay's.
	fn origin() -> moq_net::origin::Producer {
		let _rt = super::RUNTIME.enter();
		moq_tokio::origin::spawn()
	}

	/// A broadcast with one video rendition, and what feeds it.
	struct Publisher {
		broadcast: moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::Producer,
		video: moq_mux::container::Producer<moq_mux::catalog::hang::Container>,
		start: std::time::Instant,
	}

	impl Publisher {
		/// Publish at `room` on `origin` under `route`.
		fn new(origin: &moq_net::origin::Producer, route: moq_net::origin::Route, config: VideoConfig) -> Self {
			let publisher = Self::build(origin.create_broadcast("room").unwrap(), config);
			publisher.broadcast.announce(route).unwrap();
			publisher
		}

		/// A broadcast at no path, for a covering route to serve.
		fn standalone(config: VideoConfig) -> Self {
			Self::build(moq_net::broadcast::Info::new().produce(), config)
		}

		fn build(mut broadcast: moq_net::broadcast::Producer, config: VideoConfig) -> Self {
			let mut catalog =
				moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
			let video = broadcast
				.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
				.unwrap();
			{
				let mut guard = catalog.modify().unwrap();
				guard.video.renditions = BTreeMap::from([("video".to_string(), config)]);
			}
			let video = moq_mux::container::Producer::new(
				video,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
			);
			Self {
				broadcast,
				catalog,
				video,
				start: std::time::Instant::now(),
			}
		}

		/// Write a keyframe whose every byte is `tag`, stamped with the time since publishing so a
		/// synced sink renders it on time.
		fn write(&mut self, tag: u8) {
			let elapsed = self.start.elapsed().as_micros() as u64;
			self.video
				.write(moq_mux::container::Frame {
					timestamp: moq_net::Timestamp::from_micros(elapsed).unwrap(),
					payload: bytes::Bytes::from(vec![tag; 64]),
					keyframe: true,
					duration: None,
				})
				.unwrap();
		}
	}

	// A dial still running once the element reached NULL can outlive `main`, and aws-lc aborts the
	// process when a thread asks it for randomness after its exit destructors ran.
	#[test]
	fn stop_returns_after_the_connection_closes() {
		let element = element();
		let (session, connection) = started(&element);
		session.stop(&element);
		assert!(is_closed(&connection));
	}

	/// The state change itself is what an application waits on, so it has to be the one that stops.
	#[test]
	fn paused_to_ready_returns_after_the_connection_closes() {
		let element = element();
		element.set_property("url", "https://127.0.0.1:1");
		element.set_property("broadcast", "test");
		element.set_state(gst::State::Paused).expect("start the source");
		let connection = element
			.imp()
			.session
			.lock()
			.unwrap()
			.as_ref()
			.unwrap()
			.connection
			.clone();
		element.set_state(gst::State::Ready).expect("stop the source");
		assert!(is_closed(&connection));
	}

	// A notify or bus sync handler can stop the element from a runtime worker. With the tasks parked,
	// their wakeups land in that worker's own LIFO slot, which no other worker can steal, so blocking
	// the worker outright would never let them run.
	#[test]
	fn stop_from_a_runtime_worker_does_not_deadlock() {
		let element = element();
		let (session, connection) = started(&element);
		let metrics = super::RUNTIME.metrics();
		// An odd count means that worker is parked.
		while metrics.global_queue_depth() > 0
			|| (0..metrics.num_workers()).any(|worker| metrics.worker_park_unpark_count(worker).is_multiple_of(2))
		{
			std::thread::yield_now();
		}
		let stopping = element.clone();
		super::RUNTIME
			.block_on(super::RUNTIME.spawn(async move { session.stop(&stopping) }))
			.unwrap();
		assert!(is_closed(&connection));
	}

	// An application driving its own executor can reach NULL from inside it, and executors refuse to nest.
	#[test]
	fn stop_inside_another_executor() {
		let element = element();
		let (session, connection) = started(&element);
		futures::executor::block_on(async { session.stop(&element) });
		assert!(is_closed(&connection));
	}

	/// Stop waits for every pump to remove its pad, so a pump held in a push has to be let go
	/// first, or stop never returns.
	#[test]
	fn stop_releases_a_blocked_push() {
		let _pad_ids = pad_ids();
		let element = element();
		let origin = origin();
		let mut publisher = Publisher::new(&origin, Default::default(), video_rendition());
		let (session, connection) = serve(&element, &origin);

		let pad = await_pad(&element, "video_");
		let (blocked, reached) = std::sync::mpsc::channel();
		pad.add_probe(gst::PadProbeType::BLOCK | gst::PadProbeType::BUFFER, move |_, _| {
			let _ = blocked.send(());
			gst::PadProbeReturn::Ok
		});
		publisher.write(0);
		reached
			.recv_timeout(Duration::from_secs(10))
			.expect("the frame never reached the pad");

		session.stop(&element);
		assert!(pad.parent().is_none(), "the pad outlived stop");
		assert!(is_closed(&connection));
	}

	/// A pump whose subscription resolves while stop flushes the pads adds its own just after, with
	/// its cancel still on the way. A push on that pad must not block stop either.
	#[test]
	fn a_pad_added_after_stop_flushed_does_not_block() {
		let _pad_ids = pad_ids();
		let element = element();
		let mut publisher = Publisher::new(&origin(), Default::default(), video_rendition());
		element.connect_pad_added(|_, pad| {
			pad.add_probe(gst::PadProbeType::BLOCK | gst::PadProbeType::BUFFER, |_, _| {
				gst::PadProbeReturn::Ok
			});
		});
		publisher.write(0);

		let (_cancel, cancel) = watch::channel(false);
		let (_shutdown, shutdown) = watch::channel(true);
		let pump = super::Pump {
			element: element.downgrade(),
			kind: super::TrackKind::Video,
			name: "video".into(),
			caps: gst::Caps::new_empty_simple("video/x-h264"),
			track: publisher.broadcast.consume().track("video").unwrap(),
			container: moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
			pads: Pads::default(),
			state: std::sync::Arc::new(super::PumpState::new()),
			cancel,
			shutdown,
		};
		super::RUNTIME
			.block_on(async { tokio::time::timeout(Duration::from_secs(10), super::RUNTIME.spawn(pump.run())).await })
			.expect("a push on a pad added after the flush blocked")
			.unwrap();
	}

	/// A bus sync or pad handler can stop the element from inside a pump's push. Waiting for the
	/// session there would wait on that pump, so stop settles for the connection closing.
	#[test]
	fn stop_from_its_own_pump_does_not_deadlock() {
		let _pad_ids = pad_ids();
		let element = element();
		let origin = origin();
		let mut publisher = Publisher::new(&origin, Default::default(), video_rendition());
		let (session, connection) = serve(&element, &origin);
		*element.imp().session.lock().unwrap() = Some(session);

		let pad = await_pad(&element, "video_");
		let (stopped, returned) = std::sync::mpsc::channel();
		pad.add_probe(gst::PadProbeType::BUFFER, move |pad, _| {
			let element = pad
				.parent_element()
				.unwrap()
				.downcast::<super::super::MoqSrc>()
				.unwrap();
			element.imp().stop_session();
			let _ = stopped.send(());
			gst::PadProbeReturn::Drop
		});
		publisher.write(0);
		returned
			.recv_timeout(Duration::from_secs(10))
			.expect("stop from the pump never returned");
		assert!(is_closed(&connection));
	}

	/// What reached the element's video pads, in order.
	#[derive(Clone, Debug, PartialEq)]
	enum Seen {
		Pad(String),
		/// A new stream on a kept pad.
		Start(String),
		/// A kept pad's caps, by media type.
		Caps(String, String),
		/// A buffer, by its tag.
		Buffer(String, u8),
		Eos(String),
	}

	/// Records what reaches the element's video pads. A new pad's first stream-start and caps
	/// precede its pad-added, so only a kept pad's later ones show.
	#[derive(Clone, Default)]
	struct Recorder(std::sync::Arc<Mutex<Vec<Seen>>>);

	impl Recorder {
		fn attach(element: &super::super::MoqSrc) -> Self {
			let recorder = Self::default();
			let added = recorder.clone();
			element.connect_pad_added(move |_, pad| {
				let name = pad.name().to_string();
				if !name.starts_with("video_") {
					return;
				}
				added.push(Seen::Pad(name.clone()));
				let seen = added.clone();
				pad.add_probe(
					gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM,
					move |_, info| {
						match &info.data {
							Some(gst::PadProbeData::Buffer(buffer)) => {
								seen.push(Seen::Buffer(name.clone(), buffer.map_readable().unwrap()[0]));
								return gst::PadProbeReturn::Drop;
							}
							Some(gst::PadProbeData::Event(event)) => match event.view() {
								gst::EventView::StreamStart(_) => seen.push(Seen::Start(name.clone())),
								gst::EventView::Caps(caps) => {
									let media = caps.caps().structure(0).unwrap().name().to_string();
									seen.push(Seen::Caps(name.clone(), media));
								}
								gst::EventView::Eos(_) => seen.push(Seen::Eos(name.clone())),
								_ => {}
							},
							_ => {}
						}
						gst::PadProbeReturn::Ok
					},
				);
			});
			recorder
		}

		fn push(&self, seen: Seen) {
			self.0.lock().unwrap().push(seen);
		}

		fn seen(&self) -> Vec<Seen> {
			self.0.lock().unwrap().clone()
		}

		fn count(&self, matches: impl Fn(&Seen) -> bool) -> usize {
			self.seen().iter().filter(|seen| matches(seen)).count()
		}

		/// Keep writing `tag` until it reaches a pad, returning that pad. Writing again covers
		/// whichever moment the run subscribes.
		fn written(&self, publisher: &mut Publisher, tag: u8) -> String {
			for _ in 0..200 {
				publisher.write(tag);
				std::thread::sleep(Duration::from_millis(50));
				let reached = self.seen().into_iter().find_map(|seen| match seen {
					Seen::Buffer(pad, seen) if seen == tag => Some(pad),
					_ => None,
				});
				if let Some(pad) = reached {
					return pad;
				}
			}
			panic!("{tag} never reached a pad: {:?}", self.seen());
		}

		/// One video pad, never ended: the session held it throughout.
		fn assert_one_pad_held(&self) {
			let seen = self.seen();
			assert_eq!(self.count(|seen| matches!(seen, Seen::Pad(_))), 1, "{seen:?}");
			assert_eq!(self.count(|seen| matches!(seen, Seen::Eos(_))), 0, "{seen:?}");
		}
	}

	/// A session following `room` on `origin`, in place of a relay's.
	fn follow(
		element: &super::super::MoqSrc,
		origin: &moq_net::origin::Producer,
	) -> (watch::Sender<bool>, tokio::task::JoinHandle<anyhow::Result<()>>) {
		let (shutdown, stopping) = watch::channel(false);
		let (origin, weak) = (origin.consume(), element.downgrade());
		let task = super::RUNTIME
			.spawn(async move { follow_path(&origin, "room", weak, stopping, std::future::pending()).await });
		(shutdown, task)
	}

	fn stop((shutdown, task): (watch::Sender<bool>, tokio::task::JoinHandle<anyhow::Result<()>>)) {
		let _ = shutdown.send(true);
		super::RUNTIME.block_on(task).unwrap().unwrap();
	}

	/// Block until the path's announcements report an event `wanted` accepts.
	fn await_event(follow: &mut moq_net::announce::Follow, wanted: impl Fn(&moq_net::announce::Event) -> bool) {
		let next = async {
			while let Some(event) = follow.next().await {
				if wanted(&event) {
					return;
				}
			}
			panic!("the origin closed");
		};
		super::RUNTIME
			.block_on(async { tokio::time::timeout(Duration::from_secs(10), next).await })
			.expect("the announcement never arrived");
	}

	/// Follow `room` on `origin` past its start. A follower folds everything on hand while nothing
	/// serves the path, so one first polled after the next change would never report it.
	fn announced(origin: &moq_net::origin::Producer) -> moq_net::announce::Follow {
		let mut follow = origin.consume().follow("room").unwrap();
		await_event(&mut follow, |event| matches!(event, moq_net::announce::Event::Start(_)));
		follow
	}

	fn epoch() -> moq_net::origin::Route {
		moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint())
	}

	/// A publisher restarted while the old one stays up switches at once, on the same pad, and
	/// nothing more from the old broadcast gets through.
	fn restart(route: impl Fn() -> moq_net::origin::Route) {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();

		let mut old = Publisher::new(&origin, route(), video_rendition());
		let session = follow(&element, &origin);
		let pad = recorder.written(&mut old, 1);

		let mut new = Publisher::new(&origin, route(), video_rendition());
		assert_eq!(recorder.written(&mut new, 2), pad, "the restart moved to another pad");

		// The old broadcast still writes, but nothing reads it any more.
		old.write(1);
		recorder.written(&mut new, 3);
		stop(session);

		let seen = recorder.seen();
		let switched = seen
			.iter()
			.position(|seen| *seen == Seen::Buffer(pad.clone(), 2))
			.unwrap();
		assert!(
			!seen[switched..].contains(&Seen::Buffer(pad.clone(), 1)),
			"the replaced broadcast kept playing: {seen:?}"
		);
		recorder.assert_one_pad_held();
	}

	#[test]
	fn a_restart_switches_on_the_same_pad() {
		restart(epoch);
	}

	/// Without epochs (moq-lite 06), the newest announcement still replaces the old one.
	#[test]
	fn an_epochless_restart_switches_on_the_same_pad() {
		restart(moq_net::origin::Route::default);
	}

	/// The route serving the path can be a prefix covering it, here the root, and its restart
	/// switches too.
	#[test]
	fn a_covering_route_restart_switches_on_the_same_pad() {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();

		let mut old = Publisher::standalone(video_rendition());
		let serving = std::sync::Arc::new(Mutex::new(old.broadcast.consume()));
		let pool = std::sync::Arc::new(origin.dynamic("", epoch()).unwrap());
		let (handler, answer) = (pool.clone(), serving.clone());
		let handler = super::RUNTIME.spawn(async move {
			while let Ok(request) = handler.requested_broadcast().await {
				request.accept(answer.lock().unwrap().clone());
			}
		});

		let session = follow(&element, &origin);
		let pad = recorder.written(&mut old, 1);

		let mut new = Publisher::standalone(video_rendition());
		*serving.lock().unwrap() = new.broadcast.consume();
		pool.update(epoch()).unwrap();
		assert_eq!(recorder.written(&mut new, 2), pad, "the restart moved to another pad");

		stop(session);
		handler.abort();
		recorder.assert_one_pad_held();
	}

	/// Re-pricing the same instance is an update, which the run rides out without a new stream.
	#[test]
	fn an_update_keeps_the_run() {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();
		let route = epoch();

		let mut publisher = Publisher::new(&origin, route.clone(), video_rendition());
		let mut announced = announced(&origin);
		let session = follow(&element, &origin);
		recorder.written(&mut publisher, 1);

		publisher.broadcast.announce(route.with_cost(5)).unwrap();
		await_event(&mut announced, |event| {
			matches!(event, moq_net::announce::Event::Update(_))
		});
		recorder.written(&mut publisher, 2);
		stop(session);

		let seen = recorder.seen();
		assert_eq!(
			recorder.count(|seen| matches!(seen, Seen::Start(_))),
			0,
			"the update restarted the run: {seen:?}"
		);
		recorder.assert_one_pad_held();
	}

	/// A restart onto another codec keeps the pad and sends it the new caps, so a pipeline that
	/// can't take them fails as not-negotiated instead of going quiet.
	#[test]
	fn a_restart_with_new_caps_keeps_the_pad() {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();

		let mut old = Publisher::new(&origin, epoch(), video_rendition());
		let session = follow(&element, &origin);
		let pad = recorder.written(&mut old, 1);

		let mut vp8 = VideoConfig::new(VideoCodec::VP8);
		vp8.container = Container::Legacy;
		let mut new = Publisher::new(&origin, epoch(), vp8);
		assert_eq!(recorder.written(&mut new, 2), pad, "the new caps moved to another pad");
		stop(session);

		let seen = recorder.seen();
		assert!(seen.contains(&Seen::Caps(pad, "video/x-vp8".into())), "{seen:?}");
		recorder.assert_one_pad_held();
	}

	/// A publisher that stops ends its media, a beat later its catalog, then its announcement.
	/// Its pad is held without an EOS until the path is announced again, and the next broadcast
	/// resumes on it.
	#[test]
	fn an_ended_broadcast_resumes_on_the_same_pad() {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();

		let mut old = Publisher::new(&origin, Default::default(), video_rendition());
		let mut announced = announced(&origin);
		let session = follow(&element, &origin);
		let pad = recorder.written(&mut old, 1);

		old.video.finish().unwrap();
		// The media and the catalog travel apart, so their ends arrive apart too.
		std::thread::sleep(Duration::from_millis(100));
		old.catalog.finish().unwrap();
		drop(old);
		await_event(&mut announced, |event| {
			matches!(event, moq_net::announce::Event::End(_))
		});

		let mut new = Publisher::new(&origin, Default::default(), video_rendition());
		assert_eq!(
			recorder.written(&mut new, 2),
			pad,
			"the next broadcast moved to another pad"
		);
		stop(session);
		recorder.assert_one_pad_held();
	}

	/// The announcement can end before the media does: the route goes while its tracks carry on,
	/// and the source goes later without finishing them. The pad is held from the end, and the
	/// next broadcast resumes on it.
	#[test]
	fn a_source_lost_after_its_end_resumes_on_the_same_pad() {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();

		let mut old = Publisher::new(&origin, Default::default(), video_rendition());
		let mut announced = announced(&origin);
		let session = follow(&element, &origin);
		let pad = recorder.written(&mut old, 1);

		old.broadcast.unannounce();
		await_event(&mut announced, |event| {
			matches!(event, moq_net::announce::Event::End(_))
		});
		drop(old);

		let mut new = Publisher::new(&origin, Default::default(), video_rendition());
		assert_eq!(
			recorder.written(&mut new, 2),
			pad,
			"the next broadcast moved to another pad"
		);
		stop(session);
		recorder.assert_one_pad_held();
	}

	/// A broadcast with no catalog refuses it, which no restart fixes, so the session fails
	/// loudly instead of holding its pads.
	#[test]
	fn a_refused_catalog_fails_the_session() {
		let element = element();
		let origin = origin();
		let broadcast = origin.create_broadcast("room").unwrap();
		broadcast.announce(Default::default()).unwrap();

		let (_shutdown, session) = follow(&element, &origin);
		let err = super::RUNTIME
			.block_on(async { tokio::time::timeout(Duration::from_secs(10), session).await })
			.expect("the refusal held the session")
			.unwrap()
			.expect_err("the refusal did not fail the session");
		assert!(
			matches!(err.downcast_ref::<moq_net::Error>(), Some(moq_net::Error::NotFound)),
			"{err:?}"
		);
	}

	/// A relay on loopback: an origin served over plain TCP to every session, which may also
	/// publish into it.
	struct Relay {
		origin: moq_net::origin::Producer,
		url: url::Url,
		task: tokio::task::JoinHandle<()>,
	}

	impl Relay {
		fn new() -> Self {
			super::RUNTIME.block_on(async {
				let origin = moq_tokio::origin::spawn();
				let mut config = moq_tokio::listen::Config::default();
				config.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
				let mut server = config.init(Default::default()).unwrap().listen().await.unwrap();
				let port = server.tcp_local_addr().unwrap().port();
				let serving = origin.clone();
				let task = tokio::spawn(async move {
					let mut sessions = Vec::new();
					while let Some(request) = server.accept().await {
						let request = request.with_publisher(&serving).with_subscriber(serving.clone());
						if let Ok(session) = request.ok().await {
							sessions.push(session);
						}
					}
				});
				Self {
					origin,
					url: format!("tcp://127.0.0.1:{port}/").parse().unwrap(),
					task,
				}
			})
		}
	}

	impl Drop for Relay {
		fn drop(&mut self) {
			self.task.abort();
		}
	}

	/// `moqsrc` playing `room` from `url`, its first video pad linked by name to a synced sink
	/// that drops anything more than 100ms late.
	fn pipeline(url: &url::Url) -> (gst::Pipeline, gst::Element) {
		static REGISTER: std::sync::Once = std::sync::Once::new();
		gst::init().unwrap();
		REGISTER.call_once(|| {
			gst::Element::register(None, "moqsrc", gst::Rank::NONE, super::super::MoqSrc::static_type()).unwrap();
		});

		let first = NEXT_VIDEO_PAD_ID.load(Ordering::Relaxed);
		let pipeline = gst::parse::launch(&format!(
			"moqsrc name=src url={url} broadcast=room \
			 src.video_{first} ! appsink name=sink sync=true max-lateness=100000000"
		))
		.unwrap()
		.downcast::<gst::Pipeline>()
		.unwrap();
		let sink = pipeline.by_name("sink").unwrap();
		pipeline.set_state(gst::State::Playing).unwrap();
		(pipeline, sink)
	}

	/// Keep writing `tag` until the sink renders it, then check it renders every frame after.
	/// A sink renders a late frame when nothing rendered for a second, so one frame proves
	/// nothing about lateness.
	fn rendered(sink: &gst::Element, publisher: &mut Publisher, tag: u8) {
		const FRAMES: usize = 10;
		let pull = |timeout: Duration| {
			let tagged = sink
				.emit_by_name::<Option<gst::Sample>>("try-pull-sample", &[&(timeout.as_nanos() as u64)])?
				.buffer()
				.is_some_and(|buffer| buffer.map_readable().unwrap()[0] == tag);
			Some(tagged)
		};

		let mut reached = false;
		for _ in 0..200 {
			publisher.write(tag);
			while let Some(tagged) = pull(Duration::from_millis(50)) {
				reached |= tagged;
			}
			if reached {
				break;
			}
		}
		assert!(reached, "{tag} never rendered");

		for _ in 0..FRAMES {
			publisher.write(tag);
			std::thread::sleep(Duration::from_millis(50));
		}
		let mut frames = 0;
		while let Some(tagged) = pull(Duration::from_millis(500)) {
			frames += usize::from(tagged);
		}
		assert!(
			frames >= FRAMES,
			"the sink dropped {tag} as late: {frames} of {FRAMES} rendered"
		);
	}

	/// The pipeline played on one video pad with nothing on the bus.
	fn assert_played_cleanly(pipeline: gst::Pipeline) {
		let error = pipeline.bus().unwrap().pop_filtered(&[gst::MessageType::Error]);
		let video = pipeline
			.by_name("src")
			.unwrap()
			.src_pads()
			.into_iter()
			.filter(|pad| pad.name().starts_with("video_"))
			.count();
		pipeline.set_state(gst::State::Null).unwrap();
		assert!(error.is_none(), "{error:?}");
		assert_eq!(video, 1, "the switch took another pad");
	}

	/// End to end through a relay: a publisher restarted under a new epoch, the old one still up.
	/// The pad linked by name keeps flowing into a synced sink, which renders the new broadcast on
	/// time even though its timestamps start over.
	#[test]
	fn a_restart_renders_through_a_synced_sink() {
		let _pad_ids = pad_ids();
		let relay = Relay::new();

		let mut old = Publisher::new(&relay.origin, epoch(), video_rendition());
		let (pipeline, sink) = pipeline(&relay.url);
		rendered(&sink, &mut old, 1);

		// Long enough that a restarted run placed at the old segment would be a second late.
		std::thread::sleep(Duration::from_secs(1));
		let mut new = Publisher::new(&relay.origin, epoch(), video_rendition());
		rendered(&sink, &mut new, 2);
		assert_played_cleanly(pipeline);
	}

	/// End to end through a relay: the publisher's session closes without finishing its
	/// broadcast, so the relay loses the source. The pads hold until the path is announced again,
	/// and the next broadcast renders on the same pad.
	#[test]
	fn a_closed_publisher_session_resumes_on_the_next_start() {
		let _pad_ids = pad_ids();
		let relay = Relay::new();

		let local = origin();
		let mut old = Publisher::new(&local, Default::default(), video_rendition());
		let session = {
			let _rt = super::RUNTIME.enter();
			moq_tokio::connect::Config::default()
				.init(Default::default())
				.unwrap()
				.with_publisher(local.consume())
				.with_reconnect(false)
				.connect(relay.url.clone())
		};
		let (pipeline, sink) = pipeline(&relay.url);
		rendered(&sink, &mut old, 1);

		let mut announced = announced(&relay.origin);
		session.abort(moq_net::Error::App(1));
		await_event(&mut announced, |event| {
			matches!(event, moq_net::announce::Event::End(_))
		});

		let mut new = Publisher::new(&relay.origin, Default::default(), video_rendition());
		rendered(&sink, &mut new, 2);
		assert_played_cleanly(pipeline);
	}
}
