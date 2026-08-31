use std::collections::HashMap;
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
/// created by a restarted session can't collide with one still being torn down by the
/// previous one, and split per kind so the *first* video pad is reliably `video_0` and the
/// first audio pad `audio_0`. That predictability matters because `gst-launch` links a
/// source's sometimes-pads by name (`moqsrc name=s s.video_0 ! ...`); a single shared counter
/// made the first pad's number depend on catalog arrival order (audio could claim `0`),
/// silently breaking those pipelines. Counters only ever increment, so a mid-stream reshape
/// still gets a fresh, collision-free id.
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
}

/// The session task drives everything: it connects, follows the catalog, and
/// runs one [`Pump`] per active rendition. The element just starts and
/// stops it. No control-plane channel is needed because pumps push to their pads
/// directly from their own task (a source pad's push *is* its streaming thread),
/// so there's nothing to marshal back onto the element.
struct SessionController {
	shutdown: watch::Sender<bool>,
	join: tokio::task::JoinHandle<()>,
}

impl SessionController {
	fn start(settings: ResolvedSettings, element: glib::WeakRef<super::MoqSrc>) -> Self {
		let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
		let join = RUNTIME.spawn(async move {
			if let Err(err) = run_session(settings, element.clone(), &mut shutdown_rx).await
				&& let Some(obj) = element.upgrade()
			{
				gst::element_error!(obj, gst::CoreError::Failed, ("session error"), ["{err:?}"]);
			}
		});

		Self {
			shutdown: shutdown_tx,
			join,
		}
	}

	fn stop(self) {
		let _ = self.shutdown.send(true);
		RUNTIME.spawn(async move {
			if let Err(err) = self.join.await {
				gst::warning!(CAT, "session task ended with error: {err:?}");
			}
		});
	}
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
		let session = SessionController::start(settings, self.obj().downgrade());
		*self.session.lock().unwrap() = Some(session);
		Ok(())
	}

	fn stop_session(&self) {
		if let Some(session) = self.session.lock().unwrap().take() {
			session.stop();
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

/// A rendition we're currently serving, keyed in the session by moq track name.
struct ActiveTrack {
	/// Identity we diff against on each catalog update; a change recreates the pad.
	shape: Shape,
	/// Tells the pump to drop its pad and exit (set on shutdown or when reconcile
	/// removes/replaces the rendition).
	cancel: watch::Sender<bool>,
	/// Handle to the pump task in the session's `JoinSet`. We only read
	/// `is_finished()` to prune this entry once the pump ends (the `JoinSet` owns
	/// the task and reaps it); teardown goes through `cancel`, never `abort()`.
	task: tokio::task::AbortHandle,
	/// Shared with the pump, so teardown and pad creation agree on which of them happened.
	state: Arc<PumpState>,
}

impl ActiveTrack {
	/// Tear the pump down whatever stage it reached: one still subscribing never takes a pad,
	/// one that has drops it when it sees the watch.
	///
	/// Terminal, so it consumes the handle: a cancelled rendition is removed from the session's
	/// active set, and respawns as a fresh pump if the catalog names it again.
	fn cancel(self) {
		self.state.cancel_before_live();
		let _ = self.cancel.send(true);
	}
}

async fn run_session(
	settings: ResolvedSettings,
	element: glib::WeakRef<super::MoqSrc>,
	shutdown: &mut watch::Receiver<bool>,
) -> Result<()> {
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(settings.tls_disable_verify);

	let origin = moq_tokio::origin::spawn(moq_net::Hop::random());
	let origin_consumer = origin.consume();
	let client = config.init(Default::default())?.with_subscriber(origin);

	// One-shot: the catalog subscription below dies with the session anyway, so a
	// background redial could not resurrect this run. A drop surfaces as the
	// catalog closing and the loop below winding down.
	let _connection = client
		.with_reconnect(false)
		.connect(settings.url.clone())
		.established()
		.await?;

	// Wait for the broadcast to be announced. Synchronous lookup would race the gossip of
	// announcements that happens after the session is established.
	tracing::info!(broadcast = %settings.broadcast, "waiting for broadcast to be announced");
	let broadcast = tokio::select! {
		broadcast = origin_consumer.announced_broadcast(&settings.broadcast) => broadcast
			.context("broadcast not allowed or origin closed")?,
		_ = shutdown.changed() => return Ok(()),
	};

	follow_catalog(broadcast, element, shutdown).await
}

/// Follow the broadcast's catalog for the whole session, keeping one [`Pump`] per
/// announced rendition in sync with it. Returns once the catalog closes and the last pump
/// drains, or `shutdown` fires.
async fn follow_catalog(
	broadcast: moq_net::broadcast::Consumer,
	element: glib::WeakRef<super::MoqSrc>,
	shutdown: &mut watch::Receiver<bool>,
) -> Result<()> {
	let catalog_track = broadcast
		.track(hang::catalog::Catalog::DEFAULT_NAME)?
		.subscribe(hang::catalog::Catalog::default_subscription())
		.await?;
	let mut catalog_consumer = moq_mux::catalog::hang::Consumer::new(catalog_track);

	// Follow the catalog for the whole session and reconcile our pumps against every update,
	// rather than building them once from the first frame. This covers reactive publishers
	// (the browser via @moq/hang) that announce an empty catalog before their encoder
	// configures, then add renditions a beat later, as well as renditions appearing,
	// disappearing, or changing codec/resolution mid-stream.
	let mut active: HashMap<String, ActiveTrack> = HashMap::new();
	let mut pumps: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
	let mut catalog_closed = false;

	loop {
		// Prune metadata for pumps that have ended (the JoinSet has already reaped the
		// tasks). Once the catalog is closed and the last pump drains we're done: each
		// emitted EOS (or a pad drop on error) downstream via its own end path.
		active.retain(|_, track| !track.task.is_finished());
		if catalog_closed && pumps.is_empty() {
			break;
		}

		tokio::select! {
			// Full session shutdown: break to the drain below.
			_ = shutdown.changed() => break,
			// A pump finished; loop back so the `retain` above prunes its entry and the
			// break condition sees the drained set.
			_ = pumps.join_next(), if !pumps.is_empty() => {}
			// The guard stops us polling a closed catalog track (which would spin the loop
			// returning None) while we wait for the remaining pumps to drain.
			next = catalog_consumer.next(), if !catalog_closed => {
				match next? {
					Some(catalog) => reconcile(&catalog, &mut active, &mut pumps, &broadcast, &element),
					// Catalog track closed. Don't cancel the pumps: let each reach its
					// natural Ok(None) -> EOS end so downstream sees a clean EOS rather than a
					// bare pad drop. We just stop reconciling and wait for them to drain.
					//
					// That includes a pump still resolving its subscription, which is
					// indistinguishable here from one that has simply not been polled yet:
					// a final snapshot naming a track the publisher does serve arrives this
					// way, and cancelling on "not live yet" would drop it. A rendition nobody
					// ever answers therefore keeps the session alive until the broadcast ends
					// or the element is stopped.
					None => catalog_closed = true,
				}
			}
		}
	}

	// Shutdown: cancel every pump, then wait for them all to drop their pads. Cancel all up front
	// (pumps only exit on their own `cancel`), or the not-yet-cancelled ones would keep streaming
	// while we await the rest.
	// On the clean catalog-closed exit `active`/`pumps` are already drained, so this is a no-op.
	for (_, track) in active.drain() {
		track.cancel();
	}
	while pumps.join_next().await.is_some() {}

	Ok(())
}

/// Bring the live set of pumps in line with `catalog`: spawn pumps for newly announced
/// renditions, tear down ones that vanished, and recreate any whose caps or container changed.
///
/// Infallible by design: every way a single rendition can be unusable (unsupported codec,
/// malformed init, a name the broadcast refuses) skips just that rendition, so one bad entry in
/// the catalog can never tear down the ones already streaming.
fn reconcile(
	catalog: &moq_mux::catalog::hang::Catalog,
	active: &mut HashMap<String, ActiveTrack>,
	pumps: &mut tokio::task::JoinSet<()>,
	broadcast: &moq_net::broadcast::Consumer,
	element: &glib::WeakRef<super::MoqSrc>,
) {
	struct Desired {
		kind: TrackKind,
		shape: Shape,
	}

	// Build the desired shape for each rendition. This is deliberately cheap: caps come from the
	// catalog config and the container is just the hang descriptor. We defer parsing the wire
	// container (which re-parses the CMAF init) to spawn time below, so an unchanged rendition
	// costs nothing here. A rendition whose caps we can't build (unsupported codec) is logged and
	// skipped rather than failing the whole session, so one bad rendition can't tear down the
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

	// Pure set math: which active pads to tear down, which renditions to spawn.
	let plan = plan_reconcile(
		&desired
			.iter()
			.map(|(name, d)| (name.clone(), d.shape.clone()))
			.collect(),
		&active.iter().map(|(name, t)| (name.clone(), t.shape.clone())).collect(),
	);

	// Drop anything that disappeared or changed shape; each cancelled pump drops its own pad.
	// Changed renditions also land in `plan.add`, so they respawn below under a fresh pad id.
	for name in plan.remove {
		if let Some(track) = active.remove(&name) {
			track.cancel();
		}
	}

	// Spawn pumps for new or changed renditions. The wire container is parsed here, lazily and
	// only for renditions we're actually starting, since parsing a CMAF init is wasted work for
	// renditions that didn't change. A parse failure (malformed init) skips just this rendition.
	for name in plan.add {
		let d = &desired[&name];
		let container = match moq_mux::catalog::hang::Container::try_from(&d.shape.container) {
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
			Pump {
				element: element.clone(),
				kind: d.kind,
				name: name.clone(),
				caps: d.shape.caps.clone(),
				track,
				container,
				state: state.clone(),
				cancel: cancel_rx,
			}
			.run(),
			RUNTIME.handle(),
		);

		active.insert(
			name,
			ActiveTrack {
				shape: d.shape.clone(),
				cancel: cancel_tx,
				task,
				state,
			},
		);
	}
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

/// Identifies a pump's pad. Pads are named `video_<id>` / `audio_<id>` from a
/// per-kind, process-unique counter (matching the `%u` templates) rather than after
/// the track name, so a rendition can be torn down and recreated (when its
/// codec/resolution changes mid-stream) without two pads ever sharing a name. The
/// first *live* pad of each kind is `video_0` / `audio_0`, so `gst-launch` can link them by
/// name regardless of which rendition the catalog announces first, or how many it announces
/// that never arrive.
struct TrackDescriptor {
	kind: TrackKind,
	name: String,
	id: u64,
}

impl TrackDescriptor {
	/// Claim the next id of this kind, naming the pad about to be created.
	fn claim(kind: TrackKind, name: String) -> Self {
		let id = match kind {
			TrackKind::Video => &NEXT_VIDEO_PAD_ID,
			TrackKind::Audio => &NEXT_AUDIO_PAD_ID,
		}
		.fetch_add(1, Ordering::Relaxed);

		Self { kind, name, id }
	}

	fn pad_name(&self) -> String {
		match self.kind {
			TrackKind::Video => format!("video_{}", self.id),
			TrackKind::Audio => format!("audio_{}", self.id),
		}
	}
}

/// One rendition's pump: everything [`reconcile`] hands a task it spawns.
struct Pump {
	element: glib::WeakRef<super::MoqSrc>,
	kind: TrackKind,
	/// The moq track name, which is also the pad's stream id. The pad's own name comes from
	/// [`TrackDescriptor`] instead, and isn't known until the subscription resolves.
	name: String,
	caps: gst::Caps,
	track: moq_net::track::Consumer,
	container: moq_mux::catalog::hang::Container,
	/// Shared with this rendition's [`ActiveTrack::state`].
	state: Arc<PumpState>,
	cancel: watch::Receiver<bool>,
}

impl Pump {
	/// Subscribe to the track, then read its frames and push them to a pad owned for the rest of
	/// this pump's lifetime: create the pad, stream buffers, and remove the pad on exit. Runs
	/// until the track ends (EOS), errors, or `cancel` fires.
	async fn run(self) {
		let Pump {
			element,
			kind,
			name,
			caps,
			track,
			container,
			state,
			mut cancel,
		} = self;
		// Resolves once the publisher answers with the track info. A catalog can name a track its
		// publisher never serves, so this can wait forever; racing `cancel` keeps such a pump
		// reapable, and holding the wait here rather than in `reconcile` keeps it off every other
		// rendition.
		let subscriber = tokio::select! {
			_ = cancel.changed() => return,
			subscriber = track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_secs(1))) => match subscriber {
				Ok(subscriber) => subscriber,
				Err(err) => {
					gst::warning!(CAT, "track {name} failed to subscribe: {err:?}");
					return;
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
			return;
		}

		// The pad appears only once the track is live, so a pad downstream can link to is a promise
		// that the rendition is actually flowing, and only a rendition that gets this far claims a
		// pad id.
		let descriptor = TrackDescriptor::claim(kind, name);
		let Some(pad) = create_pad(&element, &descriptor, &caps) else {
			return;
		};

		let mut reference_ts = None;
		loop {
			tokio::select! {
				// This rendition is being torn down (shutdown, or replaced by a catalog update).
				_ = cancel.changed() => break,
				frame = track.read() => match frame {
					Ok(Some(frame)) => {
						let buffer = build_buffer(frame, &mut reference_ts, descriptor.kind);
						// pad.push() blocks until downstream accepts the buffer (full queues, a
						// clock-synced sink). block_in_place hands our sibling tasks to another
						// worker so a stalled downstream can't pin a runtime thread and starve
						// the session loop or other pumps.
						if tokio::task::block_in_place(|| pad.push(buffer)).is_err() {
							break;
						}
					}
					Ok(None) => {
						let _ = tokio::task::block_in_place(|| pad.push_event(gst::event::Eos::builder().build()));
						break;
					}
					Err(err) => {
						gst::warning!(CAT, "track {} failed: {err:?}", descriptor.name);
						break;
					}
				}
			}
		}

		let _ = pad.set_active(false);
		if let Some(obj) = element.upgrade() {
			let _ = obj.remove_pad(&pad);
		}
	}
}

/// Create, activate, and add a src pad for the track, seeding it with the sticky
/// stream-start/caps/segment events. Returns `None` if the element is already gone.
fn create_pad(
	element: &glib::WeakRef<super::MoqSrc>,
	descriptor: &TrackDescriptor,
	caps: &gst::Caps,
) -> Option<gst::Pad> {
	let obj = element.upgrade()?;
	let templ = obj.element_class().pad_template(descriptor.kind.template_name())?;

	let pad = gst::Pad::builder_from_template(&templ)
		.name(descriptor.pad_name())
		.build();

	pad.set_active(true).ok()?;
	pad.push_event(
		gst::event::StreamStart::builder(&descriptor.name)
			.group_id(gst::GroupId::next())
			.build(),
	);
	pad.push_event(gst::event::Caps::new(caps));
	pad.push_event(gst::event::Segment::new(&gst::FormattedSegment::<gst::ClockTime>::new()));

	obj.add_pad(&pad).ok()?;
	Some(pad)
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
	use super::{plan_reconcile, relative_pts};
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
}

#[cfg(test)]
mod session_tests {
	use std::collections::BTreeMap;
	use std::sync::Mutex;
	use std::sync::atomic::Ordering;
	use std::time::Duration;

	use gst::glib;
	use gst::prelude::*;
	use hang::catalog::{AudioCodec, AudioConfig, Container, H264, VideoConfig};
	use tokio::sync::watch;

	use super::{NEXT_VIDEO_PAD_ID, follow_catalog};

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
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();

		// First update announces audio only, and no producer ever answers for it.
		{
			let mut guard = catalog.lock();
			guard.audio.renditions = BTreeMap::from([("audio".to_string(), audio_rendition())]);
		}

		let (shutdown, mut shutdown_rx) = watch::channel(false);
		let consumer = broadcast.consume();
		let weak = element.downgrade();
		let session = super::RUNTIME.spawn(async move { follow_catalog(consumer, weak, &mut shutdown_rx).await });

		// Wait for the audio subscription before announcing video, and hold the request
		// unanswered. A catalog consumer skips to the newest snapshot, so without this the
		// session could read one update carrying both renditions, and then whether it reached
		// video before parking on audio would come down to `plan.add` ordering.
		let pending = await_request(&mut dynamic);
		assert_eq!(pending.name(), "audio");

		// Second update adds video, backed by a real track so its subscription resolves.
		let _video = broadcast.create_track("video", None).unwrap();
		{
			let mut guard = catalog.lock();
			guard.video.renditions = BTreeMap::from([("video".to_string(), video_rendition())]);
		}

		let pad = await_pad(&element, "video_");
		assert!(pads(&element, "audio_").is_empty(), "the unserved rendition got a pad");

		let _ = shutdown.send(true);
		super::RUNTIME.block_on(session).unwrap().unwrap();
		assert!(pad.parent().is_none(), "the pad outlived the session");
	}

	/// The same isolation for a rendition the broadcast refuses by name (no handler will ever
	/// serve it) rather than one that merely never answers.
	#[test]
	fn a_refused_rendition_does_not_end_the_session() {
		let _pad_ids = pad_ids();
		let element = element();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();

		// Both renditions in one snapshot, so the result can't hinge on which update the
		// session read: with no handler alive, `audio` resolves `NotFound` rather than parking,
		// and whichever order `plan.add` visits them in, `video` still has to reach a pad and
		// the session still has to end cleanly.
		let _video = broadcast.create_track("video", None).unwrap();
		{
			let mut guard = catalog.lock();
			guard.audio.renditions = BTreeMap::from([("audio".to_string(), audio_rendition())]);
			guard.video.renditions = BTreeMap::from([("video".to_string(), video_rendition())]);
		}

		let (shutdown, mut shutdown_rx) = watch::channel(false);
		let consumer = broadcast.consume();
		let weak = element.downgrade();
		let session = super::RUNTIME.spawn(async move { follow_catalog(consumer, weak, &mut shutdown_rx).await });

		await_pad(&element, "video_");

		let _ = shutdown.send(true);
		super::RUNTIME.block_on(session).unwrap().unwrap();
	}

	/// A subscription can resolve after its pump was already torn down. The pump has to stay
	/// dead: exposing a pad at that point publishes a rendition the session has finished with,
	/// and then yanks it without an EOS.
	#[test]
	fn a_subscription_resolving_after_cancellation_creates_no_pad() {
		let _pad_ids = pad_ids();
		let element = element();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut dynamic = broadcast.dynamic();
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();

		{
			let mut guard = catalog.lock();
			guard.video.renditions = BTreeMap::from([("stalled".to_string(), video_rendition())]);
		}

		let (shutdown, mut shutdown_rx) = watch::channel(false);
		let consumer = broadcast.consume();
		let weak = element.downgrade();
		let session = super::RUNTIME.spawn(async move { follow_catalog(consumer, weak, &mut shutdown_rx).await });

		// Hold the subscription pending, then drop the rendition from the catalog so its pump
		// is cancelled while it is still waiting.
		let request = await_request(&mut dynamic);
		{
			let mut guard = catalog.lock();
			guard.video.renditions.clear();
		}

		// Only now answer it. The pump was torn down, so nothing may reach a pad. Proving a pad
		// never appears has no edge to wait on, unlike `await_pad`, so this gives the runtime a
		// window in which the un-cancelled version reliably creates one.
		let _serving = request.accept(moq_net::track::Info::default());
		std::thread::sleep(Duration::from_millis(500));
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
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();

		let _video = broadcast.create_track("video", None).unwrap();
		{
			let mut guard = catalog.lock();
			guard.video.renditions = BTreeMap::from([("video".to_string(), video_rendition())]);
		}
		catalog.finish().unwrap();

		let (shutdown, mut shutdown_rx) = watch::channel(false);
		let consumer = broadcast.consume();
		let weak = element.downgrade();
		let session = super::RUNTIME.spawn(async move { follow_catalog(consumer, weak, &mut shutdown_rx).await });

		await_pad(&element, "video_");

		let _ = shutdown.send(true);
		super::RUNTIME.block_on(session).unwrap().unwrap();
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
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();

		// A video rendition nobody serves, alongside an audio one that arrives. The audio pad
		// is the signal that this update was reconciled, so the second update below is a
		// separate one and the stalled rendition had its chance to claim an id first.
		let _audio = broadcast.create_track("audio", None).unwrap();
		{
			let mut guard = catalog.lock();
			guard.video.renditions = BTreeMap::from([("stalled".to_string(), video_rendition())]);
			guard.audio.renditions = BTreeMap::from([("audio".to_string(), audio_rendition())]);
		}

		let first = NEXT_VIDEO_PAD_ID.load(Ordering::Relaxed);
		let (shutdown, mut shutdown_rx) = watch::channel(false);
		let consumer = broadcast.consume();
		let weak = element.downgrade();
		let session = super::RUNTIME.spawn(async move { follow_catalog(consumer, weak, &mut shutdown_rx).await });

		await_pad(&element, "audio_");

		let _video = broadcast.create_track("video", None).unwrap();
		{
			let mut guard = catalog.lock();
			guard.video.renditions.insert("video".to_string(), video_rendition());
		}

		let pad = await_pad(&element, "video_");
		assert_eq!(pad.name(), format!("video_{first}"));

		let _ = shutdown.send(true);
		super::RUNTIME.block_on(session).unwrap().unwrap();
	}
}
