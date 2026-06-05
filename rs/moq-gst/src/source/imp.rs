use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use tokio::sync::{mpsc, oneshot, watch};

use hang::moq_net;

static CAT: LazyLock<gst::DebugCategory> =
	LazyLock::new(|| gst::DebugCategory::new("moq-src", gst::DebugColorFlags::empty(), Some("MoQ Source Element")));

static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
	tokio::runtime::Builder::new_multi_thread()
		.enable_all()
		.build()
		.expect("spawn tokio runtime")
});

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

#[derive(Debug, Clone)]
struct TrackDescriptor {
	kind: TrackKind,
	/// The moq track name to subscribe to / the GStreamer stream id.
	name: String,
	/// A process-unique pad index. Pads are named `video_<id>` / `audio_<id>`
	/// (matching the `%u` templates) rather than after the track name, so a
	/// rendition can be torn down and recreated (when its codec/resolution
	/// changes mid-stream) without two pads ever sharing a name.
	id: u64,
}

impl TrackDescriptor {
	fn pad_name(&self) -> String {
		match self.kind {
			TrackKind::Video => format!("video_{}", self.id),
			TrackKind::Audio => format!("audio_{}", self.id),
		}
	}
}

#[derive(Debug)]
enum ControlMessage {
	CreatePad {
		descriptor: TrackDescriptor,
		caps: gst::Caps,
		reply: oneshot::Sender<PadEndpoint>,
	},
	ReportError(anyhow::Error),
}

#[derive(Debug)]
enum PadMessage {
	Buffer(gst::Buffer),
	Eos,
	Drop,
}

#[derive(Debug, Clone)]
struct PadEndpoint {
	sender: mpsc::UnboundedSender<PadMessage>,
}

impl PadEndpoint {
	fn send(&self, msg: PadMessage) -> bool {
		self.sender.send(msg).is_ok()
	}
}

struct PadHandle {
	sender: mpsc::UnboundedSender<PadMessage>,
	task: glib::JoinHandle<()>,
}

struct SessionController {
	shutdown: watch::Sender<bool>,
	join: tokio::task::JoinHandle<()>,
}

impl SessionController {
	fn start(settings: ResolvedSettings, control_tx: mpsc::UnboundedSender<ControlMessage>) -> Self {
		let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
		let control_for_error = control_tx.clone();
		let join = RUNTIME.spawn(async move {
			let result = run_session(settings, control_tx, &mut shutdown_rx).await;
			if let Err(err) = result {
				let _ = control_for_error.send(ControlMessage::ReportError(err));
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
	pads: Mutex<HashMap<String, PadHandle>>,
	control_task: Mutex<Option<glib::JoinHandle<()>>>,
	control_sender: Mutex<Option<mpsc::UnboundedSender<ControlMessage>>>,
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
					.build(),
				glib::ParamSpecString::builder("broadcast")
					.nick("Broadcast")
					.blurb("The broadcast name to subscribe to")
					.build(),
				glib::ParamSpecBoolean::builder("tls-disable-verify")
					.nick("TLS Disable Verify")
					.blurb("Disable TLS certificate verification")
					.default_value(false)
					.build(),
			]
		});
		PROPS.as_ref()
	}

	fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
		let mut settings = self.settings.lock().unwrap();
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
				let success = self.parent_change_state(transition)?;
				let result = match success {
					gst::StateChangeSuccess::Async => gst::StateChangeSuccess::Async,
					_ => gst::StateChangeSuccess::NoPreroll,
				};
				Ok(result)
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
		let settings = {
			let settings = self.settings.lock().unwrap().clone();
			ResolvedSettings::try_from(settings)?
		};

		let (control_tx, control_rx) = mpsc::unbounded_channel();
		let obj = self.obj();
		let weak = obj.downgrade();
		let context = glib::MainContext::default();
		let control_task = spawn_main_context_forwarder(&context, control_rx, move |msg| {
			if let Some(obj) = weak.upgrade() {
				obj.imp().handle_control_message(msg);
				true
			} else {
				false
			}
		});

		*self.control_task.lock().unwrap() = Some(control_task);
		*self.control_sender.lock().unwrap() = Some(control_tx.clone());

		let session = SessionController::start(settings, control_tx);
		*self.session.lock().unwrap() = Some(session);
		Ok(())
	}

	fn stop_session(&self) {
		if let Some(session) = self.session.lock().unwrap().take() {
			session.stop();
		}

		if let Some(control_task) = self.control_task.lock().unwrap().take() {
			control_task.abort();
		}

		let handles = self.pads.lock().unwrap().drain().collect::<Vec<_>>();
		for (name, handle) in handles {
			gst::debug!(CAT, "dropping pad {name}");
			let _ = handle.sender.send(PadMessage::Drop);
			handle.task.abort();
		}

		*self.control_sender.lock().unwrap() = None;
	}

	fn handle_control_message(&self, msg: ControlMessage) {
		match msg {
			ControlMessage::CreatePad {
				descriptor,
				caps,
				reply,
			} => {
				if let Err(err) = self.create_pad(descriptor, caps, reply) {
					gst::error!(CAT, obj = self.obj(), "failed to create pad: {err:?}");
				}
			}
			ControlMessage::ReportError(err) => {
				gst::element_error!(self.obj(), gst::CoreError::Failed, ("session error"), ["{err:?}"]);
			}
		}
	}

	fn create_pad(
		&self,
		descriptor: TrackDescriptor,
		caps: gst::Caps,
		reply: oneshot::Sender<PadEndpoint>,
	) -> Result<()> {
		let obj = self.obj();
		let templ = obj
			.element_class()
			.pad_template(descriptor.kind.template_name())
			.context("missing pad template")?;

		let pad = gst::Pad::builder_from_template(&templ)
			.name(descriptor.pad_name())
			.build();

		pad.set_active(true)?;

		let stream_start = gst::event::StreamStart::builder(&descriptor.name)
			.group_id(gst::GroupId::next())
			.build();
		pad.push_event(stream_start);
		pad.push_event(gst::event::Caps::new(&caps));
		pad.push_event(gst::event::Segment::new(&gst::FormattedSegment::<gst::ClockTime>::new()));

		obj.add_pad(&pad)?;

		let (pad_tx, pad_rx) = mpsc::unbounded_channel();
		let pad_clone = pad.clone();
		let weak = obj.downgrade();
		let context = glib::MainContext::default();
		let task = spawn_main_context_forwarder(&context, pad_rx, move |msg| {
			if let Some(obj) = weak.upgrade() {
				let imp = obj.imp();
				imp.dispatch_pad_message(&pad_clone, msg)
			} else {
				false
			}
		});

		self.pads.lock().unwrap().insert(
			descriptor.pad_name(),
			PadHandle {
				sender: pad_tx.clone(),
				task,
			},
		);

		let _ = reply.send(PadEndpoint { sender: pad_tx });
		Ok(())
	}

	fn dispatch_pad_message(&self, pad: &gst::Pad, msg: PadMessage) -> bool {
		match msg {
			PadMessage::Buffer(buffer) => {
				if let Err(err) = pad.push(buffer) {
					gst::warning!(CAT, "failed to push buffer: {err:?}");
					return false;
				}
				true
			}
			PadMessage::Eos => {
				pad.push_event(gst::event::Eos::builder().build());
				true
			}
			PadMessage::Drop => {
				let _ = pad.set_active(false);
				let _ = self.obj().remove_pad(pad);
				// Evict the handle so a long-lived session that keeps swapping
				// renditions doesn't accumulate dead entries.
				self.pads.lock().unwrap().remove(pad.name().as_str());
				false
			}
		}
	}
}

async fn run_session(
	settings: ResolvedSettings,
	control_tx: mpsc::UnboundedSender<ControlMessage>,
	shutdown: &mut watch::Receiver<bool>,
) -> Result<()> {
	let mut config = moq_native::ClientConfig::default();
	config.tls.disable_verify = Some(settings.tls_disable_verify);

	let origin = moq_net::Origin::random().produce();
	let origin_consumer = origin.consume();
	let client = config.init()?.with_consume(origin);

	let _session = client.connect(settings.url.clone()).await?;

	// Wait for the broadcast to be announced. Synchronous lookup would race the gossip of
	// announcements that happens after the session is established.
	tracing::info!(broadcast = %settings.broadcast, "waiting for broadcast to be announced");
	let broadcast = tokio::select! {
		broadcast = origin_consumer.announced_broadcast(&settings.broadcast) => broadcast
			.context("broadcast not allowed or origin closed")?,
		_ = shutdown.changed() => return Ok(()),
	};

	let catalog_track = broadcast.subscribe_track(&hang::catalog::Catalog::default_track())?;
	let mut catalog_consumer = moq_mux::catalog::hang::Consumer::new(catalog_track);

	// Follow the catalog for the whole session and reconcile our pads against every
	// update, rather than building them once from the first frame. This covers:
	//   - reactive publishers (the browser via @moq/hang) that announce an empty
	//     catalog before their encoder configures, then add renditions a beat later;
	//   - renditions appearing, disappearing, or changing codec/resolution mid-stream.
	// `Consumer::next` already coalesces to the newest catalog, so we only ever
	// reconcile against the latest published state.
	let mut active: HashMap<String, ActiveTrack> = HashMap::new();
	let (done_tx, mut done_rx) = mpsc::unbounded_channel::<TrackDone>();
	let mut next_pad_id: u64 = 0;
	// A detached clone for the pumps, kept separate so the `shutdown.changed()`
	// branch below can borrow `shutdown` mutably without conflicting with reconcile.
	let pump_shutdown = shutdown.clone();

	let mut catalog_closed = false;

	loop {
		// Done once the catalog is closed and every pump has drained on its own.
		// Those pumps already emitted EOS downstream via their natural end path.
		if catalog_closed && active.is_empty() {
			break;
		}

		tokio::select! {
			// Full session shutdown: cancel any pumps still running. (They watch this
			// same signal too, so this is belt-and-suspenders for a prompt teardown.)
			_ = shutdown.changed() => {
				for (_, track) in active.drain() {
					let _ = track.cancel.send(true);
				}
				break;
			}
			// A pump ended on its own (track finished or errored). Forget it so a
			// later catalog update can resubscribe if the rendition reappears -- but
			// only if it's still the pump we have on file, since a replaced rendition
			// reuses the name under a fresh id.
			Some(done) = done_rx.recv() => {
				if active.get(&done.name).is_some_and(|track| track.id == done.id) {
					active.remove(&done.name);
				}
			}
			// The guard stops us polling a closed catalog track (which would spin the
			// loop returning None) while we wait for the pumps to drain.
			next = catalog_consumer.next(), if !catalog_closed => {
				match next? {
					Some(catalog) => {
						reconcile(
							&catalog,
							&mut active,
							&mut next_pad_id,
							&broadcast,
							&control_tx,
							&done_tx,
							&pump_shutdown,
						)
						.await?;
						// We deliberately never emit `no_more_pads`: a later catalog can
						// add renditions at any time, so we can't promise the pad set is
						// complete. The `Sometimes` pads link on pad-added, and the one
						// moment we'd truly know there are no more pads -- the catalog
						// closing -- coincides with the streams ending (EOS) anyway.
					}
					// Catalog track closed. Don't cancel the pumps -- let each reach
					// its natural Ok(None) -> EOS end so downstream sees a clean EOS
					// rather than a bare pad drop. We just stop reconciling and wait
					// for the pumps to drain (or for a global shutdown).
					None => catalog_closed = true,
				}
			}
		}
	}

	Ok(())
}

/// A track whose pump is currently running, keyed in the session by moq track name.
struct ActiveTrack {
	/// Identifies this particular pump, so a stale completion from a replaced
	/// rendition (same name, older id) doesn't evict its successor.
	id: u64,
	/// The negotiated caps; a catalog update that changes them recreates the pad.
	caps: gst::Caps,
	/// The wire container; a change (e.g. legacy -> cmaf) also recreates the pad.
	container: hang::catalog::Container,
	/// Signals the pump to drop its pad and exit.
	cancel: watch::Sender<bool>,
}

/// Posted by a pump when it exits on its own (end of track or error).
struct TrackDone {
	name: String,
	id: u64,
}

/// Bring the live set of pads in line with `catalog`: spawn pumps for newly
/// announced renditions, tear down ones that vanished, and recreate any whose
/// caps or container changed.
#[allow(clippy::too_many_arguments)]
async fn reconcile(
	catalog: &hang::catalog::Catalog,
	active: &mut HashMap<String, ActiveTrack>,
	next_pad_id: &mut u64,
	broadcast: &moq_net::BroadcastConsumer,
	control_tx: &mpsc::UnboundedSender<ControlMessage>,
	done_tx: &mpsc::UnboundedSender<TrackDone>,
	shutdown: &watch::Receiver<bool>,
) -> Result<()> {
	struct Desired {
		kind: TrackKind,
		caps: gst::Caps,
		/// The hang container descriptor, kept for change detection against an [`ActiveTrack`].
		container_hint: hang::catalog::Container,
		/// The resolved wire container, moved into the pump when we spawn it.
		container: moq_mux::catalog::hang::Container,
	}

	// Resolve caps and the wire container up front. A rendition we can't handle (unsupported
	// codec, malformed CMAF init) is logged and skipped rather than failing the whole session,
	// so one bad rendition in a catalog update can't tear down the others we're already serving.
	let resolve = |kind, caps: Result<gst::Caps>, hint: &hang::catalog::Container| -> Result<Desired> {
		Ok(Desired {
			kind,
			caps: caps?,
			container: moq_mux::catalog::hang::Container::try_from(hint)?,
			container_hint: hint.clone(),
		})
	};

	let mut desired: HashMap<String, Desired> = HashMap::new();
	for (name, config) in &catalog.video.renditions {
		match resolve(TrackKind::Video, video_caps(config), &config.container) {
			Ok(d) => {
				desired.insert(name.clone(), d);
			}
			Err(err) => gst::warning!(CAT, "ignoring video rendition {name}: {err:?}"),
		}
	}
	for (name, config) in &catalog.audio.renditions {
		match resolve(TrackKind::Audio, audio_caps(config), &config.container) {
			Ok(d) => {
				desired.insert(name.clone(), d);
			}
			Err(err) => gst::warning!(CAT, "ignoring audio rendition {name}: {err:?}"),
		}
	}

	// Drop anything that disappeared or changed shape; survivors stay put, and the
	// changed ones get re-added below under a fresh pad id.
	active.retain(|name, track| match desired.get(name) {
		Some(d) if d.caps == track.caps && d.container_hint == track.container => true,
		_ => {
			let _ = track.cancel.send(true);
			false
		}
	});

	// Add the renditions we aren't already serving.
	for (name, d) in desired {
		if active.contains_key(&name) {
			continue;
		}

		let id = *next_pad_id;
		*next_pad_id += 1;

		let descriptor = TrackDescriptor {
			kind: d.kind,
			name: name.clone(),
			id,
		};
		let endpoint = request_pad(control_tx, descriptor.clone(), d.caps.clone()).await?;

		let track_consumer = broadcast.subscribe_track(&moq_net::Track::new(&name))?;
		let track = moq_mux::container::Consumer::new(track_consumer, d.container).with_latency(Duration::from_secs(1));

		let (cancel_tx, cancel_rx) = watch::channel(false);
		spawn_track_pump(
			track,
			descriptor,
			endpoint,
			shutdown.clone(),
			cancel_rx,
			done_tx.clone(),
		);

		active.insert(
			name,
			ActiveTrack {
				id,
				caps: d.caps,
				container: d.container_hint,
				cancel: cancel_tx,
			},
		);
	}

	Ok(())
}

async fn request_pad(
	control_tx: &mpsc::UnboundedSender<ControlMessage>,
	descriptor: TrackDescriptor,
	caps: gst::Caps,
) -> Result<PadEndpoint> {
	let (reply_tx, reply_rx) = oneshot::channel();
	control_tx
		.send(ControlMessage::CreatePad {
			descriptor,
			caps,
			reply: reply_tx,
		})
		.map_err(|_| anyhow::anyhow!("control plane shut down"))?;

	let endpoint = reply_rx.await.context("pad creation cancelled")?;
	Ok(endpoint)
}

fn spawn_track_pump(
	track: moq_mux::container::Consumer<moq_mux::catalog::hang::Container>,
	descriptor: TrackDescriptor,
	pad_endpoint: PadEndpoint,
	shutdown: watch::Receiver<bool>,
	cancel: watch::Receiver<bool>,
	done: mpsc::UnboundedSender<TrackDone>,
) -> tokio::task::JoinHandle<()> {
	RUNTIME.spawn(run_track_pump(track, descriptor, pad_endpoint, shutdown, cancel, done))
}

async fn run_track_pump(
	mut track: moq_mux::container::Consumer<moq_mux::catalog::hang::Container>,
	descriptor: TrackDescriptor,
	pad_endpoint: PadEndpoint,
	mut shutdown: watch::Receiver<bool>,
	mut cancel: watch::Receiver<bool>,
	done: mpsc::UnboundedSender<TrackDone>,
) {
	let mut reference_ts = None;
	loop {
		tokio::select! {
			// Whole session going away.
			_ = shutdown.changed() => {
				pad_endpoint.send(PadMessage::Drop);
				break;
			}
			// Just this rendition being torn down (dropped or replaced by a catalog update).
			_ = cancel.changed() => {
				pad_endpoint.send(PadMessage::Drop);
				break;
			}
			frame = track.read() => {
				match frame {
					Ok(Some(frame)) => {
						let timestamp = frame.timestamp;
						let is_keyframe = frame.keyframe;
						let payload = frame.payload;
						let mut buffer = gst::Buffer::from_slice(payload.to_vec());
						let buffer_mut = buffer.get_mut().unwrap();

						let pts = match reference_ts {
							Some(reference) => {
								let delta: Duration = (timestamp - reference).into();
								gst::ClockTime::from_nseconds(delta.as_nanos() as u64)
							}
							None => {
								reference_ts = Some(timestamp);
								gst::ClockTime::ZERO
							}
						};
						buffer_mut.set_pts(Some(pts));

						let mut flags = buffer_mut.flags();
						match descriptor.kind {
							TrackKind::Video => {
								if is_keyframe {
									flags.remove(gst::BufferFlags::DELTA_UNIT);
								} else {
									flags.insert(gst::BufferFlags::DELTA_UNIT);
								}
							}
							TrackKind::Audio => {
								flags.remove(gst::BufferFlags::DELTA_UNIT);
							}
						}
						buffer_mut.set_flags(flags);

						if !pad_endpoint.send(PadMessage::Buffer(buffer)) {
							break;
						}
					}
					Ok(None) => {
						pad_endpoint.send(PadMessage::Eos);
						pad_endpoint.send(PadMessage::Drop);
						break;
					}
					Err(err) => {
						gst::warning!(CAT, "track {} failed: {err:?}", descriptor.name);
						pad_endpoint.send(PadMessage::Drop);
						break;
					}
				}
			}
		}
	}

	// Let the session know this pump is gone so it can resubscribe if the rendition
	// reappears in a later catalog. Best-effort: the receiver is gone during shutdown.
	let _ = done.send(TrackDone {
		name: descriptor.name.clone(),
		id: descriptor.id,
	});
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
		other => bail!("unsupported audio codec: {other:?}"),
	};
	Ok(caps)
}

fn spawn_main_context_forwarder<T, F>(
	context: &glib::MainContext,
	mut rx: mpsc::UnboundedReceiver<T>,
	mut handler: F,
) -> glib::JoinHandle<()>
where
	T: Send + 'static,
	F: FnMut(T) -> bool + 'static,
{
	let ctx = context.clone();
	ctx.spawn_local(async move {
		while let Some(msg) = rx.recv().await {
			if !handler(msg) {
				break;
			}
		}
	})
}
