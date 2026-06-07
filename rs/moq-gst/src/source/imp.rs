use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
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
/// runs one [pump](run_pump) per active rendition. The element just starts and
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
			if let Err(err) = run_session(settings, element.clone(), &mut shutdown_rx).await {
				if let Some(obj) = element.upgrade() {
					gst::element_error!(obj, gst::CoreError::Failed, ("session error"), ["{err:?}"]);
				}
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

/// A rendition we're currently serving, keyed in the session by moq track name.
struct ActiveTrack {
	/// The negotiated caps; a catalog update that changes them recreates the pad.
	caps: gst::Caps,
	/// The wire container; a change (e.g. legacy -> cmaf) also recreates the pad.
	container: hang::catalog::Container,
	/// Tells the pump to drop its pad and exit (set on shutdown or when reconcile
	/// removes/replaces the rendition).
	cancel: watch::Sender<bool>,
	/// Handle to the pump task in the session's `JoinSet`. We only read
	/// `is_finished()` to prune this entry once the pump ends (the `JoinSet` owns
	/// the task and reaps it); teardown goes through `cancel`, never `abort()`.
	task: tokio::task::AbortHandle,
}

async fn run_session(
	settings: ResolvedSettings,
	element: glib::WeakRef<super::MoqSrc>,
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

	// Follow the catalog for the whole session and reconcile our pumps against every update,
	// rather than building them once from the first frame. This covers reactive publishers
	// (the browser via @moq/hang) that announce an empty catalog before their encoder
	// configures, then add renditions a beat later, as well as renditions appearing,
	// disappearing, or changing codec/resolution mid-stream.
	let mut active: HashMap<String, ActiveTrack> = HashMap::new();
	let mut pumps: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
	let mut next_pad_id: u64 = 0;
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
			// Full session shutdown: cancel every pump, then wait for them all to drop
			// their pads. Cancel all up front (pumps only exit on their own `cancel`), or
			// the not-yet-cancelled ones would keep streaming while we await the rest.
			_ = shutdown.changed() => {
				for (_, track) in active.drain() {
					let _ = track.cancel.send(true);
				}
				while pumps.join_next().await.is_some() {}
				break;
			}
			// A pump finished; loop back so the `retain` above prunes its entry and the
			// break condition sees the drained set.
			_ = pumps.join_next(), if !pumps.is_empty() => {}
			// The guard stops us polling a closed catalog track (which would spin the loop
			// returning None) while we wait for the remaining pumps to drain.
			next = catalog_consumer.next(), if !catalog_closed => {
				match next? {
					Some(catalog) => reconcile(&catalog, &mut active, &mut pumps, &mut next_pad_id, &broadcast, &element)?,
					// Catalog track closed. Don't cancel the pumps: let each reach its
					// natural Ok(None) -> EOS end so downstream sees a clean EOS rather than a
					// bare pad drop. We just stop reconciling and wait for them to drain.
					None => catalog_closed = true,
				}
			}
		}
	}

	Ok(())
}

/// Bring the live set of pumps in line with `catalog`: spawn pumps for newly announced
/// renditions, tear down ones that vanished, and recreate any whose caps or container changed.
fn reconcile(
	catalog: &hang::catalog::Catalog,
	active: &mut HashMap<String, ActiveTrack>,
	pumps: &mut tokio::task::JoinSet<()>,
	next_pad_id: &mut u64,
	broadcast: &moq_net::BroadcastConsumer,
	element: &glib::WeakRef<super::MoqSrc>,
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

	// Drop anything that disappeared or changed shape; survivors stay put, and the changed
	// ones get respawned below under a fresh pad id. Each cancelled pump drops its own pad.
	active.retain(|name, track| match desired.get(name) {
		Some(d) if d.caps == track.caps && d.container_hint == track.container => true,
		_ => {
			let _ = track.cancel.send(true);
			false
		}
	});

	// Spawn pumps for the renditions we aren't already serving.
	for (name, d) in desired {
		if active.contains_key(&name) {
			continue;
		}

		let id = *next_pad_id;
		*next_pad_id += 1;

		let track_consumer = broadcast.subscribe_track(&moq_net::Track::new(&name))?;
		let track = moq_mux::container::Consumer::new(track_consumer, d.container).with_latency(Duration::from_secs(1));

		let descriptor = TrackDescriptor {
			kind: d.kind,
			name: name.clone(),
			id,
		};
		let (cancel_tx, cancel_rx) = watch::channel(false);
		let task = pumps.spawn_on(
			run_pump(element.clone(), descriptor, d.caps.clone(), track, cancel_rx),
			RUNTIME.handle(),
		);

		active.insert(
			name,
			ActiveTrack {
				caps: d.caps,
				container: d.container_hint,
				cancel: cancel_tx,
				task,
			},
		);
	}

	Ok(())
}

/// Identifies a pump's pad. Pads are named `video_<id>` / `audio_<id>` from a
/// process-unique counter (matching the `%u` templates) rather than after the
/// track name, so a rendition can be torn down and recreated (when its
/// codec/resolution changes mid-stream) without two pads ever sharing a name.
struct TrackDescriptor {
	kind: TrackKind,
	name: String,
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

/// Reads frames from one track and pushes them to a pad it owns for its whole lifetime:
/// it creates the pad, streams buffers, and removes the pad on exit. Runs until the track
/// ends (EOS), errors, or `cancel` fires.
async fn run_pump(
	element: glib::WeakRef<super::MoqSrc>,
	descriptor: TrackDescriptor,
	caps: gst::Caps,
	mut track: moq_mux::container::Consumer<moq_mux::catalog::hang::Container>,
	mut cancel: watch::Receiver<bool>,
) {
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
					if pad.push(buffer).is_err() {
						break;
					}
				}
				Ok(None) => {
					let _ = pad.push_event(gst::event::Eos::builder().build());
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
	reference_ts: &mut Option<moq_mux::container::Timestamp>,
	kind: TrackKind,
) -> gst::Buffer {
	let mut buffer = gst::Buffer::from_slice(frame.payload);
	let buffer_mut = buffer.get_mut().unwrap();

	let pts = match *reference_ts {
		Some(reference) => {
			let delta: Duration = (frame.timestamp - reference).into();
			gst::ClockTime::from_nseconds(delta.as_nanos() as u64)
		}
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
		other => bail!("unsupported audio codec: {other:?}"),
	};
	Ok(caps)
}
