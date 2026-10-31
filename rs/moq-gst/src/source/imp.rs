use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
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
/// An id is claimed where the pad is created, not where its reader is spawned. A rendition whose
/// subscription never resolves therefore reserves nothing, so it can't leave `video_0` pointing
/// at a pad that will never exist while the rendition that does arrive lands on `video_1`.
static NEXT_VIDEO_PAD_ID: AtomicU64 = AtomicU64::new(0);
static NEXT_AUDIO_PAD_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Default)]
struct Settings {
	url: Option<String>,
	broadcast: Option<String>,
	connection: crate::connection::Settings,
}

#[derive(Debug, Clone)]
struct ResolvedSettings {
	url: url::Url,
	broadcast: String,
	connect: moq_tokio::connect::Config,
	quic: moq_tokio::quic::Config,
}

impl TryFrom<Settings> for ResolvedSettings {
	type Error = anyhow::Error;

	fn try_from(value: Settings) -> Result<Self> {
		let (connect, quic) = value.connection.resolve()?;
		Ok(Self {
			url: url::Url::parse(value.url.as_ref().context("url property is required")?)?,
			broadcast: value
				.broadcast
				.as_ref()
				.context("broadcast property is required")?
				.clone(),
			connect,
			quic,
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
	/// The element whose session task or reader is running, so [`SessionController::stop`] can tell
	/// it was reached from that element's own streaming context.
	static STREAMING: glib::WeakRef<super::MoqSrc>;
}

/// The session task drives everything: it connects, follows the catalog, and
/// runs one [`Reader`] per rendition and instance. The element just starts and
/// stops it. No control-plane channel is needed because readers push to their pads
/// directly from their own task (a source pad's push *is* its streaming thread),
/// so there's nothing to marshal back onto the element.
struct SessionController {
	shutdown: watch::Sender<bool>,
	join: tokio::task::JoinHandle<()>,
	/// The reconnecting connection, held so [`stop`](Self::stop) can end it and wait for it to close.
	connection: moq_tokio::Connection,
	/// The client behind `connection`, closed by [`stop`](Self::stop) so the close reaches the
	/// relay before `gst-launch` exits rather than leaving it to time the connection out.
	client: moq_tokio::Client,
}

impl SessionController {
	fn start(settings: ResolvedSettings, element: glib::WeakRef<super::MoqSrc>) -> Result<Self> {
		let (client, connection, origin) = connect(&settings)?;
		let task_connection = connection.clone();
		let task_element = element.clone();
		Ok(Self::spawn(client, connection, element, move |shutdown| async move {
			run_session(&task_connection, origin, settings.broadcast, task_element, shutdown).await
		}))
	}

	/// Run `session` as this element's session task, reporting its error on the bus.
	fn spawn<F, Fut>(
		client: moq_tokio::Client,
		connection: moq_tokio::Connection,
		element: glib::WeakRef<super::MoqSrc>,
		session: F,
	) -> Self
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
			client,
		}
	}

	/// Stop the session, returning once its readers have removed their pads and the connection closed.
	///
	/// A dial still running once the element reached NULL can outlive `main` (`gst-launch` exits right
	/// after), and aws-lc aborts the process when a thread asks it for randomness after its exit
	/// destructors ran.
	fn stop(self, element: &super::MoqSrc) {
		let _ = self.shutdown.send(true);

		// A reader blocked in a push returns once its pad flushes, the same unlock a GStreamer source
		// gives its streaming thread on stop. A push held up inside a downstream element is released
		// by that element leaving PAUSED, which a pipeline does before it reaches its source.
		for pad in element.src_pads() {
			let _ = pad.set_active(false);
		}

		// Reached from this element's own session task or reader (a bus sync or pad handler), waiting
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

		let Self {
			join,
			connection,
			client,
			..
		} = self;
		crate::block_on(async move {
			// The connection outlives the session so a stop never reads as a dropped connection.
			if !own && let Err(err) = join.await {
				gst::warning!(CAT, "session task ended with error: {err:?}");
			}
			connection.abort(moq_net::Error::Cancel);
			let _ = connection.closed().await;
			client.close().await;
		});
	}
}

/// The dial's configuration: versions and TLS as set, retrying without a budget.
fn connect_config(settings: &ResolvedSettings) -> moq_tokio::connect::Config {
	settings.connect.clone()
}

/// Start the reconnecting dial and the origin its announcements land in.
fn connect(
	settings: &ResolvedSettings,
) -> Result<(moq_tokio::Client, moq_tokio::Connection, moq_net::origin::Consumer)> {
	let config = connect_config(settings);

	// The origin and the connection loop are both spawned tasks.
	let _rt = RUNTIME.enter();
	let origin = moq_tokio::origin::spawn();
	let consumer = origin.consume();
	let quic = settings.quic.clone();
	let client = config.init(quic)?.with_subscriber(origin);
	let connection = client.connect(settings.url.clone());
	Ok((client, connection, consumer))
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
			]
			.into_iter()
			.chain(crate::connection::properties())
			.collect()
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
			name => settings.connection.set(name, value),
		}
	}

	fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
		let settings = self.settings.lock().unwrap();
		match pspec.name() {
			"url" => settings.url.to_value(),
			"broadcast" => settings.broadcast.to_value(),
			name => settings.connection.get(name),
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

/// The identity a rendition plays under: a change to any field restarts its stream on the same
/// pad. Caps cover codec/resolution; the container descriptor covers the wire framing (e.g. legacy
/// -> cmaf); the broadcast and track identify the media, apart from its catalog ID.
#[derive(Clone, PartialEq)]
struct Shape {
	caps: gst::Caps,
	container: hang::catalog::Container,
	track: String,
	broadcast: Option<moq_net::path::Relative>,
	epoch: Option<moq_net::Epoch>,
}

/// A rendition by kind and moq track name: what a pad is kept by.
type Rendition = (TrackKind, String);

/// The session's pads by rendition, and the renditions the latest catalog lists. The pads outlive
/// the readers and runs that stream to them, so a pipeline linked by name (`s.video_0 ! ...`) keeps
/// flowing across a restart.
#[derive(Clone, Default)]
struct Pads(Arc<Mutex<PadTable>>);

#[derive(Default)]
struct PadTable {
	slots: HashMap<Rendition, Slot>,
	/// Only a listed rendition takes a pad, or keeps one once its reader lets go.
	listed: HashSet<Rendition>,
}

struct Slot {
	pad: gst::Pad,
	/// Held by the reader streaming to the pad, so the next one waits for it to let go.
	owner: Arc<tokio::sync::Mutex<()>>,
	/// A retirement waits for the reader to let go.
	retiring: bool,
}

/// A pad for a reader to stream to, from [`Pads::claim`].
enum Claim {
	/// A new pad, already owned, for the reader to add to the element.
	New(gst::Pad, tokio::sync::OwnedMutexGuard<()>),
	/// A kept pad, owned once its previous reader lets go.
	Kept(gst::Pad, Arc<tokio::sync::Mutex<()>>),
	/// No pad: the latest catalog does not list the rendition.
	Unlisted,
}

impl Pads {
	/// The rendition's pad, created if the session has none. `None` once `cancel` fired or the
	/// element is gone.
	fn claim(
		&self,
		element: &glib::WeakRef<super::MoqSrc>,
		rendition: &Rendition,
		cancel: &watch::Receiver<bool>,
	) -> Option<Claim> {
		let obj = element.upgrade()?;
		let mut table = self.0.lock().unwrap();
		// Checked under the lock [`Self::list`] takes, so a reader that an update cancels or delists
		// never claims a pad behind the update's retirements.
		if *cancel.borrow() {
			return None;
		}
		if !table.listed.contains(rendition) {
			return Some(Claim::Unlisted);
		}
		if let Some(slot) = table.slots.get(rendition) {
			return Some(Claim::Kept(slot.pad.clone(), slot.owner.clone()));
		}

		let kind = rendition.0;
		let templ = obj.element_class().pad_template(kind.template_name())?;
		let pad = gst::Pad::builder_from_template(&templ)
			.name(kind.next_pad_name())
			.build();
		let owner = Arc::new(tokio::sync::Mutex::new(()));
		let owned = owner.clone().try_lock_owned().expect("a new lock is free");
		table.slots.insert(
			rendition.clone(),
			Slot {
				pad: pad.clone(),
				owner,
				retiring: false,
			},
		);
		Some(Claim::New(pad, owned))
	}

	/// Whether the latest catalog lists the rendition.
	fn is_listed(&self, rendition: &Rendition) -> bool {
		self.0.lock().unwrap().listed.contains(rendition)
	}

	/// List what the latest catalog names, returning a retirement for each pad it no longer lists.
	/// Each ends its pad with EOS once its reader lets go, unless the rendition is listed again by
	/// then, so a delisted rendition still playing out its track keeps its tail.
	/// Nothing once `cancel` fired, since a later run lists the pads then.
	fn list(
		&self,
		listed: HashSet<Rendition>,
		cancel: &watch::Receiver<bool>,
	) -> Vec<impl Future<Output = ()> + Send + 'static> {
		let mut table = self.0.lock().unwrap();
		if *cancel.borrow() {
			return Vec::new();
		}
		table.listed = listed;
		let PadTable { slots, listed } = &mut *table;
		slots
			.iter_mut()
			.filter(|(rendition, slot)| !slot.retiring && !listed.contains(*rendition))
			.map(|(rendition, slot)| {
				slot.retiring = true;
				let (pads, rendition, owner) = (self.clone(), rendition.clone(), slot.owner.clone());
				async move {
					let _owned = owner.clone().lock_owned().await;
					let slot = {
						let mut table = pads.0.lock().unwrap();
						let PadTable { slots, listed } = &mut *table;
						match slots.get_mut(&rendition) {
							Some(slot) if Arc::ptr_eq(&slot.owner, &owner) => match listed.contains(&rendition) {
								true => {
									slot.retiring = false;
									None
								}
								false => slots.remove(&rendition),
							},
							_ => None,
						}
					};
					if let Some(slot) = slot {
						slot.end(true);
					}
				}
			})
			.collect()
	}

	/// Whether `owner` still guards the rendition's pad, rather than a retirement having ended it.
	fn holds(&self, rendition: &Rendition, owner: &Arc<tokio::sync::Mutex<()>>) -> bool {
		self.0
			.lock()
			.unwrap()
			.slots
			.get(rendition)
			.is_some_and(|slot| Arc::ptr_eq(&slot.owner, owner))
	}

	/// Remove every pad once nothing streams to them any more, after an EOS if `eos`.
	fn release(&self, eos: bool) {
		let slots = std::mem::take(&mut self.0.lock().unwrap().slots);
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

	// A temporary disconnect keeps the origin alive; only a terminal failure closes the loop.
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
	use moq_mux::follow::{Event, Instances};

	let mut follow = Instances::new(origin, path, None)?;
	let pads = Pads::default();
	let mut runs = tokio::task::JoinSet::new();
	// Cancels the latest run.
	let mut latest: Option<watch::Sender<bool>> = None;
	let mut lost = std::pin::pin!(lost);

	tracing::info!(%path, "waiting for broadcast to be announced");
	let result = loop {
		let broadcast = tokio::select! {
			_ = shutdown.changed() => break Ok(()),
			err = &mut lost => break Err(err),
			event = follow.next() => match event {
				Ok(Event::Start(broadcast)) => {
					tracing::info!(%path, epoch = broadcast.info().epoch.as_ref().map(tracing::field::display), "online");
					Some(broadcast)
				}
				Ok(Event::End) => {
					tracing::info!(%path, "offline, holding pads until it returns");
					None
				}
				Err(moq_mux::Error::Moq(moq_net::Error::Closed)) => break Ok(()),
				Err(err) => break Err(anyhow::Error::from(err).context("broadcast refused")),
			},
			joined = runs.join_next(), if !runs.is_empty() => match joined.expect("guarded by is_empty") {
				Ok(Ok(())) => None,
				Ok(Err(err)) => break Err(err),
				Err(err) => break Err(anyhow::Error::from(err).context("run panicked")),
			},
		};

		if let Some(broadcast) = broadcast {
			// Cut over at once: the old run's readers hand their pads back without waiting for their
			// subscriptions to end.
			if let Some(cancel) = latest.take() {
				let _ = cancel.send(true);
			}
			latest = Some(play(
				&mut runs,
				moq_mux::Source::new(origin.clone(), broadcast),
				&pads,
				&element,
				&shutdown,
			));
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

/// Start the instance handed out by the follower until it ends or is replaced, returning its cancel handle.
fn play(
	runs: &mut tokio::task::JoinSet<Result<()>>,
	source: moq_mux::Source,
	pads: &Pads,
	element: &glib::WeakRef<super::MoqSrc>,
	shutdown: &watch::Receiver<bool>,
) -> watch::Sender<bool> {
	let (cancel, mut cancelled) = watch::channel(false);
	let (pads, task_element, shutdown) = (pads.clone(), element.clone(), shutdown.clone());
	runs.spawn_on(
		STREAMING.scope(element.clone(), async move {
			follow_catalog(source, &pads, task_element, &shutdown, &mut cancelled).await
		}),
		RUNTIME.handle(),
	);
	cancel
}

/// Whether a failure is the source going away (its session closing, or its route going), which
/// holds the pads for the next start. A refusal is not: no restart answers a path that names
/// nothing or a token that doesn't grant it, so it fails the session, as a malformed catalog does.
fn source_lost(err: &moq_net::Error) -> bool {
	if matches!(err, moq_net::Error::NotFound | moq_net::Error::Unauthorized) {
		return false;
	}
	let code = match err {
		moq_net::Error::Stream(moq_net::StreamError::Session(code)) => code.clone(),
		_ => moq_net::SessionError::from(err),
	};
	!matches!(
		code,
		moq_net::SessionError::Unauthorized
			| moq_net::SessionError::ProtocolViolation
			| moq_net::SessionError::KeyValueFormatting
			| moq_net::SessionError::InvalidRequestId
	)
}

/// [`source_lost`] for a catalog read, where anything but a transport failure is malformed.
fn catalog_lost(err: &moq_mux::Error) -> bool {
	match err {
		moq_mux::Error::Moq(err) => source_lost(err),
		_ => false,
	}
}

/// Follow one broadcast's catalog, with one [`Follower`] per broadcast its renditions reference.
/// Returns once the catalog closes and the last follower drains, or `cancel` fires. The pads stay
/// with the session, except those of renditions the catalog retired.
async fn follow_catalog(
	source: moq_mux::Source,
	pads: &Pads,
	element: glib::WeakRef<super::MoqSrc>,
	shutdown: &watch::Receiver<bool>,
	cancel: &mut watch::Receiver<bool>,
) -> Result<()> {
	let broadcast = source.broadcast();
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

	// Follow the catalog for the whole run and reconcile against every update, rather than
	// building the renditions once from the first frame. This covers reactive publishers (the
	// browser via @moq/hang) that announce an empty catalog before their encoder configures, then
	// add renditions a beat later, as well as renditions appearing, disappearing, or changing
	// codec/resolution mid-stream.
	let (stop, stopped) = watch::channel(false);
	let playback = Playback {
		element,
		broadcast: broadcast.clone(),
		pads: pads.clone(),
		timeline: Arc::default(),
		shutdown: shutdown.clone(),
		stop: stopped,
	};
	let mut followers: HashMap<moq_net::Path, Followed> = HashMap::new();
	// Woken when a follower runs out of renditions, so the run can let it go without waiting for
	// another catalog update.
	let emptied = Arc::new(tokio::sync::Notify::new());
	// The followers, and the retirements of pads the catalog no longer lists.
	let mut tasks = tokio::task::JoinSet::new();
	let mut catalog_closed = false;
	let mut result = Ok(());

	loop {
		if catalog_closed && tasks.is_empty() {
			break;
		}

		tokio::select! {
			biased;
			// Ended by a restart, or the session ending.
			_ = cancel.changed() => break,
			_ = tasks.join_next(), if !tasks.is_empty() => {}
			_ = emptied.notified() => followers.retain(|_, follower| !follower.plan.is_closed() && !follower.idle()),
			// The guard stops us polling a closed catalog track (which would spin the loop
			// returning None) while the followers drain.
			next = catalog_consumer.next(), if !catalog_closed => match next {
				Ok(Some(catalog)) => reconcile(&catalog, &source, &playback, &mut followers, &mut tasks, &emptied, cancel),
				// Catalog track closed. Don't stop the followers: let each reader reach its natural
				// end, and just stop reconciling while they drain.
				//
				// That includes a reader still resolving its subscription, which is
				// indistinguishable here from one that has simply not been polled yet: a final
				// snapshot naming a track the publisher does serve arrives this way, and stopping on
				// "not live yet" would drop it. Whether a rendition will ever be served is the
				// publisher's to answer, and ending the broadcast is where it does: every name it
				// never served resolves with an error, which fails that reader's subscribe and ends
				// it.
				Ok(None) => catalog_closed = true,
				Err(err) if catalog_lost(&err) => {
					tracing::warn!(%err, "catalog lost, holding pads");
					catalog_closed = true;
				}
				Err(err) => {
					result = Err(err.into());
					break;
				}
			},
		}

		if catalog_closed {
			for (_, follower) in followers.drain() {
				follower.plan.send_modify(|plan| plan.closed = true);
			}
		}
	}

	// Stop every follower, then wait for each to hand its pads back. On the clean catalog-closed
	// exit the tasks already drained, so this is a no-op.
	let _ = stop.send(true);
	while tasks.join_next().await.is_some() {}

	result
}

/// Bring the run in line with `catalog`: list its renditions on the pads, retiring the pads of
/// those it dropped, and hand each referenced broadcast's follower the renditions that read it.
///
/// Infallible by design: every way a single rendition can be unusable (unsupported codec,
/// malformed init, a reference that escapes the root) skips just that rendition, so one bad entry
/// in the catalog can never tear down the ones already streaming.
fn reconcile(
	catalog: &moq_mux::catalog::hang::Catalog,
	source: &moq_mux::Source,
	playback: &Playback,
	followers: &mut HashMap<moq_net::Path, Followed>,
	tasks: &mut tokio::task::JoinSet<()>,
	emptied: &Arc<tokio::sync::Notify>,
	cancel: &watch::Receiver<bool>,
) {
	// Share the players' selection rule: keep an already selected disabled rendition only
	// while no enabled alternative of its kind exists. Pads alone also include idle renditions
	// displaced by an alternative, so derive selection from the plans the readers follow.
	let mut selected = moq_mux::select::Selected::default();
	for follower in followers.values() {
		for (kind, name) in follower.plan.borrow().renditions.keys() {
			match kind {
				TrackKind::Video => selected.video.insert(name.clone()),
				TrackKind::Audio => selected.audio.insert(name.clone()),
			};
		}
	}
	let mut playable = catalog.clone();
	moq_mux::select::Broadcast::default()
		.video(Default::default())
		.audio(Default::default())
		.retain(&mut playable, &selected);

	// Build the desired shape for each rendition. This is deliberately cheap: caps come from the
	// catalog config and the container is just the hang descriptor. The reader parses the wire
	// container (which re-parses the CMAF init) when it starts, so an unchanged rendition costs
	// nothing here. A rendition whose caps we can't build (unsupported codec) is logged and
	// skipped.
	let mut desired: HashMap<Rendition, Shape> = HashMap::new();
	let mut insert = |name: &String,
	                  kind,
	                  caps: Result<gst::Caps>,
	                  container: &hang::catalog::Container,
	                  reference: &dyn hang::catalog::Reference| match caps {
		Ok(caps) => {
			let shape = Shape {
				caps,
				container: container.clone(),
				track: reference.track(name).to_string(),
				broadcast: reference.broadcast().cloned(),
				epoch: reference.epoch().cloned(),
			};
			desired.insert((kind, name.clone()), shape);
		}
		Err(err) => gst::warning!(CAT, "ignoring {kind:?} rendition {name}: {err:?}"),
	};
	for (name, config) in &playable.video.renditions {
		insert(name, TrackKind::Video, video_caps(config), &config.container, config);
	}
	for (name, config) in &playable.audio.renditions {
		insert(name, TrackKind::Audio, audio_caps(config), &config.container, config);
	}

	// A disabled rendition keeps its pad even when an enabled alternative displaced its reader,
	// since a pipeline links pads by name (`s.video_0 ! ...`).
	let disabled: Vec<(Rendition, &dyn hang::catalog::Reference)> = catalog
		.video
		.renditions
		.iter()
		.filter(|(_, config)| !config.enabled)
		.map(|(name, config)| {
			(
				(TrackKind::Video, name.clone()),
				config as &dyn hang::catalog::Reference,
			)
		})
		.chain(
			catalog
				.audio
				.renditions
				.iter()
				.filter(|(_, config)| !config.enabled)
				.map(|(name, config)| {
					(
						(TrackKind::Audio, name.clone()),
						config as &dyn hang::catalog::Reference,
					)
				}),
		)
		.collect();
	// Listed before any follower hears of the update, so a reader it stops can't claim a pad.
	let listed = desired
		.keys()
		.cloned()
		.chain(disabled.iter().map(|(rendition, _)| rendition.clone()))
		.collect();
	for retire in playback.pads.list(listed, cancel) {
		tasks.spawn_on(STREAMING.scope(playback.element.clone(), retire), RUNTIME.handle());
	}

	let mut plans: HashMap<moq_net::Path, Plan> = HashMap::new();
	for (rendition, shape) in desired {
		let Some(path) = source.resolve_reference(shape.broadcast.as_ref()) else {
			gst::warning!(
				CAT,
				"ignoring rendition {}: its broadcast escapes the root",
				rendition.1
			);
			continue;
		};
		plans.entry(path).or_default().renditions.insert(rendition, shape);
	}

	// A broadcast no selected rendition reads any more stops its readers, apart from a delisted
	// one playing out its tail. Its follower stays reachable, with its finished records, while a
	// disabled rendition still names it or a delisted one drains, so a re-enable or relist reaches
	// it rather than a new follower. Once it plays nothing, the run lets it go.
	let held: HashSet<moq_net::Path> = disabled
		.iter()
		.filter_map(|(_, reference)| source.resolve_reference(reference.broadcast()))
		.collect();
	followers.retain(|path, follower| {
		// A follower whose broadcast failed exited, and this update follows it again.
		if follower.plan.is_closed() {
			return false;
		}
		follower.held = held.contains(path);
		follower.plan.send_replace(plans.remove(path).unwrap_or_default());
		!follower.idle()
	});
	for (path, plan) in plans {
		// Follow without waiting: an unavailable sibling must not park the catalog loop and hold
		// up every other rendition. The follower resolves its instances under the run. Every
		// instance is followed, and each rendition checks the epoch it names.
		let shape = plan.renditions.values().next().expect("a plan names a rendition");
		let instances = match source.follow(shape.broadcast.as_ref(), None) {
			Ok(instances) => instances,
			Err(err) => {
				gst::warning!(CAT, "ignoring the renditions of {path}: {err:?}");
				continue;
			}
		};
		let (plan_tx, plan) = watch::channel(plan);
		let (empty_tx, empty) = watch::channel(false);
		let held = held.contains(&path);
		let follower_task = Follower {
			playback: playback.clone(),
			path: path.clone(),
			renditions: HashMap::new(),
			readers: tokio::task::JoinSet::new(),
			current: None,
			resumable: HashMap::new(),
			finished: Finished::default(),
			started: 0,
			next_id: 0,
			empty: empty_tx,
			emptied: emptied.clone(),
		}
		.run(instances, plan);
		tasks.spawn_on(
			STREAMING.scope(playback.element.clone(), follower_task),
			RUNTIME.handle(),
		);
		followers.insert(
			path,
			Followed {
				plan: plan_tx,
				empty,
				held,
			},
		);
	}
}

/// A [`Follower`] as its run reaches it.
struct Followed {
	plan: watch::Sender<Plan>,
	/// Set while it plays no rendition.
	empty: watch::Receiver<bool>,
	/// A disabled rendition names its path, so a re-enable needs its finished records.
	held: bool,
}

impl Followed {
	/// Whether nothing reaches it again: it plays nothing, was asked for nothing, and keeps no
	/// record a re-enable needs. Judged against the plan sent last, so a plan it has yet to take
	/// keeps it.
	fn idle(&self) -> bool {
		!self.held && *self.empty.borrow() && self.plan.borrow().renditions.is_empty()
	}
}

/// What one broadcast run shares with every follower and reader it starts. Pads survive a
/// broadcast switch; its timing does not.
#[derive(Clone)]
struct Playback {
	element: glib::WeakRef<super::MoqSrc>,
	/// The catalog's broadcast. A sibling outlives its closed catalog until it ends.
	broadcast: moq_net::broadcast::Consumer,
	pads: Pads,
	/// Shared by the catalog's own renditions and the first instance of each sibling.
	timeline: Arc<Timeline>,
	/// The session's shutdown, which [`SessionController::stop`] sets before it flushes the pads.
	shutdown: watch::Receiver<bool>,
	/// Set once the run ends, stopping every follower.
	stop: watch::Receiver<bool>,
}

/// The renditions a run asks one [`Follower`] to play.
#[derive(Clone, Default)]
struct Plan {
	renditions: HashMap<Rendition, Shape>,
	/// The catalog closed: the renditions no longer change. The catalog's own follower ends once
	/// its readers drain, and a sibling's keeps following its instances until the catalog's
	/// broadcast ends.
	closed: bool,
}

/// Follows the instances at one path and plays the renditions that read it on each in turn: a
/// start reads them on that instance, an end idles their pads, and another instance taking over
/// restarts each stream on the same pad. The same instance returning resumes its state. The
/// catalog's own broadcast is a single instance that never ends.
///
/// It owns its renditions' readers and the state of the instance they read, so whether a
/// rendition is reading, idle, or playing out its tail is its own state rather than a task's.
struct Follower {
	playback: Playback,
	path: moq_net::Path,
	renditions: HashMap<Rendition, Entry>,
	readers: tokio::task::JoinSet<(Rendition, u64, Exit)>,
	/// The instance last started.
	current: Option<Current>,
	/// The number and timeline of each instance a return would resume: the current one and each
	/// a rendition pins, by epoch. An epochless instance cannot prove it is the one returning.
	resumable: HashMap<moq_net::Epoch, (u64, Arc<Timeline>)>,
	/// Shared with the readers.
	finished: Finished,
	/// How many instances started, numbering each.
	started: u64,
	next_id: u64,
	/// The run's [`Followed::empty`].
	empty: watch::Sender<bool>,
	/// Woken as [`Self::empty`] sets, so the run checks whether to let it go.
	emptied: Arc<tokio::sync::Notify>,
}

/// The instance a [`Follower`] reads.
struct Current {
	/// Released at its end, so an offline sibling holds none of its state.
	instance: Option<moq_net::broadcast::Consumer>,
	/// Its route's, which [`Follower::resumable`] keeps it by.
	epoch: Option<moq_net::Epoch>,
	/// Shared by every rendition reading the instance, so their first frames fix one mapping for
	/// all of them.
	timeline: Arc<Timeline>,
	/// Its number among the follower's instances.
	serial: u64,
}

/// A rendition a [`Follower`] plays.
struct Entry {
	shape: Shape,
	/// Reading the current instance, if it is.
	reader: Option<ReaderHandle>,
	/// Delisted while reading: plays out the instance it reads, then leaves.
	draining: bool,
}

/// A running [`Reader`], cancelled when dropped.
struct ReaderHandle {
	id: u64,
	/// Set once the reader owns the rendition's pad.
	live: Arc<AtomicBool>,
	cancel: watch::Sender<bool>,
}

impl Drop for ReaderHandle {
	fn drop(&mut self) {
		let _ = self.cancel.send(true);
	}
}

impl Follower {
	/// Play the renditions `plan` names on each instance until the plan closes and the last reader
	/// drains, the run stops, or the broadcast fails.
	async fn run(mut self, mut instances: moq_mux::follow::Instances, mut plan: watch::Receiver<Plan>) {
		let mut stop = self.playback.stop.clone();
		// The catalog's own broadcast is ready at once, so its renditions play even when the
		// catalog closed before this follower ran.
		match instances.poll_next(&moq_net::kio::Waiter::noop()) {
			std::task::Poll::Ready(Ok(moq_mux::follow::Event::Start(instance))) => self.start(instance),
			std::task::Poll::Ready(Err(err)) => {
				gst::warning!(CAT, "lost {}: {err:?}", self.path);
				return;
			}
			_ => {}
		}
		let catalog = self.playback.broadcast.clone();
		let sibling = self.path != catalog.info().path;
		let first = plan.borrow_and_update().clone();
		// Taking plan updates, until the catalog closes or stops referencing this path.
		let mut listening = !first.closed;
		// Following instances, which a sibling does past its catalog's close.
		let mut following = listening || sibling;
		self.apply(first.renditions);
		self.play();

		loop {
			// Done once nothing it reads remains, or nothing is left to read and no plan can add
			// any: a closed catalog stops renditions changing.
			if (!following || !listening && self.renditions.is_empty()) && self.readers.is_empty() {
				break;
			}
			let empty = self.renditions.is_empty();
			if self
				.empty
				.send_if_modified(|was| std::mem::replace(was, empty) != empty)
				&& empty
			{
				self.emptied.notify_one();
			}

			tokio::select! {
				biased;
				_ = stop.changed() => break,
				changed = plan.changed(), if listening => match changed {
					Ok(()) => {
						let next = plan.borrow_and_update().clone();
						listening = !next.closed;
						following = listening || sibling;
						let fresh = self.apply(next.renditions);
						// The final plan starts what it adds or reshapes, but restarts no reader
						// that already ended.
						match listening {
							true => self.play(),
							false => self.play_where(|rendition| fresh.contains(rendition)),
						}
					}
					// No rendition reads this path any more.
					Err(_) => {
						(listening, following) = (false, false);
						self.apply(HashMap::new());
					}
				},
				Some(joined) = self.readers.join_next() => {
					if let Ok((rendition, id, exit)) = joined {
						self.exited(rendition, id, exit);
					}
				}
				event = instances.next(), if following => match event {
					Ok(moq_mux::follow::Event::Start(instance)) => {
						gst::info!(CAT, "{} is online, starting its renditions", self.path);
						self.start(instance);
					}
					Ok(moq_mux::follow::Event::End) => {
						gst::info!(CAT, "{} is offline, holding its pads", self.path);
						self.offline();
					}
					Err(err) => {
						gst::warning!(CAT, "lost {}: {err:?}", self.path);
						break;
					}
				},
				// The catalog's broadcast ended after its catalog: the run is offline, so its
				// sibling renditions idle with it.
				_ = catalog.closed(), if following && !listening => {
					following = false;
					self.offline();
				}
			}
		}

		self.renditions.clear();
		while self.readers.join_next().await.is_some() {}
	}

	/// Bring the renditions in line with `wanted`, stopping each one reshaped or gone, and return
	/// those added or reshaped. [`Self::play`] starts the rest.
	fn apply(&mut self, mut wanted: HashMap<Rendition, Shape>) -> HashSet<Rendition> {
		let pads = &self.playback.pads;
		let mut fresh = HashSet::new();
		self.renditions
			.retain(|rendition, entry| match wanted.remove(rendition) {
				Some(shape) => {
					// A new shape hands the pad to a new reader.
					if shape != entry.shape {
						entry.shape = shape;
						entry.reader = None;
						fresh.insert(rendition.clone());
					}
					entry.draining = false;
					true
				}
				// A delisted rendition plays out the instance it reads, then leaves. One that never
				// took its pad, was displaced by an enabled alternative, or now reads another
				// broadcast stops now.
				None => {
					entry.draining = !pads.is_listed(rendition)
						&& entry
							.reader
							.as_ref()
							.is_some_and(|reader| reader.live.load(Ordering::Acquire));
					entry.draining
				}
			});
		for (rendition, shape) in wanted {
			fresh.insert(rendition.clone());
			self.renditions.insert(
				rendition,
				Entry {
					shape,
					reader: None,
					draining: false,
				},
			);
		}
		self.forget();
		fresh
	}

	/// Read `instance`, which took over from any before it.
	fn start(&mut self, instance: moq_net::broadcast::Consumer) {
		// Announced again without an end, its readers carry on.
		if let Some(Current {
			instance: Some(current),
			..
		}) = &self.current
			&& current.is_clone(&instance)
		{
			self.play();
			return;
		}

		// An instance served again keeps its timeline and the tracks it finished.
		let epoch = instance.info().epoch.clone();
		let (serial, timeline) = match epoch.as_ref().and_then(|epoch| self.resumable.get(epoch)) {
			Some((serial, timeline)) => (*serial, timeline.clone()),
			None => {
				self.started += 1;
				// The first instance is the one the run started alongside. A later one's
				// timestamps share no origin with the run's.
				let timeline = match self.started {
					1 => self.playback.timeline.clone(),
					_ => Arc::default(),
				};
				(self.started, timeline)
			}
		};
		if let Some(epoch) = &epoch {
			self.resumable.insert(epoch.clone(), (serial, timeline.clone()));
		}
		self.current = Some(Current {
			instance: Some(instance),
			epoch,
			timeline,
			serial,
		});
		self.forget();
		self.idle();
		self.play();
	}

	/// Drop the state no return can resume: an instance neither current nor pinned by a rendition,
	/// and the records of a retired pad. A disabled rendition keeps its pad, and its records.
	fn forget(&mut self) {
		let pinned: HashSet<&moq_net::Epoch> = self
			.renditions
			.values()
			.filter_map(|entry| entry.shape.epoch.as_ref())
			.chain(self.current.as_ref().and_then(|current| current.epoch.as_ref()))
			.collect();
		self.resumable.retain(|epoch, _| pinned.contains(epoch));
		let serials: HashSet<u64> = self
			.resumable
			.values()
			.map(|(serial, _)| *serial)
			.chain(self.current.as_ref().map(|current| current.serial))
			.collect();
		let pads = &self.playback.pads;
		self.finished
			.retain(|rendition, serial| pads.is_listed(rendition) && serials.contains(&serial));
	}

	/// The instance ended: release it and idle until the next one.
	fn offline(&mut self) {
		if let Some(current) = &mut self.current {
			current.instance = None;
		}
		self.idle();
	}

	/// Stop reading the instance. A draining rendition leaves with it.
	fn idle(&mut self) {
		self.renditions.retain(|_, entry| !entry.draining);
		for entry in self.renditions.values_mut() {
			entry.reader = None;
		}
	}

	/// Start a reader on the current instance for each rendition without one that it may serve.
	fn play(&mut self) {
		self.play_where(|_| true);
	}

	/// [`Self::play`] for the renditions `only` accepts.
	fn play_where(&mut self, only: impl Fn(&Rendition) -> bool) {
		let Some(current) = &self.current else {
			return;
		};
		let Some(instance) = &current.instance else {
			return;
		};
		for (rendition, entry) in &mut self.renditions {
			if entry.reader.is_none() && only(rendition) && serves(instance, entry.shape.epoch.as_ref()) {
				self.next_id += 1;
				let (cancel, cancelled) = watch::channel(false);
				let live = Arc::new(AtomicBool::new(false));
				let reader = Reader {
					playback: self.playback.clone(),
					rendition: rendition.clone(),
					shape: entry.shape.clone(),
					instance: instance.clone(),
					serial: current.serial,
					timeline: current.timeline.clone(),
					finished: self.finished.clone(),
					live: live.clone(),
					cancel: cancelled,
				};
				let (rendition, id) = (rendition.clone(), self.next_id);
				self.readers.spawn_on(
					STREAMING.scope(self.playback.element.clone(), async move {
						let exit = reader.run().await;
						(rendition, id, exit)
					}),
					RUNTIME.handle(),
				);
				entry.reader = Some(ReaderHandle { id, live, cancel });
			}
		}
	}

	/// The rendition's reader `id` exited on its own.
	fn exited(&mut self, rendition: Rendition, id: u64, exit: Exit) {
		let Some(entry) = self.renditions.get_mut(&rendition) else {
			return;
		};
		if entry.reader.as_ref().is_none_or(|reader| reader.id != id) {
			return;
		}
		entry.reader = None;
		if entry.draining {
			self.renditions.remove(&rendition);
		} else if let Exit::Unlisted = exit
			&& self.playback.pads.is_listed(&rendition)
		{
			// Delisted and listed again while it went to claim its pad, which the plan coalesced.
			self.play();
		}
	}
}

/// Whether `instance` may serve a rendition naming `epoch`. An epochless route cannot prove its
/// instance, so only a known mismatch refuses it.
fn serves(instance: &moq_net::broadcast::Consumer, epoch: Option<&moq_net::Epoch>) -> bool {
	match (epoch, &instance.info().epoch) {
		(Some(expected), Some(actual)) => expected == actual,
		_ => true,
	}
}

/// Why a [`Reader`] exited.
enum Exit {
	/// It is done with the instance.
	Done,
	/// The catalog did not list the rendition when it went to claim its pad.
	Unlisted,
}

/// One rendition reading one instance: everything [`Follower::play`] hands a task it spawns.
struct Reader {
	playback: Playback,
	rendition: Rendition,
	shape: Shape,
	instance: moq_net::broadcast::Consumer,
	/// The instance's [`Current::serial`].
	serial: u64,
	timeline: Arc<Timeline>,
	/// The follower's [`Follower::finished`].
	finished: Finished,
	/// Shared with this reader's [`ReaderHandle::live`].
	live: Arc<AtomicBool>,
	cancel: watch::Receiver<bool>,
}

impl Reader {
	/// Subscribe to the track, then stream its frames to the rendition's pad, owning it until the
	/// track ends, errors, its instance stops serving, or `cancel` fires. The pad stays with the
	/// session either way.
	async fn run(self) -> Exit {
		let Reader {
			playback,
			rendition,
			shape,
			instance,
			serial,
			timeline,
			finished,
			live,
			mut cancel,
		} = self;
		let Playback {
			element,
			pads,
			shutdown,
			..
		} = playback;
		let (kind, name) = (rendition.0, &rendition.1);

		// An instance that already played this track to its end on the pad would replay its last
		// group.
		if finished.contains(&rendition, &shape.track, serial) {
			return Exit::Done;
		}

		// Resolves once the publisher answers, with the track info or with an error (which is what
		// ending a broadcast produces for a name nobody served). A publisher that answers neither
		// leaves this waiting, so racing `cancel` keeps such a reader reapable, and holding the
		// wait here keeps it off every other rendition.
		let subscriber = tokio::select! {
			_ = cancel.changed() => return Exit::Done,
			subscriber = async {
				instance
					.track(&shape.track)?
					.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(1)))
					.await
			} => match subscriber {
				Ok(subscriber) => subscriber,
				// The instance went while subscribing: that is its end, not this rendition's.
				Err(_) if instance.is_closed() => return Exit::Done,
				Err(err) => {
					gst::warning!(CAT, "track {name} failed to subscribe: {err:?}");
					return Exit::Done;
				}
			}
		};
		let container = match moq_mux::catalog::hang::Container::new(
			&shape.container,
			match kind {
				TrackKind::Video => moq_mux::container::Kind::Video,
				TrackKind::Audio => moq_mux::container::Kind::Audio,
			},
		) {
			Ok(container) => container,
			Err(err) => {
				gst::warning!(CAT, "ignoring rendition {name}: {err:?}");
				return Exit::Done;
			}
		};
		let mut track = moq_mux::container::Consumer::new(subscriber, container);

		// A pad appears only once its track is live, so a pad downstream can link to is a promise
		// that the rendition is actually flowing, and only a rendition that gets this far claims a
		// pad id. A kept pad starts a new stream on it, with caps pushed even when they differ, so
		// a format downstream can't take fails loudly as not-negotiated rather than going quiet.
		let (pad, _owned) = match pads.claim(&element, &rendition, &cancel) {
			None => return Exit::Done,
			Some(Claim::Unlisted) => return Exit::Unlisted,
			Some(Claim::New(pad, owned)) => {
				let Some(obj) = element.upgrade() else {
					return Exit::Done;
				};
				if pad.set_active(true).is_err() {
					return Exit::Done;
				}
				begin(&pad, name, &shape.caps);
				if obj.add_pad(&pad).is_err() {
					return Exit::Done;
				}
				(pad, owned)
			}
			Some(Claim::Kept(pad, owner)) => {
				let owned = tokio::select! {
					biased;
					_ = cancel.changed() => return Exit::Done,
					owned = owner.clone().lock_owned() => owned,
				};
				// The reader this one replaces records its track as finished before letting go, so
				// one started while it was reading the end (a reshape) can't replay it. A
				// retirement that got the pad first ended it.
				if !pads.holds(&rendition, &owner) || finished.contains(&rendition, &shape.track, serial) {
					return Exit::Done;
				}
				tokio::task::block_in_place(|| begin(&pad, name, &shape.caps));
				(pad, owned)
			}
		};
		live.store(true, Ordering::Release);
		// Stop flushes only the pads it finds, and this one may have been added just after, while
		// this reader's cancel is still on its way. Flushing it here keeps a push from blocking
		// stop.
		if *shutdown.borrow() {
			let _ = pad.set_active(false);
		}

		let mut sent_segment = false;
		// Each reader starts a new stream on the pad, which opusdec decodes from a fresh state.
		let mut pre_skip = PreSkip::new(&shape.caps);
		loop {
			tokio::select! {
				// The follower stopped this reader (the instance changed, the rendition was
				// reshaped or delisted, or the run ended). An end that arrived alongside still
				// finishes the track, or a reader waiting for this pad would replay it.
				_ = cancel.changed() => {
					if ended(&mut track) {
						finished.insert(&rendition, &shape.track, serial);
					}
					return Exit::Done;
				}
				frame = track.read() => match frame {
					Ok(Some(moq_mux::container::Read::Gap { .. } | moq_mux::container::Read::End { .. })) => {}
					Ok(Some(moq_mux::container::Read::Frame { frame, .. })) => {
						let mut buffer = match build_buffer(frame, kind) {
							Ok(buffer) => buffer,
							Err(err) => {
								if let Some(obj) = element.upgrade() {
									gst::element_error!(obj, gst::StreamError::Format, ("track {name} has an invalid timestamp"), ["{err:#}"]);
								}
								return Exit::Done;
							}
						};
						if let Some(pre_skip) = &mut pre_skip
							&& let Err(err) = pre_skip.apply(buffer.get_mut().unwrap())
						{
							if let Some(obj) = element.upgrade() {
								gst::element_error!(obj, gst::StreamError::Format, ("track {name} has an invalid Opus packet"), ["{err:#}"]);
							}
							return Exit::Done;
						}
						let segment = (!sent_segment).then(|| {
							let base = element.upgrade().and_then(|obj| obj.current_running_time()).unwrap_or(gst::ClockTime::ZERO);
							sent_segment = true;
							gst::event::Segment::new(timeline.segment(buffer.pts().unwrap(), base))
						});
						// pad.push() blocks until downstream accepts the buffer (full queues, a
						// clock-synced sink). block_in_place hands our sibling tasks to another
						// worker so a stalled downstream can't pin a runtime thread and starve
						// the session loop or other readers.
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
									gst::element_error!(obj, gst::StreamError::Format, ("track {name} was not negotiated"), ["caps {}", shape.caps]);
								}
								return Exit::Done;
							}
							Err(_) => return Exit::Done,
						}
					}
					// The track stopping with its instance is that instance's end, which idles the
					// pad for the next one rather than ending the rendition.
					Ok(None) | Err(_) if instance.is_closed() => return Exit::Done,
					// Recorded while this reader still owns the pad, before any other can claim it,
					// so a reshape on this instance does not replay it.
					Ok(None) => {
						finished.insert(&rendition, &shape.track, serial);
						return Exit::Done;
					}
					Err(err) => {
						gst::warning!(CAT, "track {name} failed: {err:?}");
						return Exit::Done;
					}
				}
			}
		}
	}
}

/// The track each of a follower's renditions last played to the end on its pad, per instance the
/// follower keeps, by serial. A new subscription to a finished track replays its last group, so a
/// catalog update that still lists the rendition (a publisher's final snapshot lands after its
/// tracks end, or a disable and an enable) must not start it on that pad again. Another instance
/// carries a new track, and a pad that never played the track joins it like any late subscriber.
/// Only the last track is kept, so the records stay bounded by the catalog: a rendition switched
/// back to a track it finished before another replays its last group. Shared with the readers,
/// since a reader that replaces another on its pad learns this only once the other lets go.
#[derive(Clone, Default)]
struct Finished(Arc<Mutex<HashMap<(Rendition, u64), String>>>);

impl Finished {
	/// Record that instance `serial` served the rendition's `track` to its end.
	fn insert(&self, rendition: &Rendition, track: &str, serial: u64) {
		self.0
			.lock()
			.unwrap()
			.insert((rendition.clone(), serial), track.to_string());
	}

	/// Whether instance `serial` already served the rendition's `track` to its end on its pad.
	fn contains(&self, rendition: &Rendition, track: &str, serial: u64) -> bool {
		self.0
			.lock()
			.unwrap()
			.get(&(rendition.clone(), serial))
			.is_some_and(|finished| finished == track)
	}

	/// Keep only the records `keep` accepts.
	fn retain(&self, keep: impl Fn(&Rendition, u64) -> bool) {
		self.0
			.lock()
			.unwrap()
			.retain(|(rendition, serial), _| keep(rendition, *serial));
	}
}

/// The first decoded frame fixes one mapping for every rendition reading an instance.
#[derive(Default)]
struct Timeline(OnceLock<gst::FormattedSegment<gst::ClockTime>>);

impl Timeline {
	fn segment(&self, pts: gst::ClockTime, base: gst::ClockTime) -> &gst::FormattedSegment<gst::ClockTime> {
		self.0.get_or_init(|| {
			let mut segment = gst::FormattedSegment::new();
			segment.set_start(pts);
			segment.set_position(pts);
			segment.set_base(base);
			segment
		})
	}
}

/// Whether the track's end is the next media item, without waiting for it. Markers ready before it
/// are passed over; a frame or anything not ready yet means it has not ended.
fn ended(track: &mut moq_mux::container::Consumer<moq_mux::catalog::hang::Container>) -> bool {
	let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
	loop {
		let mut read = std::pin::pin!(track.read());
		match read.as_mut().poll(&mut cx) {
			std::task::Poll::Ready(Ok(None)) => return true,
			std::task::Poll::Ready(Ok(Some(
				moq_mux::container::Read::Gap { .. } | moq_mux::container::Read::End { .. },
			))) => continue,
			_ => return false,
		}
	}
}

/// Start a new stream on the pad: a fresh stream-start and the rendition's caps. The segment
/// waits for this pad's first buffer, then uses the run's shared mapping.
fn begin(pad: &gst::Pad, name: &str, caps: &gst::Caps) {
	pad.push_event(
		gst::event::StreamStart::builder(name)
			.group_id(gst::GroupId::next())
			.build(),
	);
	pad.push_event(gst::event::Caps::new(caps));
}

/// Preserve the broadcast timestamp at the common nanosecond scale used by GStreamer.
fn build_buffer(frame: moq_mux::container::Frame, kind: TrackKind) -> Result<gst::Buffer> {
	let mut buffer = gst::Buffer::from_slice(frame.payload);
	let buffer_mut = buffer.get_mut().unwrap();

	let pts =
		gst::ClockTime::try_from(Duration::from(frame.timestamp)).context("timestamp exceeds GStreamer clock range")?;
	buffer_mut.set_pts(Some(pts));

	let mut flags = buffer_mut.flags();
	match kind {
		// Video carries the keyframe bit per frame; audio frames are all keyframes.
		TrackKind::Video if frame.keyframe => flags.remove(gst::BufferFlags::DELTA_UNIT),
		TrackKind::Video => flags.insert(gst::BufferFlags::DELTA_UNIT),
		TrackKind::Audio => flags.remove(gst::BufferFlags::DELTA_UNIT),
	}
	buffer_mut.set_flags(flags);

	Ok(buffer)
}

/// The Opus pre-skip moqsrc clips: opusdec trims only what a buffer's `GstAudioClippingMeta`
/// asks for, never the OpusHead's pre-skip, and stamps each output at its input's pts.
struct PreSkip {
	/// The OpusHead's pre-skip, in 48 kHz samples.
	total: u64,
	/// What the next packets still have to clip.
	remaining: u64,
}

impl PreSkip {
	/// The pre-skip of an `audio/x-opus` caps' OpusHead, or `None` for other caps or none to skip.
	fn new(caps: &gst::CapsRef) -> Option<Self> {
		let structure = caps.structure(0).filter(|s| s.name() == "audio/x-opus")?;
		let headers = structure.get_optional::<gst::Array>("streamheader").ok()??;
		let head = headers.first()?.get::<gst::Buffer>().ok()?;
		let head = head.map_readable().ok()?;
		let total = moq_mux::codec::opus::Config::parse(&mut head.as_slice())
			.ok()?
			.pre_skip
			.into();
		(total > 0).then_some(Self {
			total,
			remaining: total,
		})
	}

	/// Clip this packet's share of the pre-skip, and stamp it where its first kept sample plays.
	/// A packet's timestamp is where its decode would start without the pre-skip, so each one plays
	/// the pre-skip earlier, less what it clips itself.
	fn apply(&mut self, buffer: &mut gst::BufferRef) -> Result<()> {
		let mut clip = 0;
		if self.remaining > 0 {
			let samples =
				moq_mux::codec::opus::packet_samples(&buffer.map_readable()?).context("malformed Opus packet")?;
			clip = self.remaining.min(samples.into());
			self.remaining -= clip;
			gst_audio::AudioClippingMeta::add(buffer, gst::format::Default::from_u64(clip), gst::format::Default::ZERO);
		}
		let early = gst::ClockTime::from_nseconds(
			(self.total - clip)
				.mul_div_round(gst::ClockTime::SECOND.nseconds(), 48_000)
				.context("pre-skip overflows the clock")?,
		);
		// Only a packet clipped whole can reach before zero, and it plays nothing.
		buffer.set_pts(buffer.pts().map(|pts| pts.saturating_sub(early)));
		Ok(())
	}
}

fn video_caps(config: &hang::catalog::VideoConfig) -> Result<gst::Caps> {
	use hang::catalog::VideoCodec;

	let caps = match &config.codec {
		VideoCodec::H264(h264) => {
			let mut builder = gst::Caps::builder("video/x-h264").field("alignment", "au");
			if let Some(description) = &config.description {
				builder = builder
					.field("stream-format", if h264.inline { "avc3" } else { "avc" })
					.field("codec_data", gst::Buffer::from_slice(description.clone()));
			} else {
				builder = builder.field("stream-format", "byte-stream");
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
					builder = builder.field("stream-format", "byte-stream");
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

// GStreamer's audio caps carry rate and channels as `int`: an unsigned field never intersects a
// decoder's template, so decodebin finds no decoder.
fn audio_caps(config: &hang::catalog::AudioConfig) -> Result<gst::Caps> {
	let rate = i32::try_from(config.sample_rate).context("sample rate out of range")?;
	let channels = i32::try_from(config.channel_count).context("channel count out of range")?;
	let caps = match &config.codec {
		hang::catalog::AudioCodec::AAC(_) => {
			let mut builder = gst::Caps::builder("audio/mpeg")
				.field("mpegversion", 4)
				.field("rate", rate)
				.field("channels", channels);
			if let Some(description) = &config.description {
				builder = builder
					.field("codec_data", gst::Buffer::from_slice(description.clone()))
					.field("stream-format", "aac");
			} else {
				builder = builder.field("stream-format", "adts");
			}
			builder.build()
		}
		hang::catalog::AudioCodec::Opus => opus_caps(config, rate, channels)?,
		hang::catalog::AudioCodec::Mp3 => gst::Caps::builder("audio/mpeg")
			.field("mpegversion", 1)
			.field("layer", 3)
			.field("rate", rate)
			.field("channels", channels)
			.build(),
		other => bail!("unsupported audio codec: {other:?}"),
	};
	Ok(caps)
}

/// An OpusTags packet (RFC 7845 §5.2) with no vendor and no comments.
const OPUS_TAGS: &[u8] = b"OpusTags\0\0\0\0\0\0\0\0";

/// The `audio/x-opus` caps opusdec and moqsink negotiate on: the stream layout as mapping fields,
/// which decodebin needs to pick a decoder, plus the catalog's OpusHead and an empty OpusTags as the
/// streamheader. opusdec reads the head, and so its output gain, only from a two-buffer streamheader.
fn opus_caps(config: &hang::catalog::AudioConfig, rate: i32, channels: i32) -> Result<gst::Caps> {
	let head = match &config.description {
		Some(description) => moq_mux::codec::opus::Config::parse(&mut description.as_ref())
			.context("Opus description is not an OpusHead")?,
		None => moq_mux::codec::opus::Config::new(config.sample_rate, config.channel_count),
	};
	anyhow::ensure!(
		head.channel_count == config.channel_count,
		"the catalog's {} Opus channels contradict its OpusHead's {}",
		config.channel_count,
		head.channel_count
	);
	let mut builder = gst::Caps::builder("audio/x-opus")
		.field("rate", rate)
		.field("channels", channels);
	builder = match &head.mapping {
		Some(mapping) => builder
			.field("channel-mapping-family", i32::from(mapping.family()))
			.field("stream-count", i32::from(mapping.streams()))
			.field("coupled-count", i32::from(mapping.coupled()))
			.field(
				"channel-mapping",
				gst::Array::new(mapping.table().iter().map(|&c| i32::from(c))),
			),
		// Family 0 is mono or stereo: one stream, coupled when stereo.
		None => {
			anyhow::ensure!(
				(1..=2).contains(&channels),
				"Opus family 0 carries 1 or 2 channels, not {channels}"
			);
			builder
				.field("channel-mapping-family", 0i32)
				.field("stream-count", 1i32)
				.field("coupled-count", channels - 1)
		}
	};
	if let Some(description) = &config.description {
		builder = builder.field(
			"streamheader",
			gst::Array::new([
				gst::Buffer::from_slice(description.clone()),
				gst::Buffer::from_slice(OPUS_TAGS),
			]),
		);
	}
	Ok(builder.build())
}

#[cfg(test)]
mod tests {
	use super::{Timeline, ended};
	use moq_net::Timestamp;

	/// A reader cancelled as its track ends records the end only when no media is left to read,
	/// passing over the markers a publisher closes with.
	#[test]
	fn ended_sees_only_an_end_with_no_media_before_it() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast.create_track("video", hang::container::track_info(0)).unwrap();
		let legacy = || moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let mut producer = moq_mux::container::Producer::new(track, legacy());
		let mut consumer = moq_mux::container::Consumer::new(producer.consume(), legacy());
		assert!(!ended(&mut consumer), "an open track has not ended");

		producer
			.write(moq_mux::container::Frame {
				timestamp: Timestamp::ZERO,
				payload: bytes::Bytes::from_static(&[0]),
				keyframe: true,
				duration: None,
			})
			.unwrap();
		assert!(!ended(&mut consumer), "a frame was still unread");

		producer.discontinuity().unwrap();
		producer.finish().unwrap();
		assert!(ended(&mut consumer), "the closing marker hid the end");
	}

	#[test]
	fn video_caps_preserve_nal_framing() {
		gst::init().unwrap();
		for (codec, format) in [
			("avc1.42001f", "avc"),
			("avc3.42001f", "avc3"),
			("hvc1.1.0.L93.00", "hvc1"),
			("hev1.1.0.L93.00", "hev1"),
		] {
			for description in [None, Some(bytes::Bytes::from_static(b"config"))] {
				let mut config = hang::catalog::VideoConfig::new(codec.parse::<hang::catalog::VideoCodec>().unwrap());
				config.description = description.clone();
				let caps = super::video_caps(&config).unwrap();
				let structure = caps.structure(0).unwrap();
				assert_eq!(
					structure.get::<String>("stream-format").unwrap(),
					if description.is_some() { format } else { "byte-stream" }
				);
				assert_eq!(structure.has_field("codec_data"), description.is_some());
			}
		}
	}

	/// opusdec's templates require the mapping fields as ints, so decodebin finds no decoder without them.
	#[test]
	fn opus_caps_carry_the_stream_layout() {
		gst::init().unwrap();
		for channels in [1, 2] {
			let head = moq_mux::codec::opus::Config::new(48_000, channels).with_pre_skip(312);
			for description in [None, Some(head.encode().unwrap())] {
				let mut config = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, channels);
				config.description = description.clone();
				let caps = super::audio_caps(&config).unwrap();
				let structure = caps.structure(0).unwrap();
				assert_eq!(structure.get::<i32>("rate").unwrap(), 48_000);
				assert_eq!(structure.get::<i32>("channels").unwrap(), channels as i32);
				assert_eq!(structure.get::<i32>("channel-mapping-family").unwrap(), 0);
				assert_eq!(structure.get::<i32>("stream-count").unwrap(), 1);
				assert_eq!(structure.get::<i32>("coupled-count").unwrap(), channels as i32 - 1);
				let headers = structure.get_optional::<gst::Array>("streamheader").unwrap();
				let headers: Option<Vec<Vec<u8>>> = headers.map(|headers| {
					headers
						.iter()
						.map(|header| header.get::<gst::Buffer>().unwrap().map_readable().unwrap().to_vec())
						.collect()
				});
				assert_eq!(
					headers,
					description
						.as_ref()
						.map(|head| vec![head.to_vec(), super::OPUS_TAGS.to_vec()]),
					"opusdec ignores a streamheader without the OpusTags after the OpusHead"
				);
			}
		}
	}

	/// opusdec only clips what a buffer's meta asks, one packet at a time, and stamps what's left
	/// at the packet's pts, so the pre-skip spreads over as many packets as it spans and every
	/// packet moves to where its first kept sample plays.
	#[test]
	fn opus_pre_skip_is_clipped_across_packets() {
		gst::init().unwrap();
		let mut config = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
		assert!(super::PreSkip::new(&super::audio_caps(&config).unwrap()).is_none());
		config.description = Some(
			moq_mux::codec::opus::Config::new(48_000, 2)
				.with_pre_skip(312)
				.encode()
				.unwrap(),
		);
		let caps = super::audio_caps(&config).unwrap();

		// TOC configs 28 and 31: a 2.5 ms (120-sample) and a 20 ms (960-sample) CELT packet.
		for (toc, micros, expected) in [
			(0xe0, 2_500, &[(120, 0), (120, 0), (72, 0), (0, 1_000), (0, 3_500)][..]),
			(0xf8, 20_000, &[(312, 0), (0, 13_500)][..]),
		] {
			let mut pre_skip = super::PreSkip::new(&caps).unwrap();
			for (index, &(clip, pts)) in expected.iter().enumerate() {
				let mut buffer = gst::Buffer::from_slice([toc]);
				let buffer = buffer.get_mut().unwrap();
				buffer.set_pts(gst::ClockTime::from_useconds(index as u64 * micros));
				pre_skip.apply(buffer).unwrap();
				let clipped = buffer.meta::<gst_audio::AudioClippingMeta>().map(|meta| {
					assert_eq!(meta.end(), gst::format::Default::ZERO.into());
					meta.start().value() as u64
				});
				assert_eq!(clipped.unwrap_or(0), clip, "packet {index} of {micros}us");
				assert_eq!(
					buffer.pts(),
					Some(gst::ClockTime::from_useconds(pts)),
					"packet {index} of {micros}us"
				);
			}
		}
	}

	#[test]
	fn the_first_frame_fixes_the_segment_start_and_base() {
		gst::init().unwrap();
		let timeline = Timeline::default();
		let start = gst::ClockTime::from_seconds(10);
		let base = gst::ClockTime::from_seconds(3);
		let first = timeline.segment(start, base);
		assert_eq!(first.start(), Some(start));
		assert_eq!(first.position(), Some(start));
		assert_eq!(first.base(), Some(base));
		assert_eq!(
			timeline.segment(gst::ClockTime::from_seconds(9), gst::ClockTime::from_seconds(8)),
			first
		);
		assert_eq!(
			first.to_running_time(gst::ClockTime::from_seconds(12)),
			Some(gst::ClockTime::from_seconds(5))
		);
		assert_eq!(first.to_running_time(gst::ClockTime::from_seconds(9)), None);
		assert_eq!(
			Timeline::default().segment(start, gst::ClockTime::ZERO).base(),
			Some(gst::ClockTime::ZERO)
		);
	}

	#[test]
	fn an_unrepresentable_timestamp_is_refused() {
		gst::init().unwrap();
		let frame = moq_mux::container::Frame {
			timestamp: Timestamp::from_scale(1 << 60, 1).unwrap(),
			payload: bytes::Bytes::new(),
			keyframe: true,
			duration: None,
		};
		assert!(super::build_buffer(frame, super::TrackKind::Video).is_err());
	}
}

#[cfg(test)]
#[expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "tests read the host clock directly"
)]
mod session_tests {
	use std::collections::{BTreeMap, HashSet};
	use std::sync::atomic::Ordering;
	use std::sync::{Arc, Mutex};
	use std::time::Duration;

	use gst::glib;
	use gst::prelude::*;
	use gst::subclass::prelude::*;
	use hang::catalog::{AudioCodec, AudioConfig, Container, H264, VideoCodec, VideoConfig};
	use tokio::sync::watch;

	use super::{NEXT_VIDEO_PAD_ID, Pads, ResolvedSettings, SessionController, TrackKind, follow_catalog, follow_path};

	/// The pad-id counters are process-global, so a test reading one has to be the only test
	/// allocating while it runs. `cargo test` shares a process across tests (nextest doesn't),
	/// and a panic elsewhere shouldn't cascade, hence the poison recovery.
	static PAD_IDS: Mutex<()> = Mutex::new(());

	fn pad_ids() -> std::sync::MutexGuard<'static, ()> {
		PAD_IDS.lock().unwrap_or_else(|err| err.into_inner())
	}

	/// The readers push from their own tasks, so the element only has to exist and own pads.
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
			let (origin, _driver) = moq_net::origin::Producer::new(Default::default());
			let source = moq_mux::Source::new(origin.consume(), broadcast);
			follow_catalog(source, &Pads::default(), element, &stopping, &mut shutdown).await
		})
	}

	#[test]
	fn malformed_input_is_not_a_lost_source() {
		for err in [
			moq_net::Error::ProtocolViolation,
			moq_net::Error::WrongSize,
			moq_net::Error::Session(moq_net::SessionError::ProtocolViolation),
			moq_net::Error::Session(moq_net::SessionError::KeyValueFormatting),
		] {
			assert!(!super::source_lost(&err), "{err:?}");
		}
		for err in [
			moq_net::Error::Closed,
			moq_net::Error::Stream(moq_net::StreamError::Session(moq_net::SessionError::Cancel)),
			moq_net::Error::Cancel,
		] {
			assert!(super::source_lost(&err), "{err:?}");
		}
	}

	/// Selection follows the reader plans, independently by kind; idle pads do not keep a
	/// displaced rendition selected. Reconcile against a plan receiver without starting readers.
	#[test]
	fn disabled_renditions_follow_the_player_selection_rule() {
		let element = element();
		let (origin, _driver) = moq_net::origin::Producer::new(Default::default());
		let broadcast = moq_net::broadcast::Info::new().produce();
		let source = moq_mux::Source::new(origin.consume(), broadcast.consume());
		let (_stop, stopped) = watch::channel(false);
		let playback = super::Playback {
			element: element.downgrade(),
			broadcast: broadcast.consume(),
			pads: Pads::default(),
			timeline: Arc::default(),
			shutdown: stopped.clone(),
			stop: stopped.clone(),
		};
		let (plan, received) = watch::channel(super::Plan::default());
		let (_empty, empty) = watch::channel(false);
		let mut followers = std::collections::HashMap::from([(
			source.resolve_reference(None).unwrap(),
			super::Followed {
				plan,
				empty,
				held: false,
			},
		)]);
		let mut tasks = tokio::task::JoinSet::new();
		let emptied = Arc::default();
		let mut check = |catalog: &moq_mux::catalog::hang::Catalog, expected: &[(TrackKind, &str)]| {
			super::reconcile(
				catalog,
				&source,
				&playback,
				&mut followers,
				&mut tasks,
				&emptied,
				&stopped,
			);
			assert!(tasks.is_empty(), "the existing follower owns every plan");
			assert_eq!(
				received.borrow().renditions.keys().cloned().collect::<HashSet<_>>(),
				expected.iter().map(|(kind, name)| (*kind, name.to_string())).collect(),
			);
		};
		let mut catalog = moq_mux::catalog::hang::Catalog::default();
		let mut video = video_rendition();
		video.enabled = false;
		let mut audio = audio_rendition();
		audio.enabled = false;
		catalog.video.renditions.insert("main".into(), video);
		catalog.audio.renditions.insert("main".into(), audio);
		check(&catalog, &[]);

		let both = [(TrackKind::Video, "main"), (TrackKind::Audio, "main")];
		catalog.video.renditions.get_mut("main").unwrap().enabled = true;
		catalog.audio.renditions.get_mut("main").unwrap().enabled = true;
		check(&catalog, &both);
		catalog.video.renditions.get_mut("main").unwrap().enabled = false;
		catalog.audio.renditions.get_mut("main").unwrap().enabled = false;
		check(&catalog, &both);

		catalog.video.renditions.insert("alternative".into(), video_rendition());
		let alternative = [(TrackKind::Video, "alternative"), (TrackKind::Audio, "main")];
		check(&catalog, &alternative);
		catalog.video.renditions.get_mut("alternative").unwrap().enabled = false;
		check(&catalog, &alternative);

		catalog.video.renditions.remove("alternative");
		catalog.audio.renditions.remove("main");
		check(&catalog, &[]);
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

	/// Real wire consumers and source pads, with each write acknowledged by a pad probe.
	/// No arrival-time guesses: the first acknowledgement fixes which reader won the run.
	#[test]
	fn audio_and_video_share_a_run_timeline_across_scales_and_restarts() {
		let _pad_ids = pad_ids();
		let element = element();
		let pads = Pads::default();
		let (tx, rx) = std::sync::mpsc::channel();
		element.connect_pad_added(move |_, pad| {
			let name = pad.name().to_string();
			let tx = tx.clone();
			let segment = Mutex::new(gst::FormattedSegment::<gst::ClockTime>::new());
			pad.add_probe(
				gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM,
				move |_, info| {
					if let Some(event) = info.event()
						&& let gst::EventView::Segment(event) = event.view()
					{
						*segment.lock().unwrap() = event.segment().downcast_ref::<gst::ClockTime>().unwrap().clone();
					}
					if let Some(buffer) = info.buffer() {
						tx.send((name.clone(), buffer.pts().unwrap(), segment.lock().unwrap().clone()))
							.unwrap();
						return gst::PadProbeReturn::Drop;
					}
					gst::PadProbeReturn::Ok
				},
			);
		});

		for cmaf in [false, true] {
			for audio_first in [false, true] {
				for other_earlier in [false, true] {
					// A second broadcast uses the held pads but must choose a new reference.
					for reference in [10, 100] {
						let mut broadcast = moq_net::broadcast::Info::new().produce();
						let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
						let mut video = VideoConfig::new(VideoCodec::VP8);
						video.coded_width = Some(16);
						video.coded_height = Some(16);
						let mut audio = audio_rendition();
						let scales = if cmaf { [90_000, 48_000] } else { [1_000_000; 2] };
						if cmaf {
							use moq_mux::container::fmp4::Muxer;
							video.container = Container::Cmaf {
								init: Muxer::video(&video)
									.unwrap()
									.with_timescale(moq_net::Timescale::new(scales[0]).unwrap())
									.unwrap()
									.init()
									.unwrap()
									.unwrap(),
							};
							audio.container = Container::Cmaf {
								init: Muxer::audio(&audio).unwrap().init().unwrap().unwrap(),
							};
						}
						let containers = [
							(&video.container, moq_mux::container::Kind::Video),
							(&audio.container, moq_mux::container::Kind::Audio),
						];
						let mut tracks: Vec<_> = ["video", "audio"]
							.into_iter()
							.zip(containers)
							.zip(scales)
							.map(|((name, (container, kind)), scale)| {
								let track = broadcast
									.create_track(
										name,
										hang::container::track_info(0)
											.with_timescale(moq_net::Timescale::new(scale).unwrap()),
									)
									.unwrap();
								moq_mux::container::Producer::new(
									track,
									moq_mux::catalog::hang::Container::new(container, kind).unwrap(),
								)
							})
							.collect();
						{
							let mut guard = catalog.modify().unwrap();
							guard.video.renditions.insert("video".into(), video);
							guard.audio.renditions.insert("audio".into(), audio);
						}
						let (_shutdown, shutdown) = watch::channel(false);
						let (cancel, mut cancelled) = watch::channel(false);
						let weak = element.downgrade();
						let pads = pads.clone();
						let source = moq_mux::Source::new(origin().consume(), broadcast.consume());
						let task = super::RUNTIME
							.spawn(async move { follow_catalog(source, &pads, weak, &shutdown, &mut cancelled).await });
						let first = usize::from(audio_first);
						let other = 1 - first;
						let mut write = |index: usize, seconds: u64| {
							tracks[index]
								.write(moq_mux::container::Frame {
									timestamp: moq_net::Timestamp::from_scale(seconds * scales[index], scales[index])
										.unwrap(),
									payload: bytes::Bytes::from_static(&[0xf8]),
									keyframe: true,
									duration: Some(
										moq_net::Timestamp::from_scale(scales[index] / 50, scales[index]).unwrap(),
									),
								})
								.unwrap();
							rx.recv_timeout(Duration::from_secs(10))
								.expect("reader did not deliver frame")
						};
						let (name, pts, segment) = write(first, reference);
						assert!(name.starts_with(if audio_first { "audio" } else { "video" }));
						assert_eq!(pts, gst::ClockTime::from_seconds(reference));
						assert_eq!(segment.start(), Some(pts));
						let other_time = if other_earlier { reference - 1 } else { reference + 1 };
						let (_, other_pts, other_segment) = write(other, other_time);
						assert_eq!(other_pts, gst::ClockTime::from_seconds(other_time));
						assert_eq!(other_segment, segment, "pads chose different run timing");
						if other_earlier {
							assert!(other_pts < other_segment.start().unwrap());
							assert_eq!(other_segment.to_running_time(other_pts), None);
						}
						let (_, a, a_segment) = write(0, reference + 2);
						let (_, b, b_segment) = write(1, reference + 2);
						assert_eq!(a_segment.to_running_time(a), b_segment.to_running_time(b));
						cancel.send(true).unwrap();
						super::RUNTIME.block_on(task).unwrap().unwrap();
					}
				}
			}
		}
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
	fn await_request(dynamic: &mut moq_net::broadcast::Requests) -> moq_net::track::Request {
		super::RUNTIME
			.block_on(async { tokio::time::timeout(Duration::from_secs(10), dynamic.requested_track()).await })
			.expect("no track was ever requested")
			.expect("broadcast closed")
	}

	/// Poll for a pad rather than sleeping a fixed beat: the readers run on another runtime, so
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

	#[test]
	fn rendition_subscribes_on_its_referenced_broadcast() {
		let _pad_ids = pad_ids();
		let element = element();
		let origin = origin();
		let catalog_broadcast = origin.create_broadcast("room/catalog").unwrap();
		let mut local = catalog_broadcast.requests();
		let left = origin.create_broadcast("room/left").unwrap();
		let mut remote = left.requests();
		let right = origin.create_broadcast("room/right").unwrap();
		let mut replacement = right.requests();
		let mut catalog =
			moq_mux::catalog::Producer::new(&mut catalog_broadcast.broadcast().clone(), Default::default()).unwrap();
		let mut config = video_rendition();
		config.broadcast = Some(moq_net::path::Relative::new("./left").unwrap());
		config.track = Some("encoded".into());
		catalog.modify().unwrap().video.renditions.insert("view".into(), config);
		catalog_broadcast.announce(Default::default()).unwrap();
		left.announce(Default::default()).unwrap();
		right.announce(Default::default()).unwrap();
		let (stopping, shutdown) = watch::channel(false);
		let consumer = origin.consume();
		let weak = element.downgrade();
		let following = super::RUNTIME
			.spawn(async move { follow_path(&consumer, "room/catalog", weak, shutdown, std::future::pending()).await });
		let (path, request) = super::RUNTIME.block_on(async {
			tokio::select! {
				request = local.requested_track() => ("catalog", request.unwrap()),
				request = remote.requested_track() => ("left", request.unwrap()),
			}
		});
		if path != "left" {
			stopping.send(true).unwrap();
			super::RUNTIME.block_on(following).unwrap().unwrap();
			panic!("the rendition was requested on the catalog's broadcast");
		}
		assert_eq!(request.name(), "encoded");
		// The ID and wire track stay the same; only its broadcast changes.
		catalog
			.modify()
			.unwrap()
			.video
			.renditions
			.get_mut("view")
			.unwrap()
			.broadcast = Some(moq_net::path::Relative::new("./right").unwrap());
		let next = super::RUNTIME
			.block_on(async { tokio::time::timeout(Duration::from_secs(10), replacement.requested_track()).await });
		stopping.send(true).unwrap();
		super::RUNTIME.block_on(following).unwrap().unwrap();
		assert_eq!(
			next.expect("the broadcast change did not replace its reader")
				.unwrap()
				.name(),
			"encoded"
		);
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
		let mut dynamic = broadcast.requests();
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

	/// A rendition delisted while its reader is still subscribing ends that reader, and answering
	/// the subscription afterwards must not resurrect it into a pad. Listing it again starts it
	/// anew, since the reader that left is no longer the rendition's.
	#[test]
	fn a_rendition_delisted_while_subscribing_takes_no_pad_until_relisted() {
		let _pad_ids = pad_ids();
		let element = element();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut dynamic = broadcast.requests();
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();

		{
			let mut guard = catalog.modify().unwrap();
			guard.video.renditions = BTreeMap::from([("stalled".to_string(), video_rendition())]);
		}

		let (shutdown, shutdown_rx) = watch::channel(false);
		let consumer = broadcast.consume();
		let weak = element.downgrade();
		let session = run(consumer, weak, shutdown_rx);

		// Hold the subscription pending, then drop the rendition from the catalog so its reader
		// is cancelled while it is still waiting.
		let request = await_request(&mut dynamic);
		{
			let mut guard = catalog.modify().unwrap();
			guard.video.renditions.clear();
		}

		// A cancelled reader returns out of its subscribe, dropping the only consumer this request
		// has: that edge says the session reconciled the removal, which a fixed beat can only
		// guess at. Answering before it lands is a reader that legitimately goes live, so the
		// wait is what the assertion below is about.
		let mut demand = request.demand();
		let unwanted = async {
			while demand.wanted_changed().await?.is_some() {}
			moq_net::Result::Ok(())
		};
		super::RUNTIME
			.block_on(async { tokio::time::timeout(Duration::from_secs(10), unwanted).await })
			.expect("the cancelled reader never dropped its subscription")
			.expect("the pending request is still open");

		// Only now answer it. The reader is gone, so no later scheduling can produce a pad.
		let _serving = request.accept(moq_net::track::Info::default());
		assert!(
			pads(&element, "video_").is_empty(),
			"a cancelled reader still took a pad"
		);

		catalog.modify().unwrap().video.renditions = BTreeMap::from([("stalled".to_string(), video_rendition())]);
		await_pad(&element, "video_");

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

	/// A rendition the catalog's final snapshot adds plays, even when the snapshot and the close
	/// reach the follower together.
	#[test]
	fn a_closing_catalog_plays_the_rendition_its_final_snapshot_adds() {
		let _pad_ids = pad_ids();
		let element = element();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let _video = broadcast.create_track("video", None).unwrap();
		let _audio = broadcast.create_track("audio", None).unwrap();
		{
			let mut guard = catalog.modify().unwrap();
			guard.video.renditions = BTreeMap::from([("video".to_string(), video_rendition())]);
		}

		let (shutdown, shutdown_rx) = watch::channel(false);
		let session = run(broadcast.consume(), element.downgrade(), shutdown_rx);
		await_pad(&element, "video_");

		{
			let mut guard = catalog.modify().unwrap();
			guard.audio.renditions = BTreeMap::from([("audio".to_string(), audio_rendition())]);
		}
		catalog.finish().unwrap();
		await_pad(&element, "audio_");

		let _ = shutdown.send(true);
		super::RUNTIME.block_on(session).unwrap().unwrap();
	}

	/// The flip side of the test above: a rendition the publisher reserved and never served is
	/// answerable once the publisher ends the broadcast, which resolves every name it never
	/// filled. That reader must end on its own so the run ends on the media it was serving, and the
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
	/// update can reach the subscriber before the tail of the media does. It must not cut the reader
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
		// stay open, so nothing else can end the reader. The same update lists an audio rendition:
		// its pad appearing proves the session acted on the update before the tail is written.
		{
			let mut guard = catalog.modify().unwrap();
			guard.video.renditions.clear();
			guard.audio.renditions = BTreeMap::from([("audio".to_string(), audio_rendition())]);
		}
		await_pad(&element, "audio_");

		// A reader the update cancelled still has to notice. It takes its pad with it when it does,
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

		// The reader removes its pad on the way out, whichever way it goes.
		for _ in 0..100 {
			if pads(&element, "video_").is_empty() {
				break;
			}
			std::thread::sleep(Duration::from_millis(50));
		}
		assert!(pads(&element, "video_").is_empty(), "the reader never ended");

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
	/// (the browser toggling a source) is resuming the same track. The reader that is still reading
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

	/// A publisher's last catalog snapshot can land after its tracks end, such as when finishing
	/// the catalog declares pending timelines. A new subscription to a finished track replays its
	/// last group, so the update must not start the rendition again, even reshaped. A new
	/// rendition over the same track still joins it on a pad of its own.
	#[test]
	fn a_finished_track_is_not_replayed_after_a_catalog_update() {
		const FRAMES: u64 = 3;

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
		catalog.modify().unwrap().video.renditions = BTreeMap::from([("video".to_string(), video_rendition())]);

		// Count what reaches the first video pad once it is added. A reader adds a new pad from the
		// signal, before it pushes anything, so every buffer and every later stream is seen.
		let buffers = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
		let restarts = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
		let (counted, restarted) = (buffers.clone(), restarts.clone());
		let probed = std::sync::atomic::AtomicBool::new(false);
		let (added, mut adds) = tokio::sync::mpsc::unbounded_channel();
		element.connect_pad_added(move |_, pad| {
			if pad.name().starts_with("video_") && !probed.swap(true, Ordering::Relaxed) {
				let (counted, restarted) = (counted.clone(), restarted.clone());
				pad.add_probe(
					gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM,
					move |_, info| match &info.data {
						Some(gst::PadProbeData::Buffer(_)) => {
							counted.fetch_add(1, Ordering::Relaxed);
							gst::PadProbeReturn::Drop
						}
						Some(gst::PadProbeData::Event(event)) if event.type_() == gst::EventType::StreamStart => {
							restarted.fetch_add(1, Ordering::Relaxed);
							gst::PadProbeReturn::Ok
						}
						_ => gst::PadProbeReturn::Ok,
					},
				);
			}
			added.send(pad.name().to_string()).unwrap();
		});

		let pads = Pads::default();
		let (_shutdown, shutdown) = watch::channel(false);
		let (_cancel, mut cancelled) = watch::channel(false);
		let session = {
			let pads = pads.clone();
			let weak = element.downgrade();
			let source = moq_mux::Source::new(origin().consume(), broadcast.consume());
			super::RUNTIME.spawn(async move { follow_catalog(source, &pads, weak, &shutdown, &mut cancelled).await })
		};
		let pad = super::RUNTIME.block_on(adds.recv()).unwrap();
		assert!(pad.starts_with("video_"), "{pad} was added first");

		let mut producer = moq_mux::container::Producer::new(
			video,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
		);
		for i in 0..FRAMES {
			producer
				.write(moq_mux::container::Frame {
					timestamp: moq_net::Timestamp::from_micros(i * 33_000).unwrap(),
					payload: bytes::Bytes::from(vec![i as u8; 64]),
					keyframe: i == 0,
					duration: None,
				})
				.unwrap();
		}
		producer.finish().unwrap();

		// The reader owns its pad until it has pushed the whole track and read its end.
		let owner = pads.0.lock().unwrap().slots[&(TrackKind::Video, "video".to_string())]
			.owner
			.clone();
		drop(super::RUNTIME.block_on(owner.lock_owned()));
		assert_eq!(buffers.load(Ordering::Relaxed), FRAMES, "the track never played");

		// The update reshapes the finished rendition over the same track, which hands its pad to a
		// new reader, and lists a second rendition of that track. Its audio pad proves the session
		// acted on it.
		{
			let mut guard = catalog.modify().unwrap();
			guard.video.renditions.get_mut("video").unwrap().description = Some(bytes::Bytes::from_static(&[1]));
			let mut again = video_rendition();
			again.track = Some("video".into());
			guard.video.renditions.insert("again".into(), again);
			guard.audio.renditions = BTreeMap::from([("audio".to_string(), audio_rendition())]);
		}
		let mut added: Vec<String> =
			super::RUNTIME.block_on(async { vec![adds.recv().await.unwrap(), adds.recv().await.unwrap()] });
		added.sort();
		assert!(
			added[0].starts_with("audio_") && added[1].starts_with("video_"),
			"the update added {added:?}"
		);

		// The run ends only once every reader drains, a replaying one included.
		audio.finish().unwrap();
		catalog.finish().unwrap();
		super::RUNTIME.block_on(session).unwrap().unwrap();

		assert_eq!(restarts.load(Ordering::Relaxed), 0, "the finished track started again");
		assert_eq!(buffers.load(Ordering::Relaxed), FRAMES, "the finished track replayed");
	}

	/// Pipelines link `moqsrc`'s pads by name, so the first video rendition that actually
	/// arrives has to be `video_0`. A rendition announced but never served must not claim that
	/// name and leave the real one on `video_1`, where `s.video_0 ! ...` never links.
	#[test]
	fn an_unserved_rendition_does_not_claim_the_first_pad_name() {
		let _pad_ids = pad_ids();
		let element = element();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let _dynamic = broadcast.requests();
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
			connect: crate::connection::Settings::default().resolve().unwrap().0,
			quic: Default::default(),
		}
	}

	/// The source retries without a budget, so an outage of any length is ridden out
	/// (`relay_loss_reconnects_after_refused_redials` covers the redial itself).
	#[test]
	fn the_source_retries_without_a_budget() {
		let config = super::connect_config(&unreachable());
		assert_eq!(
			config.backoff.timeout,
			Some((Duration::ZERO).into()),
			"zero is unlimited"
		);
	}

	/// `quic-keylog` reaches the client's QUIC config, which refuses it without `SSLKEYLOGFILE`.
	#[test]
	fn keylog_reaches_the_client() {
		if !test_env::Env::without(&["SSLKEYLOGFILE"]).isolate() {
			return;
		}
		let mut connection = crate::connection::Settings::default();
		connection.set("quic-keylog", &true.to_value());
		let keylog = ResolvedSettings {
			quic: connection.resolve().unwrap().1,
			..unreachable()
		};
		let err = super::connect(&keylog)
			.err()
			.expect("a keylog without SSLKEYLOGFILE must be refused");
		assert!(err.to_string().contains("SSLKEYLOGFILE"), "{err}");
		assert!(super::connect(&unreachable()).is_ok());
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
		let (client, connection, _) = super::connect(&unreachable()).unwrap();
		let weak = element.downgrade();
		let origin = origin.consume();
		let session = SessionController::spawn(
			client,
			connection.clone(),
			element.downgrade(),
			move |shutdown| async move { follow_path(&origin, "room", weak, shutdown, std::future::pending()).await },
		);
		(session, connection)
	}

	/// An origin on the element's runtime, standing in for a relay's.
	fn origin() -> moq_net::origin::Producer {
		let _rt = super::RUNTIME.enter();
		moq_tokio::origin::spawn()
	}

	/// A broadcast with one video rendition, and what feeds it.
	struct Publisher {
		publication: Option<moq_net::origin::Publication>,
		broadcast: moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::Producer,
		video: moq_mux::container::Producer<moq_mux::catalog::hang::Container>,
		start: std::time::Instant,
	}

	impl Publisher {
		/// Publish at `room` on `origin` under `route`.
		fn new(origin: &moq_net::origin::Producer, route: moq_net::origin::Route, config: VideoConfig) -> Self {
			let publication = origin.create_broadcast("room").unwrap();
			let mut publisher = Self::build(publication.broadcast().clone(), config);
			publication.announce(route).unwrap();
			publisher.publication = Some(publication);
			publisher
		}

		/// The `cam` broadcast a rendition of `room` references as `./cam`, under `route`.
		fn sibling(origin: &moq_net::origin::Producer, route: moq_net::origin::Route) -> Self {
			let publication = origin.create_broadcast("cam").unwrap();
			let mut publisher = Self::build(publication.broadcast().clone(), video_rendition());
			publication.announce(route).unwrap();
			publisher.publication = Some(publication);
			publisher
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
				publication: None,
				broadcast,
				catalog,
				video,
				start: std::time::Instant::now(),
			}
		}

		/// Write a keyframe whose every byte is `tag`, stamped with the time since publishing so a
		/// synced sink renders it on time.
		fn write(&mut self, tag: u8) {
			let frame = self.frame(tag);
			self.video.write(frame).unwrap();
		}

		/// The keyframe [`Self::write`] writes for `tag`.
		fn frame(&self, tag: u8) -> moq_mux::container::Frame {
			let elapsed = self.start.elapsed().as_micros() as u64;
			moq_mux::container::Frame {
				timestamp: moq_net::Timestamp::from_micros(elapsed).unwrap(),
				payload: bytes::Bytes::from(vec![tag; 64]),
				keyframe: true,
				duration: None,
			}
		}

		/// Move the timestamps of later frames `by` ahead.
		fn skip(&mut self, by: Duration) {
			self.start -= by;
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

	/// Stop waits for every reader to remove its pad, so a reader held in a push has to be let go
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

	/// A reader whose subscription resolves while stop flushes the pads adds its own just after,
	/// with its cancel still on the way. A push on that pad must not block stop either.
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

		let rendition = (TrackKind::Video, "video".to_string());
		let (_cancel, cancel) = watch::channel(false);
		let (_shutdown, shutdown) = watch::channel(true);
		let (_stop, stop) = watch::channel(false);
		let pads = Pads::default();
		assert!(pads.list(HashSet::from([rendition.clone()]), &cancel).is_empty());
		let instance = publisher.broadcast.consume();
		let reader = super::Reader {
			playback: super::Playback {
				element: element.downgrade(),
				broadcast: instance.clone(),
				pads,
				timeline: Arc::default(),
				shutdown,
				stop,
			},
			rendition,
			shape: super::Shape {
				caps: gst::Caps::new_empty_simple("video/x-h264"),
				container: Container::Legacy,
				track: "video".into(),
				broadcast: None,
				epoch: None,
			},
			instance,
			serial: 1,
			timeline: Arc::default(),
			finished: super::Finished::default(),
			live: Arc::default(),
			cancel,
		};
		super::RUNTIME
			.block_on(async { tokio::time::timeout(Duration::from_secs(10), super::RUNTIME.spawn(reader.run())).await })
			.expect("a push on a pad added after the flush blocked")
			.unwrap();
	}

	/// A bus sync or pad handler can stop the element from inside a reader's push. Waiting for the
	/// session there would wait on that reader, so stop settles for the connection closing.
	#[test]
	fn stop_from_its_own_reader_does_not_deadlock() {
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
			.expect("stop from the reader never returned");
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
		/// A segment, by its start.
		Segment(String, i64),
	}

	/// Records what reaches the element's video pads. A new pad's first stream-start and caps
	/// precede its pad-added, so only a kept pad's later ones show.
	#[derive(Clone, Default)]
	struct Recorder(Arc<(Mutex<Vec<Seen>>, std::sync::Condvar)>);

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
								gst::EventView::Segment(segment) => {
									seen.push(Seen::Segment(name.clone(), segment.segment().start().value()))
								}
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
			let (recorded, changed) = &*self.0;
			recorded.lock().unwrap().push(seen);
			changed.notify_all();
		}

		fn seen(&self) -> Vec<Seen> {
			self.0.0.lock().unwrap().clone()
		}

		fn count(&self, matches: impl Fn(&Seen) -> bool) -> usize {
			self.seen().iter().filter(|seen| matches(seen)).count()
		}

		/// Block until what reached the pads gives `done` an answer, returning it. Bounded so a
		/// run that never delivers fails the test with what it saw.
		fn wait<T>(&self, what: &str, done: impl Fn(&[Seen]) -> Option<T>) -> T {
			let (recorded, changed) = &*self.0;
			let (seen, _) = changed
				.wait_timeout_while(recorded.lock().unwrap(), Duration::from_secs(10), |seen| {
					done(seen).is_none()
				})
				.unwrap();
			done(&seen).unwrap_or_else(|| panic!("{what}: {seen:?}"))
		}

		/// Write `tag` once and block until it reaches a pad, returning that pad. A track keeps
		/// its newest group for a late subscriber, so one write reaches the run whenever it
		/// subscribes, and a pad never sees `tag` twice unless the run replays it.
		fn written(&self, publisher: &mut Publisher, tag: u8) -> String {
			publisher.write(tag);
			self.wait(&format!("{tag} never reached a pad"), |seen| {
				reached(seen, tag).into_iter().next()
			})
		}

		/// One video pad, never ended: the session held it throughout.
		fn assert_one_pad_held(&self) {
			let seen = self.seen();
			assert_eq!(self.count(|seen| matches!(seen, Seen::Pad(_))), 1, "{seen:?}");
			assert_eq!(self.count(|seen| matches!(seen, Seen::Eos(_))), 0, "{seen:?}");
		}
	}

	/// The pads `tag` reached, in the order it first reached each.
	fn reached(seen: &[Seen], tag: u8) -> Vec<String> {
		let mut pads = Vec::new();
		for seen in seen {
			if let Seen::Buffer(pad, seen) = seen
				&& *seen == tag
				&& !pads.contains(pad)
			{
				pads.push(pad.clone());
			}
		}
		pads
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
	fn await_event(follow: &mut moq_net::announce::Consumer, wanted: impl Fn(&moq_net::announce::Event) -> bool) {
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

	/// Block until someone reads the publisher's video, or until nobody does once `wanted` is false.
	fn await_demand(publisher: &Publisher, wanted: bool) {
		let mut demand = publisher.video.demand();
		let changed = async { while demand.wanted_changed().await.expect("the track closed").is_some() != wanted {} };
		super::RUNTIME
			.block_on(async { tokio::time::timeout(Duration::from_secs(10), changed).await })
			.expect("the demand never changed");
	}

	/// The announcements of the routes covering `path` on `origin`.
	fn watch(origin: &moq_net::origin::Producer, path: &str) -> moq_net::announce::Consumer {
		let path = moq_net::Patterns::from(moq_net::Pattern::literal(path).unwrap());
		origin.consume().scope("", &path).unwrap().announced()
	}

	/// Follow `room` on `origin` past its start.
	fn announced(origin: &moq_net::origin::Producer) -> moq_net::announce::Consumer {
		let mut follow = watch(origin, "room");
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

	/// The route serving the path can be a prefix covering it, here the root, and a restart of
	/// its answer switches too.
	#[test]
	fn a_covering_route_restart_switches_on_the_same_pad() {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();

		let claim = origin.requests("").unwrap();
		let (answers, answered) = std::sync::mpsc::channel();
		let handler = super::RUNTIME.spawn(async move {
			while let Ok(request) = claim.requested_broadcast().await {
				let prepared = request.prepare(moq_net::Epoch::mint()).unwrap();
				let answer = Publisher::build(prepared.broadcast().clone(), video_rendition());
				prepared.publish().unwrap();
				answers.send(answer).unwrap();
			}
		});

		let session = follow(&element, &origin);
		let mut old = answered
			.recv_timeout(Duration::from_secs(5))
			.expect("the claim answered");
		let pad = recorder.written(&mut old, 1);

		let mut new = Publisher::new(&origin, epoch(), video_rendition());
		assert_eq!(recorder.written(&mut new, 2), pad, "the restart moved to another pad");

		stop(session);
		handler.abort();
		recorder.assert_one_pad_held();
	}

	/// A claim that never answers must not hold the reader on a stale resolution once a
	/// replacement serves the path. Its late refusal cannot end the chosen run.
	#[test]
	fn a_replacement_supersedes_a_pending_broadcast_request() {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();
		let pool = origin.requests("").unwrap();
		let session = follow(&element, &origin);
		let pending = super::RUNTIME.block_on(async {
			tokio::time::timeout(Duration::from_secs(10), pool.requested_broadcast())
				.await
				.unwrap()
				.unwrap()
		});
		let mut new = Publisher::new(&origin, epoch(), video_rendition());
		recorder.written(&mut new, 2);
		pending.reject(moq_net::Error::Unauthorized);
		recorder.written(&mut new, 3);
		stop(session);
		recorder.assert_one_pad_held();
	}

	/// A `room` whose video rendition references the `cam` sibling, naming `epoch` when given.
	fn referencing(origin: &moq_net::origin::Producer, epoch: Option<moq_net::Epoch>) -> Publisher {
		let mut config = video_rendition();
		config.broadcast = Some(moq_net::path::Relative::new("./cam").unwrap());
		config.epoch = epoch;
		Publisher::new(origin, Default::default(), config)
	}

	/// A sibling announced after the catalog starts its rendition, its end holds the pad, and its
	/// return starts a new stream on that pad.
	#[test]
	fn a_late_sibling_starts_its_rendition_and_returns_after_an_end() {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();
		let _room = referencing(&origin, None);
		let session = follow(&element, &origin);

		let mut first = Publisher::sibling(&origin, Default::default());
		let pad = recorder.written(&mut first, 1);
		let mut follow = watch(&origin, "cam");
		await_event(&mut follow, |event| matches!(event, moq_net::announce::Event::Start(_)));
		drop(first);
		await_event(&mut follow, |event| matches!(event, moq_net::announce::Event::End(_)));

		let mut second = Publisher::sibling(&origin, Default::default());
		assert_eq!(recorder.written(&mut second, 2), pad, "the return moved to another pad");
		stop(session);

		assert!(
			recorder.seen().contains(&Seen::Start(pad)),
			"no new stream on the kept pad"
		);
		recorder.assert_one_pad_held();
	}

	/// A rendition that joins a sibling instance after another started on it shares that instance's
	/// timeline, so both map the instance's timestamps to one running time.
	#[test]
	fn a_rendition_joining_a_sibling_instance_shares_its_timeline() {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();
		let mut room = referencing(&origin, None);
		let session = follow(&element, &origin);

		// A replacement starts a timeline of its own, which its first frame on `first` fixes.
		let mut old = Publisher::sibling(&origin, Default::default());
		let first = recorder.written(&mut old, 1);
		let mut new = Publisher::sibling(&origin, Default::default());
		assert_eq!(recorder.written(&mut new, 2), first);
		// Media past the subscription's one second of delay makes the groups before it stale, so
		// the joining rendition starts on a later frame than `first` did.
		recorder.written(&mut new, 3);
		new.skip(Duration::from_secs(2));
		recorder.written(&mut new, 4);

		let mut again = room.catalog.modify().unwrap().video.renditions["video"].clone();
		again.track = Some("video".into());
		room.catalog
			.modify()
			.unwrap()
			.video
			.renditions
			.insert("again".into(), again);
		let mut second = None;
		for _ in 0..200 {
			new.write(5);
			std::thread::sleep(Duration::from_millis(50));
			second = recorder.seen().into_iter().find_map(|seen| match seen {
				Seen::Buffer(pad, 5) if pad != first => Some(pad),
				_ => None,
			});
			if second.is_some() {
				break;
			}
		}
		let second = second.expect("the joining rendition never played");
		stop(session);

		let segment = |pad: &String| {
			recorder.seen().into_iter().rev().find_map(|seen| match seen {
				Seen::Segment(segmented, start) if segmented == *pad => Some(start),
				_ => None,
			})
		};
		assert_eq!(
			segment(&second),
			segment(&first),
			"the joining rendition mapped the instance on its own: {:?}",
			recorder.seen()
		);
		drop(old);
	}

	/// The same sibling instance announced again resumes its state, whether or not its end reached
	/// the follower first: a track it finished does not replay on its pad.
	#[test]
	fn a_reannounced_sibling_instance_does_not_replay_its_finish() {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();
		let _room = referencing(&origin, None);
		let session = follow(&element, &origin);
		let mut follow = watch(&origin, "cam");

		let route = epoch();
		let mut first = Publisher::sibling(&origin, route.clone());
		let pad = recorder.written(&mut first, 1);
		first.video.finish().unwrap();
		await_finished(&first);
		await_event(&mut follow, |event| matches!(event, moq_net::announce::Event::Start(_)));
		first.publication.as_ref().unwrap().unannounce();
		await_event(&mut follow, |event| matches!(event, moq_net::announce::Event::End(_)));
		first.publication.as_ref().unwrap().announce(route).unwrap();
		await_event(&mut follow, |event| matches!(event, moq_net::announce::Event::Start(_)));

		let mut second = Publisher::sibling(&origin, epoch());
		assert_eq!(
			recorder.written(&mut second, 2),
			pad,
			"the next instance moved to another pad"
		);
		stop(session);

		// Only the next instance starts a stream on the pad.
		let seen = recorder.seen();
		assert_eq!(recorder.count(|seen| *seen == Seen::Start(pad.clone())), 1, "{seen:?}");
		recorder.assert_one_pad_held();
		drop(first);
	}

	/// A sibling the catalog stopped reading is let go: once the catalog closes, the run ends while
	/// both broadcasts stay up.
	#[test]
	fn a_sibling_no_rendition_reads_does_not_outlive_its_catalog() {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();
		let mut room = referencing(&origin, None);
		let (_shutdown, shutdown) = watch::channel(false);
		let (_cancel, mut cancelled) = watch::channel(false);
		let run = {
			let weak = element.downgrade();
			let source = moq_mux::Source::new(origin.consume(), room.broadcast.consume());
			super::RUNTIME
				.spawn(async move { follow_catalog(source, &Pads::default(), weak, &shutdown, &mut cancelled).await })
		};

		let mut cam = Publisher::sibling(&origin, Default::default());
		recorder.written(&mut cam, 1);
		// The delisted rendition plays out its track before its pad retires.
		room.catalog.modify().unwrap().video.renditions.clear();
		cam.video.finish().unwrap();
		for _ in 0..200 {
			if pads(&element, "video_").is_empty() {
				break;
			}
			std::thread::sleep(Duration::from_millis(50));
		}
		assert!(pads(&element, "video_").is_empty(), "the removal never landed");

		room.catalog.finish().unwrap();
		super::RUNTIME
			.block_on(async { tokio::time::timeout(Duration::from_secs(10), run).await })
			.expect("the sibling's follower outlived the catalog")
			.unwrap()
			.unwrap();
		drop((room, cam));
	}

	/// A catalog finished while its broadcast stays up still follows its siblings: the next
	/// instance plays on the same pad, until the catalog's broadcast ends.
	#[test]
	fn a_sibling_is_followed_past_its_catalogs_close() {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();
		let mut room = referencing(&origin, None);
		let session = follow(&element, &origin);

		let mut first = Publisher::sibling(&origin, Default::default());
		let pad = recorder.written(&mut first, 1);
		room.catalog.finish().unwrap();
		let mut follow = watch(&origin, "cam");
		await_event(&mut follow, |event| matches!(event, moq_net::announce::Event::Start(_)));
		drop(first);
		await_event(&mut follow, |event| matches!(event, moq_net::announce::Event::End(_)));

		let mut second = Publisher::sibling(&origin, Default::default());
		assert_eq!(recorder.written(&mut second, 2), pad, "the return moved to another pad");

		// The catalog's broadcast ending lets go of the sibling it read.
		let mut demand = second.video.demand();
		drop(room);
		let unwanted = async {
			while demand.wanted_changed().await?.is_some() {}
			moq_net::Result::Ok(())
		};
		super::RUNTIME
			.block_on(async { tokio::time::timeout(Duration::from_secs(10), unwanted).await })
			.expect("the sibling outlived its catalog's broadcast")
			.expect("the sibling track closed");
		stop(session);
		recorder.assert_one_pad_held();
	}

	/// A sibling track finishing while its instance stays up idles the pad, and the instance that
	/// replaces it starts a new stream there.
	#[test]
	fn a_finished_sibling_track_returns_on_the_next_instance() {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();
		let _room = referencing(&origin, None);
		let session = follow(&element, &origin);

		let mut first = Publisher::sibling(&origin, Default::default());
		let pad = recorder.written(&mut first, 1);
		first.video.finish().unwrap();
		let mut second = Publisher::sibling(&origin, Default::default());
		assert_eq!(
			recorder.written(&mut second, 2),
			pad,
			"the next instance moved to another pad"
		);
		stop(session);

		assert!(!recorder.seen().contains(&Seen::Eos(pad)), "the finish ended the pad");
		recorder.assert_one_pad_held();
		drop(first);
	}

	/// Block until `publisher`'s video track reads to its end, as a reader reading it does.
	fn await_finished(publisher: &Publisher) {
		let broadcast = publisher.broadcast.consume();
		super::RUNTIME.block_on(async {
			let finished = async {
				let subscriber = broadcast
					.track("video")
					.unwrap()
					.subscribe(moq_net::track::Subscription::default())
					.await
					.unwrap();
				let mut track = moq_mux::container::Consumer::new(
					subscriber,
					moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
				);
				while track.read().await.unwrap().is_some() {}
			};
			tokio::time::timeout(Duration::from_secs(10), finished)
				.await
				.expect("the track never finished");
		});
	}

	/// A sibling track that finished stays finished for its instance only: a disable and an enable
	/// replays nothing on that instance, and the next instance plays even before the enable.
	#[test]
	fn a_reenabled_sibling_rendition_plays_the_next_instance_after_a_finish() {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();
		let mut room = referencing(&origin, None);
		let _audio = ["audio", "again"].map(|name| {
			room.broadcast
				.create_track(name, hang::container::track_info(hang::catalog::PRIORITY.audio))
				.unwrap()
		});
		let session = follow(&element, &origin);

		let mut first = Publisher::sibling(&origin, Default::default());
		let pad = recorder.written(&mut first, 1);
		first.video.finish().unwrap();
		await_finished(&first);

		// The catalog consumer only yields the newest snapshot, so each update lists another audio
		// rendition: its pad appearing proves the run acted on it.
		{
			let mut guard = room.catalog.modify().unwrap();
			guard.video.renditions.get_mut("video").unwrap().enabled = false;
			guard.audio.renditions = BTreeMap::from([("audio".to_string(), audio_rendition())]);
		}
		await_pad(&element, "audio_");
		let mut second = Publisher::sibling(&origin, Default::default());
		assert_eq!(
			recorder.written(&mut second, 2),
			pad,
			"the next instance moved to another pad"
		);

		{
			let mut guard = room.catalog.modify().unwrap();
			guard.video.renditions.get_mut("video").unwrap().enabled = true;
			let mut again = audio_rendition();
			again.track = Some("again".into());
			guard.audio.renditions.insert("again".to_string(), again);
		}
		for _ in 0..200 {
			if pads(&element, "audio_").len() == 2 {
				break;
			}
			std::thread::sleep(Duration::from_millis(50));
		}
		assert_eq!(pads(&element, "audio_").len(), 2, "the re-enable never landed");
		let replayed = recorder.count(|seen| *seen == Seen::Buffer(pad.clone(), 1));

		assert_eq!(recorder.written(&mut second, 3), pad);

		stop(session);

		// Only the next instance starts a stream: the finished one stays finished on its pad.
		let seen = recorder.seen();
		assert_eq!(recorder.count(|seen| *seen == Seen::Start(pad.clone())), 1, "{seen:?}");
		assert_eq!(
			recorder.count(|seen| *seen == Seen::Buffer(pad.clone(), 1)),
			replayed,
			"{seen:?}"
		);
		recorder.assert_one_pad_held();
		drop(first);
	}

	/// A sibling rendition delisted while it plays and listed again before its track finishes stays
	/// with the reader that drained it: the finished track does not replay on its pad, and the
	/// next instance plays there.
	#[test]
	fn a_sibling_rendition_relisted_while_draining_does_not_replay_its_finish() {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();
		let mut room = referencing(&origin, None);
		let _audio = ["audio", "again"].map(|name| {
			room.broadcast
				.create_track(name, hang::container::track_info(hang::catalog::PRIORITY.audio))
				.unwrap()
		});
		let session = follow(&element, &origin);

		let mut first = Publisher::sibling(&origin, Default::default());
		let pad = recorder.written(&mut first, 1);

		// The catalog consumer only yields the newest snapshot, so each update lists another audio
		// rendition: its pad appearing proves the run acted on it.
		let listed = room.catalog.modify().unwrap().video.renditions.clone();
		{
			let mut guard = room.catalog.modify().unwrap();
			guard.video.renditions.clear();
			guard.audio.renditions = BTreeMap::from([("audio".to_string(), audio_rendition())]);
		}
		await_pad(&element, "audio_");
		{
			let mut guard = room.catalog.modify().unwrap();
			guard.video.renditions = listed;
			let mut again = audio_rendition();
			again.track = Some("again".into());
			guard.audio.renditions.insert("again".to_string(), again);
		}
		for _ in 0..200 {
			if pads(&element, "audio_").len() == 2 {
				break;
			}
			std::thread::sleep(Duration::from_millis(50));
		}
		assert_eq!(pads(&element, "audio_").len(), 2, "the relist never landed");

		first.video.finish().unwrap();
		await_finished(&first);
		let mut second = Publisher::sibling(&origin, Default::default());
		assert_eq!(
			recorder.written(&mut second, 2),
			pad,
			"the next instance moved to another pad"
		);
		stop(session);

		let seen = recorder.seen();
		assert_eq!(recorder.count(|seen| *seen == Seen::Start(pad.clone())), 1, "{seen:?}");
		recorder.assert_one_pad_held();
		drop(first);
	}

	/// A publisher handoff after a sibling track finished, the catalog naming the new instance's
	/// epoch, plays that instance on the same pad.
	#[test]
	fn an_epoch_handoff_after_a_sibling_finish_plays_the_new_instance() {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();
		let (old, new) = (moq_net::Epoch::mint(), moq_net::Epoch::mint());
		let mut room = referencing(&origin, Some(old.clone()));
		let session = follow(&element, &origin);

		let mut first = Publisher::sibling(&origin, moq_net::origin::Route::default().with_epoch(old));
		let pad = recorder.written(&mut first, 1);
		first.video.finish().unwrap();
		await_finished(&first);

		let mut second = Publisher::sibling(&origin, moq_net::origin::Route::default().with_epoch(new.clone()));
		room.catalog
			.modify()
			.unwrap()
			.video
			.renditions
			.get_mut("video")
			.unwrap()
			.epoch = Some(new);
		assert_eq!(
			recorder.written(&mut second, 2),
			pad,
			"the new instance moved to another pad"
		);
		stop(session);

		recorder.assert_one_pad_held();
		drop(first);
	}

	/// An instance a rendition pins that wins its path back from another resumes its state: the
	/// track it finished replays on no pad, pinned or not.
	#[test]
	fn a_pinned_instance_winning_its_path_back_does_not_replay_its_finish() {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();
		let (pinned, other, next) = (moq_net::Epoch::mint(), moq_net::Epoch::mint(), moq_net::Epoch::mint());
		let mut room = referencing(&origin, Some(pinned.clone()));
		{
			let mut guard = room.catalog.modify().unwrap();
			let mut again = guard.video.renditions["video"].clone();
			again.track = Some("video".into());
			again.epoch = None;
			guard.video.renditions.insert("again".into(), again);
		}
		let session = follow(&element, &origin);

		// Each rendition plays the first instance on a pad of its own. Written once, so a second
		// copy on either pad can only be a replay.
		let mut first = Publisher::sibling(&origin, moq_net::origin::Route::default().with_epoch(pinned));
		first.write(1);
		let played = recorder.wait("1 never reached both renditions", |seen| {
			let pads = reached(seen, 1);
			(pads.len() == 2).then_some(pads)
		});
		first.video.finish().unwrap();
		await_finished(&first);

		// Another instance wins the path, which only the unpinned rendition plays, then leaves it
		// to the pinned one.
		let mut second = Publisher::sibling(&origin, moq_net::origin::Route::default().with_epoch(other));
		let unpinned = recorder.written(&mut second, 2);
		let pad = played.into_iter().find(|pad| *pad != unpinned).unwrap();
		let mut follow = watch(&origin, "cam");
		await_event(&mut follow, |event| matches!(event, moq_net::announce::Event::Start(_)));
		drop(second);
		await_event(&mut follow, |event| {
			matches!(
				event,
				moq_net::announce::Event::Restart(_)
					| moq_net::announce::Event::Takeover { .. }
					| moq_net::announce::Event::Start(_)
			)
		});

		let mut third = Publisher::sibling(&origin, moq_net::origin::Route::default().with_epoch(next.clone()));
		room.catalog
			.modify()
			.unwrap()
			.video
			.renditions
			.get_mut("video")
			.unwrap()
			.epoch = Some(next);
		third.write(3);
		recorder.wait("the next instance never reached the pinned pad", |seen| {
			reached(seen, 3).contains(&pad).then_some(())
		});
		stop(session);

		// Neither pad replays the first instance's track on its return.
		let seen = recorder.seen();
		for pad in [&pad, &unpinned] {
			assert_eq!(
				recorder.count(|seen| *seen == Seen::Buffer(pad.clone(), 1)),
				1,
				"{seen:?}"
			);
		}
		drop(first);
	}

	/// A rendition delisted while its sibling is offline ends its idle pad, and the sibling's next
	/// instance plays nothing on it.
	#[test]
	fn a_delisted_sibling_rendition_follows_no_later_instance() {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();
		let mut room = referencing(&origin, None);
		let session = follow(&element, &origin);

		let mut first = Publisher::sibling(&origin, Default::default());
		let pad = recorder.written(&mut first, 1);
		let mut follow = watch(&origin, "cam");
		await_event(&mut follow, |event| matches!(event, moq_net::announce::Event::Start(_)));
		drop(first);
		await_event(&mut follow, |event| matches!(event, moq_net::announce::Event::End(_)));

		room.catalog.modify().unwrap().video.renditions.clear();
		for _ in 0..200 {
			if recorder.seen().contains(&Seen::Eos(pad.clone())) {
				break;
			}
			std::thread::sleep(Duration::from_millis(50));
		}
		assert!(
			recorder.seen().contains(&Seen::Eos(pad.clone())),
			"the idle pad never ended"
		);

		let mut second = Publisher::sibling(&origin, Default::default());
		await_event(&mut follow, |event| matches!(event, moq_net::announce::Event::Start(_)));
		for _ in 0..10 {
			second.write(2);
			std::thread::sleep(Duration::from_millis(50));
		}
		stop(session);

		let seen = recorder.seen();
		assert!(!seen.iter().any(|seen| matches!(seen, Seen::Buffer(_, 2))), "{seen:?}");
	}

	/// A rendition delisted while its sibling is offline and listed again follows the sibling's next
	/// instance, even though the reader that left took its pad with it.
	#[test]
	fn a_relisted_sibling_rendition_follows_the_next_instance() {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();
		let mut room = referencing(&origin, None);
		let session = follow(&element, &origin);

		let mut first = Publisher::sibling(&origin, Default::default());
		let pad = recorder.written(&mut first, 1);
		let mut follow = watch(&origin, "cam");
		await_event(&mut follow, |event| matches!(event, moq_net::announce::Event::Start(_)));
		drop(first);
		await_event(&mut follow, |event| matches!(event, moq_net::announce::Event::End(_)));

		let listed = room.catalog.modify().unwrap().video.renditions.clone();
		room.catalog.modify().unwrap().video.renditions.clear();
		for _ in 0..200 {
			if recorder.seen().contains(&Seen::Eos(pad.clone())) {
				break;
			}
			std::thread::sleep(Duration::from_millis(50));
		}
		assert!(
			recorder.seen().contains(&Seen::Eos(pad.clone())),
			"the idle pad never ended"
		);
		room.catalog.modify().unwrap().video.renditions = listed;

		let mut second = Publisher::sibling(&origin, Default::default());
		assert_ne!(recorder.written(&mut second, 2), pad, "the ended pad came back");
		stop(session);
	}

	/// A sibling replaced while the old one stays up restarts a reference that names no epoch, on
	/// the same pad, and nothing more from the old instance gets through.
	fn sibling_restart(route: impl Fn() -> moq_net::origin::Route) {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();
		let _room = referencing(&origin, None);
		let mut old = Publisher::sibling(&origin, route());
		let session = follow(&element, &origin);
		let pad = recorder.written(&mut old, 1);

		let mut new = Publisher::sibling(&origin, route());
		assert_eq!(recorder.written(&mut new, 2), pad, "the restart moved to another pad");
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
			"the replaced sibling kept playing: {seen:?}"
		);
		recorder.assert_one_pad_held();
	}

	#[test]
	fn a_sibling_restart_switches_on_the_same_pad() {
		sibling_restart(epoch);
	}

	#[test]
	fn an_epochless_sibling_restart_switches_on_the_same_pad() {
		sibling_restart(moq_net::origin::Route::default);
	}

	/// A reference naming its sibling's epoch never reads the instance replacing it: the pad
	/// idles, and plays again once the named instance serves the path again.
	#[test]
	fn a_sibling_restart_idles_a_reference_naming_the_old_epoch() {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();
		let named = moq_net::Epoch::mint();
		let _room = referencing(&origin, Some(named.clone()));
		let mut old = Publisher::sibling(&origin, moq_net::origin::Route::default().with_epoch(named));
		let session = follow(&element, &origin);
		await_demand(&old, true);
		let pad = recorder.written(&mut old, 1);

		// The reference lets the old instance go once another wins its path, and only then
		// does the new one leave, so the return is a resume rather than a missed restart.
		let mut follow = watch(&origin, "cam");
		await_event(&mut follow, |event| matches!(event, moq_net::announce::Event::Start(_)));
		let mut new = Publisher::sibling(&origin, epoch());
		await_event(&mut follow, |event| {
			matches!(event, moq_net::announce::Event::Restart(_))
		});
		await_demand(&old, false);
		new.write(2);
		drop(new);
		await_event(&mut follow, |event| {
			matches!(event, moq_net::announce::Event::Restart(_))
		});

		await_demand(&old, true);
		assert_eq!(
			recorder.written(&mut old, 3),
			pad,
			"the named instance moved to another pad"
		);
		stop(session);
		assert!(
			!recorder.seen().contains(&Seen::Buffer(pad, 2)),
			"another epoch played: {:?}",
			recorder.seen()
		);
		recorder.assert_one_pad_held();
	}

	/// A rendition the catalog moves to another track, in its broadcast and then in a sibling one,
	/// plays on from it on the same pad, and nothing more from the track it left gets through.
	#[test]
	fn a_moved_rendition_plays_on_the_same_pad() {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();

		let mut room = Publisher::new(&origin, Default::default(), video_rendition());
		let session = follow(&element, &origin);
		let pad = recorder.written(&mut room, 1);

		// Moved to another track of the broadcast, which the publisher writes from now on.
		let moved = room
			.broadcast
			.create_track("moved", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let moved = moq_mux::container::Producer::new(
			moved,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
		);
		let mut left = std::mem::replace(&mut room.video, moved);
		room.catalog
			.modify()
			.unwrap()
			.video
			.renditions
			.get_mut("video")
			.unwrap()
			.track = Some("moved".into());
		assert_eq!(recorder.written(&mut room, 2), pad, "the move took another pad");
		left.write(room.frame(1)).unwrap();
		recorder.written(&mut room, 3);

		// Moved to a sibling broadcast on the same clock.
		let mut cam = Publisher::sibling(&origin, Default::default());
		cam.start = room.start;
		{
			let mut guard = room.catalog.modify().unwrap();
			let config = guard.video.renditions.get_mut("video").unwrap();
			config.track = None;
			config.broadcast = Some(moq_net::path::Relative::new("./cam").unwrap());
		}
		assert_eq!(recorder.written(&mut cam, 4), pad, "the move took another pad");
		room.write(3);
		recorder.written(&mut cam, 5);
		stop(session);

		let seen = recorder.seen();
		let after = |tag: u8| {
			let at = seen
				.iter()
				.position(|seen| *seen == Seen::Buffer(pad.clone(), tag))
				.unwrap();
			&seen[at..]
		};
		assert!(
			!after(2).contains(&Seen::Buffer(pad.clone(), 1)),
			"the track it left kept playing: {seen:?}"
		);
		assert!(
			!after(4).contains(&Seen::Buffer(pad.clone(), 3)),
			"the track it left kept playing: {seen:?}"
		);
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

		publisher
			.publication
			.as_ref()
			.unwrap()
			.announce(route.with_cost(5))
			.unwrap();
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

	/// With no enabled alternative, disabling holds the reader as well as its pad, so media that
	/// beats the re-enable catalog update plays without a new subscription or stream.
	#[test]
	fn a_disabled_rendition_keeps_reading_on_the_same_pad() {
		let _pad_ids = pad_ids();
		let element = element();
		let recorder = Recorder::attach(&element);
		let origin = origin();

		let mut publisher = Publisher::new(&origin, epoch(), video_rendition());
		let _audio = publisher
			.broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let session = follow(&element, &origin);
		let pad = recorder.written(&mut publisher, 1);

		// The catalog consumer only yields the newest snapshot, so the same update lists an audio
		// rendition: its pad appearing proves the run acted on the disable.
		{
			let mut guard = publisher.catalog.modify().unwrap();
			guard.video.renditions.get_mut("video").unwrap().enabled = false;
			guard.audio.renditions = BTreeMap::from([("audio".to_string(), audio_rendition())]);
		}
		await_pad(&element, "audio_");

		assert_eq!(
			recorder.written(&mut publisher, 2),
			pad,
			"media before the re-enable did not reach the held reader"
		);

		publisher
			.catalog
			.modify()
			.unwrap()
			.video
			.renditions
			.get_mut("video")
			.unwrap()
			.enabled = true;
		assert_eq!(
			recorder.written(&mut publisher, 3),
			pad,
			"the re-enabled rendition moved to another pad"
		);
		stop(session);

		let seen = recorder.seen();
		assert!(
			!seen.contains(&Seen::Start(pad)),
			"the disable restarted the stream: {seen:?}"
		);
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

		old.publication.as_ref().unwrap().unannounce();
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

	/// What a [`Relay`] does with each new session.
	#[derive(Clone, Copy)]
	enum Admit {
		Serve,
		/// Drop the connection unanswered, as a relay that is down would.
		Down,
		/// Reject the session as forbidden.
		Refuse,
	}

	/// A relay on loopback: an origin served over plain TCP to every session, which may also
	/// publish into it.
	struct Relay {
		origin: moq_net::origin::Producer,
		url: url::Url,
		task: tokio::task::JoinHandle<()>,
		close: tokio::sync::mpsc::UnboundedSender<moq_net::Error>,
		/// Set before a `close` so the redial meets it. The listener stays bound throughout:
		/// rebinding a freed port races its release and other tests binding port 0.
		admit: std::sync::Arc<Mutex<Admit>>,
		/// Dials dropped while [`Admit::Down`], and a wakeup for each.
		dropped: tokio::sync::watch::Sender<usize>,
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
				let admit = std::sync::Arc::new(Mutex::new(Admit::Serve));
				let admitting = admit.clone();
				let dropped = tokio::sync::watch::Sender::new(0);
				let dropping = dropped.clone();
				let (close, mut closing) = tokio::sync::mpsc::unbounded_channel::<moq_net::Error>();
				let task = tokio::spawn(async move {
					let mut sessions: Vec<moq_net::Session> = Vec::new();
					loop {
						tokio::select! {
							Some(err) = closing.recv() => {
								for session in &sessions { session.abort(err.clone()); }
							}
							request = server.accept() => {
								let Some(request) = request else { break };
								let admit = *admitting.lock().unwrap();
								match admit {
									Admit::Serve => {
										if let Ok(session) = request.with_publisher(&serving).with_subscriber(serving.clone()).ok().await {
											sessions.push(session);
										}
									}
									Admit::Down => {
										drop(request);
										dropping.send_modify(|dropped| *dropped += 1);
									}
									Admit::Refuse => request.reject(moq_net::SessionError::Unauthorized).await.unwrap(),
								}
							}
						}
					}
				});
				Self {
					origin,
					url: format!("tcp://127.0.0.1:{port}/").parse().unwrap(),
					task,
					close,
					admit,
					dropped,
				}
			})
		}

		fn stop(mut self) {
			self.task.abort();
			let _ = super::RUNTIME.block_on(&mut self.task);
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

	/// Both elements keep their sessions alive across an outage that refuses several redials.
	/// `the_*_retries_without_a_budget` pin that no budget ever ends the retrying.
	#[test]
	fn relay_loss_reconnects_after_refused_redials() {
		let _pad_ids = pad_ids();
		let relay = Relay::new();
		let url = relay.url.clone();
		let (pipeline, output) = pipeline(&url);
		let sink = glib::Object::builder::<crate::sink::MoqSink>()
			.property("url", url.as_str())
			.property("broadcast", "room")
			.build();
		let video = sink.request_pad_simple("sink_%u").unwrap();
		video.set_property("track", "video");
		sink.set_state(gst::State::Paused).unwrap();
		assert!(video.send_event(gst::event::StreamStart::new("video")));
		assert!(
			video.send_event(gst::event::Caps::new(
				&gst::Caps::builder("video/x-h264")
					.field("stream-format", "byte-stream")
					.field("alignment", "au")
					.build()
			))
		);
		assert!(video.send_event(gst::event::Segment::new(&gst::FormattedSegment::<gst::ClockTime>::new())));
		let start = std::time::Instant::now();
		let frame = || {
			let mut frame = gst::Buffer::from_slice(crate::sink::h264_keyframe_au());
			frame
				.get_mut()
				.unwrap()
				.set_pts(gst::ClockTime::from_nseconds(start.elapsed().as_nanos() as u64));
			assert_eq!(video.chain(frame), Ok(gst::FlowSuccess::Ok));
		};
		let pull = |wait: Duration| {
			output.emit_by_name::<Option<gst::Sample>>("try-pull-sample", &[&(wait.as_nanos() as u64)])
		};
		let play = || {
			for _ in 0..200 {
				frame();
				if pull(Duration::from_millis(50)).is_some() {
					return;
				}
			}
			panic!("moqsink frames never reached moqsrc");
		};
		play();
		let held = pipeline.by_name("src").unwrap().src_pads();
		*relay.admit.lock().unwrap() = Admit::Down;
		relay.close.send(moq_net::Error::GoingAway).unwrap();
		// Two refused redials from each element, so each retried past a failed attempt.
		let mut dropped = relay.dropped.subscribe();
		super::RUNTIME
			.block_on(async {
				tokio::time::timeout(Duration::from_secs(30), dropped.wait_for(|dropped| *dropped >= 4)).await
			})
			.expect("the elements stopped redialing")
			.unwrap();
		while pull(Duration::ZERO).is_some() {}
		*relay.admit.lock().unwrap() = Admit::Serve;
		play();
		assert_eq!(
			pipeline.by_name("src").unwrap().src_pads(),
			held,
			"reconnect replaced the linked pad"
		);
		sink.set_state(gst::State::Null).unwrap();
		assert_played_cleanly(pipeline);
		relay.stop();
	}

	#[test]
	fn a_refused_redial_posts_a_bus_error_after_playback() {
		let _pad_ids = pad_ids();
		let relay = Relay::new();
		let mut publisher = Publisher::new(&relay.origin, epoch(), video_rendition());
		let (pipeline, sink) = pipeline(&relay.url);
		rendered(&sink, &mut publisher, 1);
		*relay.admit.lock().unwrap() = Admit::Refuse;
		relay.close.send(moq_net::Error::GoingAway).unwrap();
		let error = pipeline
			.bus()
			.unwrap()
			.timed_pop_filtered(gst::ClockTime::from_seconds(10), &[gst::MessageType::Error]);
		pipeline.set_state(gst::State::Null).unwrap();
		relay.stop();
		let error = error.expect("refused redial did not fail the pipeline");
		let gst::MessageView::Error(error) = error.view() else {
			unreachable!()
		};
		assert!(error.debug().unwrap().contains("unauthorized"), "{error:?}");
	}

	#[test]
	fn a_protocol_violation_posts_a_bus_error_after_playback() {
		let _pad_ids = pad_ids();
		let relay = Relay::new();
		let mut publisher = Publisher::new(&relay.origin, epoch(), video_rendition());
		let (pipeline, sink) = pipeline(&relay.url);
		rendered(&sink, &mut publisher, 1);
		relay.close.send(moq_net::Error::ProtocolViolation).unwrap();
		let error = pipeline
			.bus()
			.unwrap()
			.timed_pop_filtered(gst::ClockTime::from_seconds(10), &[gst::MessageType::Error]);
		pipeline.set_state(gst::State::Null).unwrap();
		relay.stop();
		let error = error.expect("protocol violation did not fail the pipeline");
		let gst::MessageView::Error(error) = error.view() else {
			unreachable!()
		};
		assert!(error.debug().unwrap().contains("protocol violation"), "{error:?}");
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

	/// A real moqsink restart negotiates audio after video, keeping both held moqsrc pads.
	#[test]
	fn a_moqsink_restart_keeps_pads_when_caps_arrive_at_different_times() {
		let _pad_ids = pad_ids();
		let relay = Relay::new();
		let element = element();
		let (sent, received) = std::sync::mpsc::channel();
		let added = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
		let count = added.clone();
		element.connect_pad_added(move |_, pad| {
			count.fetch_add(1, Ordering::Relaxed);
			let sent = sent.clone();
			let generation = std::sync::atomic::AtomicUsize::new(0);
			pad.add_probe(
				gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM,
				move |pad, info| {
					match &info.data {
						Some(gst::PadProbeData::Event(event)) => match event.view() {
							gst::EventView::StreamStart(_) => {
								generation.fetch_add(1, Ordering::Relaxed);
							}
							gst::EventView::Eos(_) => {
								sent.send((pad.name().to_string(), generation.load(Ordering::Relaxed), true))
									.unwrap();
							}
							_ => {}
						},
						Some(gst::PadProbeData::Buffer(_)) => {
							sent.send((pad.name().to_string(), generation.load(Ordering::Relaxed), false))
								.unwrap();
							return gst::PadProbeReturn::Drop;
						}
						_ => {}
					}
					gst::PadProbeReturn::Ok
				},
			);
		});
		let session = follow(&element, &relay.origin);
		let sink = glib::Object::builder::<crate::sink::MoqSink>()
			.property("url", relay.url.as_str())
			.property("broadcast", "room")
			.build();
		let video = sink.request_pad_simple("sink_%u").unwrap();
		video.set_property("track", "video");
		let audio = sink.request_pad_simple("sink_%u").unwrap();
		audio.set_property("track", "audio");
		let caps = gst::Caps::builder("video/x-h264")
			.field("stream-format", "byte-stream")
			.field("alignment", "au")
			.build();
		let audio_caps = gst::Caps::builder("audio/x-opus")
			.field("rate", 48_000i32)
			.field("channels", 2i32)
			.build();
		let mut held = std::collections::BTreeSet::new();
		for generation in 0..3 {
			if generation == 2 {
				sink.release_request_pad(&audio);
			}
			sink.set_state(gst::State::Paused).unwrap();
			assert!(video.send_event(gst::event::StreamStart::new("video")));
			assert!(video.send_event(gst::event::Caps::new(&caps)));
			assert!(video.send_event(gst::event::Segment::new(&gst::FormattedSegment::<gst::ClockTime>::new())));
			let keyframe = crate::sink::h264_keyframe_au();
			for pts in [0, 20, 40] {
				let mut frame = gst::Buffer::from_slice(keyframe.clone());
				frame.get_mut().unwrap().set_pts(gst::ClockTime::from_mseconds(pts));
				assert_eq!(video.chain(frame), Ok(gst::FlowSuccess::Ok));
			}
			if generation < 2 {
				assert!(audio.send_event(gst::event::StreamStart::new("audio")));
				assert!(audio.send_event(gst::event::Caps::new(&audio_caps)));
				assert!(audio.send_event(gst::event::Segment::new(&gst::FormattedSegment::<gst::ClockTime>::new())));
				for pts in [0, 20, 40] {
					let mut frame = gst::Buffer::from_slice([0xf8, 0xff, 0xfe]);
					frame.get_mut().unwrap().set_pts(gst::ClockTime::from_mseconds(pts));
					assert_eq!(audio.chain(frame), Ok(gst::FlowSuccess::Ok));
				}
			}
			let mut playing = std::collections::BTreeSet::new();
			let mut dropped = false;
			let returning = if generation == 2 { 1 } else { 2 };
			while playing.len() < returning || (generation == 2 && !dropped) {
				let (pad, run, eos) = received
					.recv_timeout(Duration::from_secs(10))
					.expect("both renditions should resume");
				if eos {
					assert_eq!(generation, 2, "a returning rendition received EOS");
					assert!(pad.starts_with("audio_"), "the returning video received EOS");
					assert!(!dropped, "the dropped rendition ended twice");
					dropped = true;
				} else if run == generation {
					playing.insert(pad);
				}
			}
			if generation == 0 {
				held = playing;
			} else if generation == 1 {
				assert_eq!(playing, held, "restart replaced a linked pad");
			} else {
				assert!(
					playing
						.iter()
						.all(|pad| held.contains(pad) && pad.starts_with("video_"))
				);
			}
			sink.set_state(gst::State::Ready).unwrap();
		}
		assert_eq!(added.load(Ordering::Relaxed), 2);
		sink.set_state(gst::State::Null).unwrap();
		stop(session);
		assert!(received.try_iter().all(|(_, _, eos)| !eos));
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
