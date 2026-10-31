//! GObject shell for the moqsink element, on a bare GstElement.
//!
//! Each request pad has its own chain function that writes buffers straight into the moq producers
//! from the streaming thread. There is no intermediate channel and no worker task: `moq_net`'s producer
//! writes are synchronous (an in-memory append, bounded by group eviction), so the streaming thread
//! never blocks on the network. A thin async task only owns connect and the session lifetime. Pads are
//! fully independent: one pad's chain never waits on another's data.
//!
//! Locks are nested in one direction only: GStreamer's stream lock, then the element control, then a
//! pad lifecycle, then an object lock. No path takes the element control while holding a pad lifecycle.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use hang::moq_net;

use super::pad::{CapsOutcome, Names, ProducerOptions, PushOutcome, caps_supported};
use super::request_pad::{MoqSinkPad, Notifications, PadLifecycle};
use super::session::{
	CAT, Completion, CompletionHandle, ConnectionStatus, RUNTIME, ResolvedSettings, Session, SessionRegistration,
	notify, sessions_structure,
};

#[derive(Debug, Clone)]
struct Settings {
	url: Option<String>,
	broadcast: Option<String>,
	connection: crate::connection::Settings,
	linger: Duration,
}

impl Default for Settings {
	fn default() -> Self {
		Self {
			url: None,
			broadcast: None,
			connection: Default::default(),
			linger: moq_mux::Linger::DEFAULT,
		}
	}
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

/// Live state for one READY to PAUSED run.
struct State {
	session: Session,
	broadcast: moq_net::origin::Publication,
	catalog: Option<moq_mux::catalog::Producer>,
	wall: std::sync::Arc<super::wall::Wall>,
	/// The configured track names this run ended, which it never publishes again.
	names: Names,
	/// How long a clean end keeps the finished broadcast up before its EOS.
	linger: Duration,
	/// Whether the finished broadcast closed, so the EOS it owes may go out.
	drained: bool,
	/// The task serving the finished broadcast for its linger, aborted on stop.
	lingering: Option<tokio::task::AbortHandle>,
	/// Whether this publication's single EOS message already went out. One per publication, not per
	/// entry to PLAYING: a finished publication is terminal here, so there is nothing to announce twice.
	eos_delivered: bool,
}

impl State {
	/// Whether this publication still accepts pads, negotiation and buffers.
	fn is_open(&self) -> bool {
		self.session.completion().is_open()
	}
}

/// Element state that survives session replacement.
#[derive(Default)]
struct Control {
	live: Option<State>,
	/// Pads admitted before add_pad and its reentrant notifications finish.
	admissions: HashSet<MoqSinkPad>,
	/// Whether the element completed its transition into PLAYING. Tracked here rather than read from
	/// GStreamer: the current state is not committed until `change_state` returns, so a message earned
	/// inside that window would read PAUSED and be held with nobody left to release it.
	playing: bool,
}

/// Claim this publication's EOS, if it is owed and may go out now.
///
/// One claim per publication, so whichever arrives first (the transition into PLAYING, or the end of
/// the linger) is the only one that posts. Called under `control`, which is what serializes the two.
fn claim_eos(control: &mut Control) -> Posted {
	if !control.playing {
		return Posted::Nothing;
	}
	let Some(state) = control.live.as_mut() else {
		return Posted::Nothing;
	};
	// Only a clean finish drains, so a drained publication ended with EOS.
	if state.eos_delivered || !state.drained {
		return Posted::Nothing;
	}
	state.eos_delivered = true;
	Posted::Message {
		session: state.session.completion(),
		kind: MessageKind::Eos,
	}
}

/// A pad whose already-applied state change still needs GObject notification.
struct PadUpdate {
	pad: MoqSinkPad,
	changes: Notifications,
}

/// A deferred element message together with the publishing session that earned it.
#[derive(Default)]
enum Posted {
	/// Nothing to post: the pass did not run.
	#[default]
	Nothing,
	/// A message earned by one publishing session, identified by that session's completion handle.
	Message {
		session: CompletionHandle,
		kind: MessageKind,
	},
}

/// The element-wide outcome a publishing session earned.
enum MessageKind {
	/// Every pad ended and the broadcast closed.
	Eos,
	/// A producer failed to close, already formatted for the bus.
	FinalizeError(String),
	/// The reconnect task stopped on a terminal error.
	SessionError(String),
}

/// Whether confirming an admission left a usable pad.
enum Admission {
	Accepted,
	Rejected,
}

/// The result of a finalize pass: which pads to report, and what the element owes the bus.
#[derive(Default)]
struct Finished {
	updates: Vec<PadUpdate>,
	message: Posted,
}

/// The `moqsink` element implementation: its GObject properties plus the live session state.
#[derive(Default)]
pub struct MoqSink {
	settings: Mutex<Settings>,
	control: Mutex<Control>,
}

#[glib::object_subclass]
impl ObjectSubclass for MoqSink {
	const NAME: &'static str = "MoqSink";
	type Type = super::MoqSink;
	type ParentType = gst::Element;
	type Interfaces = (gst::ChildProxy,);
}

impl ObjectImpl for MoqSink {
	fn properties() -> &'static [glib::ParamSpec] {
		static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
			vec![
				glib::ParamSpecString::builder("url")
					.nick("Destination URL")
					.blurb("Connect to the given URL")
					.mutable_ready()
					.build(),
				glib::ParamSpecString::builder("broadcast")
					.nick("Broadcast")
					.blurb("The name of the broadcast to publish")
					.mutable_ready()
					.build(),
				glib::ParamSpecUInt64::builder("linger")
					.nick("Linger")
					.blurb("At end of stream, keep the finished broadcast up this many milliseconds for late subscribers, then post EOS; 0 closes it at once")
					.default_value(crate::connection::duration_millis(moq_mux::Linger::DEFAULT))
					.mutable_ready()
					.build(),
				// Read-only, served from the live session's status. Each notifies on change.
				glib::ParamSpecEnum::builder::<ConnectionStatus>("status")
					.nick("Connection status")
					.blurb("Publish connection lifecycle: disconnected (retrying), connected, or failed (gave up)")
					.read_only()
					.build(),
				glib::ParamSpecBoolean::builder("connected")
					.nick("Connected")
					.blurb("Whether the session is currently connected (status == connected)")
					.read_only()
					.build(),
				glib::ParamSpecString::builder("moq-version")
					.nick("Negotiated version")
					.blurb("The negotiated MoQ protocol version, null when disconnected")
					.read_only()
					.build(),
				glib::ParamSpecUInt64::builder("estimated-send-rate")
					.nick("Estimated send bitrate")
					.blurb("Estimated send bitrate in bits per second (congestion controller), 0 when unavailable")
					.read_only()
					.build(),
				glib::ParamSpecUInt64::builder("estimated-recv-rate")
					.nick("Estimated receive bitrate")
					.blurb("Estimated receive bitrate in bits per second, 0 when unavailable")
					.read_only()
					.build(),
				glib::ParamSpecBoxed::builder::<gst::Structure>("connection-stats")
					.nick("Connection statistics")
					.blurb("Current optional transport statistics, null while disconnected")
					.read_only()
					.build(),
				glib::ParamSpecBoxed::builder::<gst::Structure>("sessions")
					.nick("Sessions")
					.blurb("Cumulative connects and disconnects during this element session")
					.read_only()
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
			"linger" => settings.linger = Duration::from_millis(value.get().unwrap()),
			name => settings.connection.set(name, value),
		}
	}

	fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
		match pspec.name() {
			"status"
			| "connected"
			| "moq-version"
			| "estimated-send-rate"
			| "estimated-recv-rate"
			| "connection-stats"
			| "sessions" => {
				let control = self.control.lock().unwrap();
				let session = control.live.as_ref().map(|s| &s.session);
				match pspec.name() {
					"status" => session.map(|s| s.status().status()).unwrap_or_default().to_value(),
					"connected" => session.is_some_and(|s| s.status().connected()).to_value(),
					"moq-version" => session.and_then(|s| s.status().version()).to_value(),
					"estimated-send-rate" => session.map(|s| s.estimated_send_rate()).unwrap_or(0).to_value(),
					"estimated-recv-rate" => session.map(|s| s.estimated_recv_rate()).unwrap_or(0).to_value(),
					"connection-stats" => session.and_then(Session::connection_stats).to_value(),
					"sessions" => sessions_structure(session.map(Session::presence).unwrap_or_default()).to_value(),
					_ => unreachable!(),
				}
			}
			name => {
				let settings = self.settings.lock().unwrap();
				match name {
					"url" => settings.url.to_value(),
					"broadcast" => settings.broadcast.to_value(),
					"linger" => crate::connection::duration_millis(settings.linger).to_value(),
					name => settings.connection.get(name),
				}
			}
		}
	}

	fn constructed(&self) {
		self.parent_constructed();
		self.obj().set_element_flags(gst::ElementFlags::SINK);
	}
}

impl GstObjectImpl for MoqSink {}

/// Request pads are reachable as children, which is how a pipeline description names their tracks
/// (`moqsink sink_0::track=camera`).
impl ChildProxyImpl for MoqSink {
	fn children_count(&self) -> u32 {
		self.obj().num_pads() as u32
	}

	fn child_by_name(&self, name: &str) -> Option<glib::Object> {
		self.obj().static_pad(name).map(|pad| pad.upcast())
	}

	fn child_by_index(&self, index: u32) -> Option<glib::Object> {
		self.obj()
			.pads()
			.into_iter()
			.nth(index as usize)
			.map(|pad| pad.upcast())
	}
}

impl ElementImpl for MoqSink {
	fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
		static METADATA: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
			gst::subclass::ElementMetadata::new(
				"MoQ Sink",
				"Sink/Network/MoQ",
				"Transmits media over MoQ",
				"Luke Curley <kixelated@gmail.com>, Steve McFarlin <steve@stevemcfarlin.com>, Ariel Molina <ariel@edis.mx>",
			)
		});
		Some(&*METADATA)
	}

	fn pad_templates() -> &'static [gst::PadTemplate] {
		static PAD_TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
			// Every codec that converges on moq_mux::import::Track. The structural fields here
			// (byte-stream/au, AAC mpegversion/stream-format) are what negotiation enforces, so the
			// producer build does not re-check them.
			let mut caps = gst::Caps::new_empty();
			caps.merge(
				gst::Caps::builder("video/x-h264")
					.field("stream-format", "byte-stream")
					.field("alignment", "au")
					.build(),
			);
			caps.merge(
				gst::Caps::builder("video/x-h265")
					.field("stream-format", "byte-stream")
					.field("alignment", "au")
					.build(),
			);
			caps.merge(gst::Caps::builder("video/x-av1").build());
			caps.merge(gst::Caps::builder("video/x-vp8").build());
			caps.merge(gst::Caps::builder("video/x-vp9").build());
			caps.merge(
				gst::Caps::builder("audio/mpeg")
					.field("mpegversion", 4i32)
					.field("stream-format", "raw")
					.build(),
			);
			// MP3 (MPEG-1/2 Layer III). The frame header carries the config in band.
			caps.merge(
				gst::Caps::builder("audio/mpeg")
					.field("mpegversion", gst::List::new([1i32, 2i32]))
					.field("layer", 3i32)
					.build(),
			);
			caps.merge(gst::Caps::builder("audio/x-opus").build());
			// Subtitles: one decoded UTF-8 cue per buffer, as demuxers emit timed text.
			caps.merge(gst::Caps::builder("text/x-raw").field("format", "utf8").build());
			// Opaque application data: published byte for byte, so there is no structural field to pin.
			caps.merge(gst::Caps::builder("application/octet-stream").build());

			// The GType is what makes gst-inspect-1.0 list the pad's own properties; without it the
			// template reports none, however many the pad declares.
			let sink = gst::PadTemplate::builder("sink_%u", gst::PadDirection::Sink, gst::PadPresence::Request, &caps)
				.gtype(MoqSinkPad::static_type())
				.build()
				.unwrap();
			vec![sink]
		});
		PAD_TEMPLATES.as_ref()
	}

	fn request_new_pad(
		&self,
		templ: &gst::PadTemplate,
		name: Option<&str>,
		_caps: Option<&gst::Caps>,
	) -> Option<gst::Pad> {
		// Wrap both pad functions in catch_panic_pad_function: these run on the streaming thread across the
		// C FFI boundary, and they hit `state.lock().unwrap()` (poisonable) and `expect()`. An escaping
		// panic would abort the process; here it becomes a clean FlowError / `false` instead.
		let pad_builder = gst::PadBuilder::<MoqSinkPad>::from_template(templ)
			.chain_function(|pad, parent, buffer| {
				MoqSink::catch_panic_pad_function(
					parent,
					|| Err(gst::FlowError::Error),
					|this| this.forward_buffer(pad.upcast_ref::<gst::Pad>(), buffer),
				)
			})
			.event_function(|pad, parent, event| {
				MoqSink::catch_panic_pad_function(
					parent,
					|| false,
					|this| this.handle_event(pad.upcast_ref::<gst::Pad>(), event),
				)
			});

		let pad = match name {
			Some(name) => pad_builder.name(name).build(),
			None => pad_builder.generated_name().build(),
		};
		{
			let mut control = self.control.lock().unwrap();
			if control.live.as_ref().is_some_and(|state| !state.is_open()) {
				gst::warning!(CAT, "refusing a pad: the publication has ended");
				return None;
			}
			if let Some(state) = control.live.as_ref()
				&& let Some(catalog) = state.catalog.as_ref()
			{
				pad.lifecycle().media.reserve(catalog, state.wall.clone(), &state.names);
			}
			control.admissions.insert(pad.clone());
		}

		if self.obj().add_pad(&pad).is_err() {
			self.cancel_admission(&pad);
			return None;
		}
		if !self.owns_pad(&pad) {
			self.cancel_admission(&pad);
			return None;
		}
		self.obj().child_added(&pad, pad.name().as_str());
		let (admission, finished) = self.confirm_admission(&pad);
		self.publish_finished(finished);
		// The ownership check is a second question, about what the notifications above may have done.
		match admission {
			Admission::Accepted if self.owns_pad(&pad) => Some(pad.upcast()),
			_ => {
				match self.owns_pad(&pad) {
					true => self.release_pad(pad.upcast_ref()),
					false => pad.reset_detached(),
				}
				None
			}
		}
	}

	fn release_pad(&self, pad: &gst::Pad) {
		let Some(sink_pad) = pad.downcast_ref::<MoqSinkPad>() else {
			return;
		};
		if !self.owns_pad(sink_pad) {
			sink_pad.reset_detached();
			return;
		}

		let _ = pad.set_active(false);
		let changes = {
			let _rt = RUNTIME.enter();
			let mut control = self.control.lock().unwrap();
			control.admissions.remove(sink_pad);
			let mut lifecycle = sink_pad.lifecycle();
			if lifecycle.releasing {
				return;
			}
			lifecycle.releasing = true;
			if let Err(err) = lifecycle.media.finalize() {
				gst::warning!(CAT, "finalize on release {}: {err:?}", pad.name());
			}
			lifecycle.reset()
		};
		if self.obj().remove_pad(pad).is_ok() {
			self.obj().child_removed(pad, pad.name().as_str());
		}
		sink_pad.lifecycle().releasing = false;
		sink_pad.notify_changes(changes);
		let finished = {
			let mut control = self.control.lock().unwrap();
			self.maybe_finish_locked(&mut control)
		};
		self.publish_finished(finished);
	}

	fn change_state(&self, transition: gst::StateChange) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
		match transition {
			gst::StateChange::ReadyToPaused => {
				let registration = self.start_session()?;
				// Rolled back on a failed parent transition: otherwise the session would keep publishing
				// while the element sits in READY, with no later transition left to clean it up.
				match self.parent_change_state(transition) {
					Ok(success) => {
						registration.mark_registered();
						Ok(success)
					}
					Err(error) => {
						self.stop_session();
						Err(error)
					}
				}
			}
			// The parent finishes the transition before the element is allowed to speak for PLAYING.
			gst::StateChange::PausedToPlaying => {
				let success = self.parent_change_state(transition)?;
				self.enter_playing();
				Ok(success)
			}
			// Symmetric with the transition below: a failed parent is not committed, so the element is
			// still PLAYING and the gate stays open. Closing it anyway would strand an EOS earned
			// afterwards, because no later entry to PLAYING would come along to claim it.
			gst::StateChange::PlayingToPaused => {
				let success = self.parent_change_state(transition)?;
				self.leave_playing();
				Ok(success)
			}
			// The parent goes first so it deactivates the pads and waits for the streaming functions to
			// return before the session they write into is torn down. A failed parent leaves the element
			// in PAUSED rather than committing the transition, so the session stays with it: tearing down
			// anyway would leave a PAUSED element publishing nothing, with no transition left to build a
			// replacement. The retry cleans up.
			gst::StateChange::PausedToReady => {
				let success = self.parent_change_state(transition)?;
				self.leave_playing();
				self.stop_session();
				Ok(success)
			}
			_ => self.parent_change_state(transition),
		}
	}
}

impl MoqSink {
	/// Membership includes admissions that GStreamer has not made visible yet.
	fn pads_locked(&self, control: &Control) -> Vec<MoqSinkPad> {
		let mut seen = HashSet::new();
		self.obj()
			.sink_pads()
			.into_iter()
			.filter_map(|pad| pad.downcast::<MoqSinkPad>().ok())
			.chain(control.admissions.iter().cloned())
			.filter(|pad| seen.insert(pad.clone()))
			.collect()
	}

	/// Create the session and producers before any buffer flows.
	fn start_session(&self) -> Result<SessionRegistration, gst::StateChangeError> {
		let settings = self.settings.lock().unwrap().clone();
		let linger = settings.linger;
		let settings = ResolvedSettings::try_from(settings).map_err(|err| {
			gst::error!(CAT, obj = self.obj(), "invalid settings: {err:#}");
			gst::StateChangeError
		})?;
		let (session, registration, broadcast, catalog) =
			Session::start(settings, self.obj().downgrade()).map_err(|err| {
				gst::error!(CAT, obj = self.obj(), "failed to start session: {err:?}");
				gst::StateChangeError
			})?;
		let completion = session.completion();
		let wall = super::wall::Wall::new(&catalog);
		let updates = {
			let mut control = self.control.lock().unwrap();
			control.live = Some(State {
				session,
				broadcast,
				catalog: Some(catalog),
				wall,
				names: Names::default(),
				linger,
				drained: false,
				lingering: None,
				eos_delivered: false,
			});
			let pads = self.pads_locked(&control);
			let state = control.live.as_ref().unwrap();
			let catalog = state.catalog.as_ref().unwrap();
			let wall = &state.wall;
			pads.into_iter()
				.map(|pad| {
					let changes = {
						let mut lifecycle = pad.lifecycle();
						let changes = lifecycle.reset();
						lifecycle.completion = Some(completion.clone());
						if !lifecycle.releasing {
							lifecycle.media.reserve(catalog, wall.clone(), &state.names);
						}
						changes
					};
					PadUpdate { pad, changes }
				})
				.collect::<Vec<_>>()
		};
		self.notify_updates(updates);
		Ok(registration)
	}

	/// Finalize the producers (catalog last) and tear down the session. Finalize is best-effort: we are
	/// tearing down regardless.
	fn stop_session(&self) {
		let (mut state, updates, mut failure, closed) = {
			let mut control = self.control.lock().unwrap();
			let Some(state) = control.live.take() else {
				return;
			};
			let _rt = RUNTIME.enter();
			let mut failure = None;
			let mut closed = Vec::new();
			let pads = self.pads_locked(&control);
			let updates = pads
				.into_iter()
				.map(|pad| {
					let changes = {
						let mut lifecycle = pad.lifecycle();
						let (track, result) = lifecycle.media.finish();
						closed.push(track);
						if let Err(err) = result {
							gst::warning!(CAT, "finalize {} on stop: {err:?}", pad.name());
							if failure.is_none() {
								failure = Some(err);
							}
						}
						lifecycle.reset()
					};
					PadUpdate { pad, changes }
				})
				.collect::<Vec<_>>();
			(state, updates, failure, closed)
		};
		notify(
			&self.obj().downgrade(),
			&[
				"status",
				"connected",
				"moq-version",
				"estimated-send-rate",
				"estimated-recv-rate",
				"connection-stats",
				"sessions",
			],
		);
		let _rt = RUNTIME.enter();
		state.wall.finish();
		if let Some(mut catalog) = state.catalog.take()
			&& let Err(err) = catalog.finish().context("finalize catalog")
			&& failure.is_none()
		{
			failure = Some(err);
		}
		if let Some(err) = failure {
			gst::warning!(CAT, "finalize on stop: {err:?}");
		}
		// Close the broadcast before reaping the session task. This also ends a linger early.
		if let Some(lingering) = state.lingering.take() {
			lingering.abort();
		}
		state.broadcast.close();
		drop(closed);
		state.session.stop();
		self.notify_updates(updates);
	}

	/// Write one buffer straight into its pad's producer. Per-pad failures (bad caps/bitstream) drop
	/// quietly so the session and other pads keep going; an unmappable buffer or a dead session is a hard
	/// error on this pad's streaming thread.
	fn forward_buffer(&self, pad: &gst::Pad, buffer: gst::Buffer) -> Result<gst::FlowSuccess, gst::FlowError> {
		// Map and copy outside the lock so the per-pad lock covers only the producer write. Avoiding the
		// copy for an oversized buffer needs a reliable, media-aware size heuristic; moq-net remains the
		// authority and rejects FrameTooLarge before reserving its own group slot.
		// Only subtitles use this: a cue needs an explicit end, unlike a media frame.
		let duration = buffer.duration();
		let current_running_time = self.obj().current_running_time();
		// Preroll has a frozen running time before the pipeline starts its clock.
		let reference_running_time = current_running_time.or_else(|| {
			(self.obj().current_state() != gst::State::Playing)
				.then(|| self.obj().start_time())
				.flatten()
		});
		let sample = super::wall::Sample {
			running: reference_running_time,
			wall: moq_clock::wall(),
		};
		let map = buffer.map_readable().map_err(|_| {
			gst::error!(CAT, "failed to map buffer on pad {}", pad.name());
			gst::FlowError::Error
		})?;
		let data = Bytes::copy_from_slice(map.as_slice());
		drop(map);
		let Some(pad) = pad.downcast_ref::<MoqSinkPad>() else {
			return Err(gst::FlowError::Flushing);
		};

		let _rt = RUNTIME.enter();
		let (outcome, changes) = {
			let mut lifecycle = pad.lifecycle();
			let Some(completion) = lifecycle.completion.as_ref().filter(|_| !lifecycle.releasing) else {
				return Err(gst::FlowError::Flushing);
			};
			// The element's own end comes before the pad's: a broken rendition is isolated so the others
			// keep publishing, but a finished publication is not something one pad may report as OK.
			match completion.get() {
				Completion::Failed => return Err(gst::FlowError::Error),
				Completion::Eos => return Err(gst::FlowError::Eos),
				Completion::Open => {}
			}
			if lifecycle.media.is_failed() {
				return Ok(gst::FlowSuccess::Ok);
			}
			let outcome = lifecycle.media.place_wall(&buffer, sample).and_then(|()| {
				let pts = lifecycle.media.timeline_pts(&buffer)?;
				lifecycle
					.media
					.push_buffer(data, pts, duration, current_running_time)
					.map_err(anyhow::Error::msg)
			});
			let changes = match &outcome {
				Ok(PushOutcome::Failed(reason)) => Some(lifecycle.fail(reason.clone())),
				_ => None,
			};
			(outcome, changes)
		};
		if let Some(changes) = changes {
			pad.notify_changes(changes);
		}
		let outcome = match outcome {
			Ok(outcome) => outcome,
			Err(err) => {
				// Bus sync handlers run inline and may read a property that locks the lifecycle again.
				gst::element_error!(
					self.obj(),
					gst::StreamError::Format,
					("could not timestamp buffer on pad {}", pad.name()),
					["{err}"]
				);
				return Err(gst::FlowError::Error);
			}
		};
		if outcome == PushOutcome::NoSegment {
			gst::element_warning!(
				self.obj(),
				gst::StreamError::Format,
				(
					"pad {} received buffers with no TIME segment; nothing is published for it",
					pad.name()
				)
			);
		}
		Ok(gst::FlowSuccess::Ok)
	}

	fn handle_event(&self, pad: &gst::Pad, event: gst::Event) -> bool {
		let Some(sink_pad) = pad.downcast_ref::<MoqSinkPad>() else {
			return false;
		};
		match event.view() {
			gst::EventView::Caps(caps) => {
				let caps = caps.caps().to_owned();
				// A pure check, so it runs before any lock is taken.
				if !caps_supported(&caps) {
					gst::warning!(CAT, "rejecting unsupported caps on pad {}", pad.name());
					// The negotiation is refused either way, but recording it on the pad is not always
					// allowed. No publication yet is not the same as one that ended: with no session, or
					// with an open one, the pad takes the error; after a terminal state `ended` and
					// `error` are settled and a late caps event must not rewrite one as the other.
					let changes = {
						let _rt = RUNTIME.enter();
						let control = self.control.lock().unwrap();
						let terminal = control.live.as_ref().is_some_and(|state| !state.is_open());
						match terminal {
							true => None,
							false => {
								let mut lifecycle = sink_pad.lifecycle();
								match lifecycle.releasing {
									true => None,
									false => {
										lifecycle.media.invalidate();
										Some(lifecycle.fail(format!("unsupported caps: {caps}")))
									}
								}
							}
						}
					};
					if let Some(changes) = changes {
						sink_pad.notify_changes(changes);
					}
					return false;
				}
				// Negotiated under the control lock, but `event_default` runs after it drops: a sync
				// handler reached from there can re-enter the element and take the same lock.
				let negotiated = {
					let _rt = RUNTIME.enter();
					let control = self.control.lock().unwrap();
					if let Some(state) = control.live.as_ref().filter(|state| state.is_open())
						&& let Some(catalog) = state.catalog.as_ref()
					{
						let mut lifecycle = sink_pad.lifecycle();
						if lifecycle.releasing {
							return false;
						}
						Some(observe_caps(state, catalog, sink_pad, &mut lifecycle, &caps))
					} else {
						None
					}
				};
				// No live publication to negotiate against, so forward without touching the pad.
				let Some((changes, accepted)) = negotiated else {
					return gst::Pad::event_default(pad, Some(&*self.obj()), event);
				};
				if let Some(changes) = changes {
					sink_pad.notify_changes(changes);
				}
				accepted && gst::Pad::event_default(pad, Some(&*self.obj()), event)
			}
			gst::EventView::Segment(segment) => {
				let control = self.control.lock().unwrap();
				let completion = control.live.as_ref().map(|state| state.session.completion());
				let mut lifecycle = sink_pad.lifecycle();
				if !lifecycle.releasing {
					// The link is which publication the pad belongs to, not whether that publication is
					// still open. Dropping it after the end would answer the next buffer with Flushing
					// instead of the terminal result it earned.
					lifecycle.completion = completion;
					if lifecycle
						.completion
						.as_ref()
						.is_some_and(|completion| completion.is_open())
					{
						lifecycle.media.observe_segment(segment.segment().to_owned());
					}
				}
				drop(lifecycle);
				drop(control);
				gst::Pad::event_default(pad, Some(&*self.obj()), event)
			}
			gst::EventView::Eos(_) => {
				let mut retired = None;
				let finished = {
					let mut control = self.control.lock().unwrap();
					if let Some(state) = control.live.as_ref().filter(|state| state.is_open()) {
						let _rt = RUNTIME.enter();
						let mut lifecycle = sink_pad.lifecycle();
						if !lifecycle.releasing {
							lifecycle.ended = true;
							retired = lifecycle.retire_bandwidth();
							if let Some(catalog) = &state.catalog
								&& let Err(err) = lifecycle.media.end_unresolved(catalog)
							{
								lifecycle.media.invalidate();
								let changes = lifecycle.fail(format!("{err:#}"));
								drop(lifecycle);
								drop(control);
								if let Some(retired) = retired {
									sink_pad.notify_changes(retired);
								}
								sink_pad.notify_changes(changes);
								return false;
							}
						}
					}
					self.maybe_finish_locked(&mut control)
				};
				if let Some(retired) = retired {
					sink_pad.notify_changes(retired);
				}
				self.publish_finished(finished);
				gst::Pad::event_default(pad, Some(&*self.obj()), event)
			}
			// A pad that starts flushing stops counting towards EOS immediately, not when the flush
			// finishes. Waiting for FLUSH_STOP lets another pad's EOS complete the element inside the
			// flush window, and the finalize that follows is not something FLUSH_STOP can undo. The
			// first-catalog slot returns now for the same reason: a sibling resolving inside the window
			// would otherwise publish a first catalog without this pad.
			gst::EventView::FlushStart(_) => {
				let control = self.control.lock().unwrap();
				if let Some(state) = control.live.as_ref().filter(|state| state.is_open()) {
					let _rt = RUNTIME.enter();
					let mut lifecycle = sink_pad.lifecycle();
					if !lifecycle.releasing {
						lifecycle.ended = false;
						if let Some(catalog) = &state.catalog {
							lifecycle.media.reserve(catalog, state.wall.clone(), &state.names);
						}
					}
				}
				drop(control);
				gst::Pad::event_default(pad, Some(&*self.obj()), event)
			}
			// A new stream on this pad is a fresh start like a flush: it no longer counts as ended, and its
			// timeline is re-anchored. Without that the old stream's segment base stays current, and a new
			// stream restarting at zero reads as a rewind, which invalidates the pad and silently drops
			// its buffers. A pad its EOS left unresolved takes back its first-catalog slot, and an
			// importer that EOS finished is rebuilt from the sticky caps.
			gst::EventView::FlushStop(_) | gst::EventView::StreamStart(_) => {
				let changes = {
					let control = self.control.lock().unwrap();
					match control.live.as_ref().filter(|state| state.is_open()) {
						Some(state) => {
							let _rt = RUNTIME.enter();
							let mut lifecycle = sink_pad.lifecycle();
							match lifecycle.releasing {
								true => None,
								false => {
									lifecycle.ended = false;
									lifecycle.media.flush();
									let catalog = state.catalog.as_ref();
									if let Some(catalog) = catalog {
										lifecycle.media.reserve(catalog, state.wall.clone(), &state.names);
									}
									let caps = lifecycle.media.resumable_caps();
									let changes = caps.zip(catalog).and_then(|(caps, catalog)| {
										observe_caps(state, catalog, sink_pad, &mut lifecycle, &caps).0
									});
									changes.or_else(|| lifecycle.restore_bandwidth(state.session.bandwidth(), sink_pad))
								}
							}
						}
						None => None,
					}
				};
				if let Some(changes) = changes {
					sink_pad.notify_changes(changes);
				}
				gst::Pad::event_default(pad, Some(&*self.obj()), event)
			}
			_ => gst::Pad::event_default(pad, Some(&*self.obj()), event),
		}
	}

	fn owns_pad(&self, pad: &MoqSinkPad) -> bool {
		pad.parent().as_ref() == Some(self.obj().upcast_ref::<gst::Object>())
	}

	fn cancel_admission(&self, pad: &MoqSinkPad) {
		let finished = {
			let mut control = self.control.lock().unwrap();
			let _rt = RUNTIME.enter();
			if let Err(err) = pad.lifecycle().media.finalize() {
				gst::warning!(CAT, "finalize cancelled admission {}: {err:?}", pad.name());
			}
			self.finish_admission_locked(&mut control, pad)
		};
		self.publish_finished(finished);
		if self.owns_pad(pad) {
			self.release_pad(pad.upcast_ref());
		} else {
			pad.reset_detached();
		}
	}

	fn finish_admission_locked(&self, control: &mut Control, pad: &MoqSinkPad) -> Finished {
		control.admissions.remove(pad);
		self.maybe_finish_locked(control)
	}

	fn confirm_admission(&self, pad: &MoqSinkPad) -> (Admission, Finished) {
		let mut control = self.control.lock().unwrap();
		let completion = control.live.as_ref().map(|state| state.session.completion());
		let mut lifecycle = pad.lifecycle();
		// Decided here, holding both locks. `owns_pad` cannot answer it: a pad stays the element's
		// between being marked `releasing` and being removed, and a second look at the publication
		// would read a different instant than the one this confirmation ran in.
		let admission = match lifecycle.releasing {
			true => Admission::Rejected,
			false => {
				let failed = completion
					.as_ref()
					.is_some_and(|completion| completion.get() == Completion::Failed);
				lifecycle.completion = completion;
				match failed {
					true => Admission::Rejected,
					false => Admission::Accepted,
				}
			}
		};
		drop(lifecycle);
		let finished = self.finish_admission_locked(&mut control, pad);
		(admission, finished)
	}

	fn maybe_finish_locked(&self, control: &mut Control) -> Finished {
		if !control.admissions.is_empty() {
			return Finished::default();
		}
		let Some(state) = control.live.as_mut() else {
			return Finished::default();
		};
		let pads = self
			.obj()
			.sink_pads()
			.into_iter()
			.filter_map(|pad| pad.downcast::<MoqSinkPad>().ok())
			.collect::<Vec<_>>();
		let all_ended = !pads.is_empty() && pads.iter().all(|pad| pad.lifecycle().ended);
		if !all_ended || !state.is_open() {
			return Finished::default();
		}

		let _rt = RUNTIME.enter();
		let mut updates = Vec::new();
		let mut failure = None;
		let mut closed = Vec::new();
		for pad in pads {
			let mut lifecycle = pad.lifecycle();
			if lifecycle.releasing || !self.owns_pad(&pad) {
				continue;
			}
			let (track, result) = lifecycle.media.finish();
			closed.push(track);
			let changes = match result {
				Ok(_) => Some(lifecycle.end()),
				Err(err) => {
					gst::warning!(CAT, "finalize {}: {err:?}", pad.name());
					let reason = format!("{err:#}");
					if failure.is_none() {
						failure = Some(err);
					}
					Some(lifecycle.fail(reason))
				}
			};
			drop(lifecycle);
			if let Some(changes) = changes {
				updates.push(PadUpdate { pad, changes });
			}
		}
		state.wall.finish();
		let mut catalog = state.catalog.take();
		if let Some(catalog) = &mut catalog
			&& let Err(err) = catalog.finish().context("finalize catalog")
			&& failure.is_none()
		{
			failure = Some(err);
		}
		// Claimed after the work, not before: the outcome decides which terminal state this pass takes.
		// Losing the claim means the session task already ended the publication and posted its own
		// error, so this pass owes the bus nothing.
		let completion = state.session.completion();
		let message = match failure {
			Some(err) => match completion.fail() {
				true => Posted::Message {
					session: completion,
					kind: MessageKind::FinalizeError(format!("{err:?}")),
				},
				false => Posted::Nothing,
			},
			None if completion.finish() => {
				// The closed renditions and the catalog hold the finished tracks, so they go only
				// once the broadcast closes.
				let finished =
					moq_mux::Linger::new([state.broadcast.broadcast().clone()], (closed, catalog), state.linger);
				self.linger_locked(control, finished)
			}
			None => Posted::Nothing,
		};
		Finished { updates, message }
	}

	/// Serve the finished broadcast for its linger so a subscriber that arrives after the end still
	/// gets the tail, and owe the EOS once it closes. With no linger, close it and owe the EOS now.
	///
	/// The EOS waits for the linger because `gst-launch` exits on it, and NULL closes the broadcast.
	/// Stopping the pipeline is the interrupt: it aborts the linger, which closes the broadcast.
	fn linger_locked<T: Send + 'static>(&self, control: &mut Control, finished: moq_mux::Linger<T>) -> Posted {
		let state = control.live.as_mut().expect("a finished publication is live");
		let linger = state.linger;
		if linger.is_zero() {
			drop(finished);
			state.drained = true;
			return claim_eos(control);
		}
		gst::info!(
			CAT,
			obj = self.obj(),
			"all pads ended, serving the broadcast for {linger:?}"
		);
		let task = RUNTIME.spawn(linger_task(
			finished,
			self.obj().downgrade(),
			state.session.completion(),
		));
		state.lingering = Some(task.abort_handle());
		Posted::Nothing
	}

	/// Owe the EOS now that the finished broadcast closed, unless a newer run replaced `session`.
	fn drained(&self, session: &CompletionHandle) {
		let message = {
			let mut control = self.control.lock().unwrap();
			match control.live.as_mut() {
				Some(state) if std::sync::Arc::ptr_eq(&state.session.completion(), session) => state.drained = true,
				_ => return,
			}
			claim_eos(&mut control)
		};
		self.post_current(message);
	}

	fn notify_updates(&self, updates: Vec<PadUpdate>) {
		for update in updates {
			update.pad.notify_changes(update.changes);
		}
	}

	/// Open the EOS gate for this run and post the message if the publication already ended.
	fn enter_playing(&self) {
		let message = {
			let mut control = self.control.lock().unwrap();
			control.playing = true;
			claim_eos(&mut control)
		};
		self.publish_finished(Finished {
			updates: Vec::new(),
			message,
		});
	}

	/// Close the gate: an EOS earned from here on waits for the next entry to PLAYING.
	fn leave_playing(&self) {
		let mut control = self.control.lock().unwrap();
		if !std::mem::replace(&mut control.playing, false) {
			return;
		}
		// Running time stops in PAUSED while the encoder handoff clock keeps advancing.
		for pad in self.obj().sink_pads() {
			let pad = pad.downcast::<MoqSinkPad>().expect("sink pad type");
			pad.lifecycle().media.discontinuity();
		}
	}

	fn publish_finished(&self, finished: Finished) {
		self.notify_updates(finished.updates);
		self.post_current(finished.message);
	}

	/// Route a terminal reconnect error through the same session gate as finalization messages.
	pub(super) fn post_session_error(&self, session: &CompletionHandle, error: String) {
		let updates = {
			let control = self.control.lock().unwrap();
			let current = control
				.live
				.as_ref()
				.is_some_and(|state| std::sync::Arc::ptr_eq(&state.session.completion(), session));
			if !current {
				Vec::new()
			} else {
				self.obj()
					.sink_pads()
					.into_iter()
					.filter_map(|pad| pad.downcast::<MoqSinkPad>().ok())
					.filter_map(|pad| {
						let mut lifecycle = pad.lifecycle();
						let belongs = lifecycle
							.completion
							.as_ref()
							.is_some_and(|completion| std::sync::Arc::ptr_eq(completion, session));
						(!lifecycle.releasing && belongs).then(|| PadUpdate {
							pad: pad.clone(),
							changes: lifecycle.fail(format!("session failed: {error}")),
						})
					})
					.collect()
			}
		};
		self.publish_finished(Finished {
			updates,
			message: Posted::Message {
				session: session.clone(),
				kind: MessageKind::SessionError(error),
			},
		});
	}

	/// Post a message only while the publishing session that produced it remains current. The control
	/// lock is released first because bus sync handlers can re-enter element and pad properties.
	fn post_current(&self, message: Posted) {
		let Posted::Message { session, kind } = message else {
			return;
		};
		let current = self
			.control
			.lock()
			.unwrap()
			.live
			.as_ref()
			.is_some_and(|state| std::sync::Arc::ptr_eq(&state.session.completion(), &session));
		if !current {
			gst::debug!(
				CAT,
				obj = self.obj(),
				"discarding a message from a stopped publishing session"
			);
			return;
		}

		match kind {
			MessageKind::Eos => {
				gst::info!(CAT, "the broadcast ended, posting EOS");
				let obj = self.obj();
				let _ = obj.post_message(gst::message::Eos::builder().src(&*obj).build());
			}
			MessageKind::FinalizeError(err) => {
				gst::element_error!(self.obj(), gst::CoreError::Failed, ("finalize failed"), ["{err}"]);
			}
			MessageKind::SessionError(err) => {
				gst::element_error!(self.obj(), gst::CoreError::Failed, ("session error"), ["{err}"]);
			}
		}
	}
}

/// Serve a finished broadcast for its linger, then release the EOS its publication owes.
async fn linger_task<T>(
	finished: moq_mux::Linger<T>,
	element: glib::WeakRef<super::MoqSink>,
	session: CompletionHandle,
) {
	finished.wait().await;
	if let Some(element) = element.upgrade() {
		element.imp().drained(&session);
	}
}

/// Build a pad's producer for `caps` in the open publication, returning its status changes and whether
/// the caps were accepted.
fn observe_caps(
	state: &State,
	catalog: &moq_mux::catalog::Producer,
	pad: &MoqSinkPad,
	lifecycle: &mut PadLifecycle,
	caps: &gst::Caps,
) -> (Option<Notifications>, bool) {
	lifecycle.media.reserve(catalog, state.wall.clone(), &state.names);
	let requested = lifecycle.requested().map(str::to_owned);
	let mut options = ProducerOptions::new(caps).with_container(lifecycle.container().into());
	if let Some(track) = requested.as_deref() {
		options = options.with_track(track);
	}
	let outcome = lifecycle.media.observe_caps(&state.broadcast, catalog, options);
	lifecycle.completion = Some(state.session.completion());
	match outcome {
		CapsOutcome::Active(track) => (Some(lifecycle.activate(track, state.session.bandwidth(), pad)), true),
		CapsOutcome::Failed(reason) => (Some(lifecycle.fail(reason)), false),
		CapsOutcome::Unchanged => (None, true),
	}
}

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use super::super::MediaContainer;
	use super::super::request_pad::Status;
	use super::super::session::CompletionState;
	use super::*;

	/// A sink that closes its broadcast at the end of stream, for tests not about the linger.
	fn sink() -> super::super::MoqSink {
		glib::Object::builder::<super::super::MoqSink>()
			.property("linger", 0u64)
			.build()
	}

	struct CatalogRun(super::super::MoqSink);

	impl CatalogRun {
		fn new() -> Self {
			gst::init().unwrap();
			let sink = sink();
			sink.set_property("url", "tcp://127.0.0.1:1");
			sink.set_property("broadcast", "catalog-test");
			Self(sink)
		}

		fn pad(&self, name: &str) -> gst::Pad {
			let pad = self.0.request_pad_simple("sink_%u").unwrap();
			pad.set_property("track", name);
			pad
		}

		fn start(&self) -> moq_mux::catalog::Consumer {
			let consumer = self.start_unplaced();
			let control = self.0.imp().control.lock().unwrap();
			control
				.live
				.as_ref()
				.unwrap()
				.wall
				.place(
					&gst::Buffer::new(),
					Duration::ZERO,
					super::super::wall::Sample {
						running: Some(gst::ClockTime::ZERO),
						wall: moq_clock::Wall::UNIX_EPOCH + Duration::from_secs(1_735_689_600),
					},
				)
				.unwrap();
			consumer
		}

		fn start_unplaced(&self) -> moq_mux::catalog::Consumer {
			self.0.set_state(gst::State::Paused).unwrap();
			let broadcast = self
				.0
				.imp()
				.control
				.lock()
				.unwrap()
				.live
				.as_ref()
				.unwrap()
				.broadcast
				.consume();
			futures::executor::block_on(moq_mux::catalog::Consumer::new(
				&broadcast,
				moq_mux::catalog::CatalogFormat::Hang,
			))
			.unwrap()
		}
	}

	impl Drop for CatalogRun {
		fn drop(&mut self) {
			self.0.set_state(gst::State::Null).unwrap();
		}
	}

	fn negotiate(pad: &gst::Pad, caps: &gst::Caps) -> bool {
		assert!(pad.send_event(gst::event::StreamStart::new(pad.name().as_str())));
		pad.send_event(gst::event::Caps::new(caps))
	}

	fn opus_caps() -> gst::Caps {
		gst::Caps::builder("audio/x-opus")
			.field("rate", 48_000i32)
			.field("channels", 2i32)
			.build()
	}

	fn h264_caps() -> gst::Caps {
		gst::Caps::builder("video/x-h264")
			.field("stream-format", "byte-stream")
			.field("alignment", "au")
			.build()
	}

	fn video_frame(pad: &gst::Pad) {
		assert!(pad.send_event(gst::event::Segment::new(&gst::FormattedSegment::<gst::ClockTime>::new())));
		let mut buffer = gst::Buffer::from_slice(super::super::pad::h264_keyframe_au());
		buffer.get_mut().unwrap().set_pts(gst::ClockTime::ZERO);
		assert_eq!(pad.chain(buffer), Ok(gst::FlowSuccess::Ok));
	}

	fn next_catalog(
		catalog: &mut moq_mux::catalog::Consumer,
	) -> std::task::Poll<moq_mux::Result<Option<moq_mux::catalog::hang::Catalog>>> {
		use moq_mux::catalog::Stream;
		catalog.poll_next(&moq_net::kio::Waiter::noop())
	}

	#[test]
	fn reference_metadata_places_the_broadcast_wall_clock() {
		let run = CatalogRun::new();
		let pad = run.pad("audio");
		let mut catalog = run.start_unplaced();
		assert!(negotiate(&pad, &opus_caps()));
		assert!(
			next_catalog(&mut catalog).is_pending(),
			"CAPS must wait for the wall clock"
		);
		assert!(pad.send_event(gst::event::Segment::new(&gst::FormattedSegment::<gst::ClockTime>::new())));
		let mut buffer = gst::Buffer::from_slice([0xf8, 0xff, 0xfe]);
		let buffer_mut = buffer.get_mut().unwrap();
		buffer_mut.set_pts(gst::ClockTime::from_seconds(3));
		gst::ReferenceTimestampMeta::add(
			buffer_mut,
			&gst::Caps::builder("timestamp/x-unix").build(),
			gst::ClockTime::from_seconds(1_735_689_603),
			None,
		);
		assert_eq!(pad.chain(buffer), Ok(gst::FlowSuccess::Ok));
		let std::task::Poll::Ready(Ok(Some(first))) = next_catalog(&mut catalog) else {
			panic!("first catalog")
		};
		assert_eq!(
			first.clock.unwrap().wall.as_micros(),
			((1_735_689_600 - hang::catalog::MOQ_EPOCH_UNIX_MILLIS / 1000) * 1_000_000) as u128,
		);
	}

	#[test]
	fn pads_keep_one_wall_clock_and_verbatim_running_times_each_run() {
		let run = CatalogRun::new();
		let video = run.pad("video");
		let audio = run.pad("audio");
		for zero in [1_735_689_600u64, 1_735_689_660] {
			let _catalog = run.start_unplaced();
			assert!(negotiate(&video, &h264_caps()));
			assert!(negotiate(&audio, &opus_caps()));
			for (pad, origin, pts, payload, reference) in [
				(&video, 10, 13, super::super::pad::h264_keyframe_au(), zero + 3),
				(&audio, 100, 105, Bytes::from_static(&[0xf8, 0xff, 0xfe]), zero + 500),
			] {
				let mut segment = gst::FormattedSegment::<gst::ClockTime>::new();
				segment.set_start(gst::ClockTime::from_seconds(origin));
				assert!(pad.send_event(gst::event::Segment::new(&segment)));
				let mut buffer = gst::Buffer::from_slice(payload);
				let b = buffer.get_mut().unwrap();
				b.set_pts(gst::ClockTime::from_seconds(pts));
				gst::ReferenceTimestampMeta::add(
					b,
					&gst::Caps::builder("timestamp/x-unix").build(),
					gst::ClockTime::from_seconds(reference),
					None,
				);
				assert_eq!(pad.chain(buffer), Ok(gst::FlowSuccess::Ok));
			}
			let (broadcast, clock) = {
				let control = run.0.imp().control.lock().unwrap();
				let state = control.live.as_ref().unwrap();
				(
					state.broadcast.consume(),
					state.catalog.as_ref().unwrap().snapshot().clock.unwrap(),
				)
			};
			assert_eq!(
				clock.wall.as_micros(),
				((zero - hang::catalog::MOQ_EPOCH_UNIX_MILLIS / 1000) * 1_000_000) as u128
			);
			for (name, kind, expected) in [
				("video", moq_mux::container::Kind::Video, 3),
				("audio", moq_mux::container::Kind::Audio, 5),
			] {
				let subscriber = futures::executor::block_on(broadcast.track(name).unwrap().subscribe(None)).unwrap();
				let mut consumer =
					moq_mux::container::Consumer::new(subscriber, moq_mux::catalog::hang::Container::Legacy(kind));
				let std::task::Poll::Ready(Ok(Some(read))) = consumer.poll_read(&moq_net::kio::Waiter::noop()) else {
					panic!("published media frame")
				};
				let moq_mux::container::Read::Frame { frame, .. } = read else {
					panic!("media frame")
				};
				assert_eq!(frame.timestamp.as_micros(), expected * 1_000_000, "{name}");
			}
			run.0.set_state(gst::State::Null).unwrap();
		}
	}

	#[test]
	fn first_catalog_waits_for_late_audio_caps_each_run() {
		let run = CatalogRun::new();
		let video = run.pad("video");
		let audio = run.pad("audio");
		for _ in 0..2 {
			let mut catalog = run.start();
			assert!(negotiate(&video, &h264_caps()));
			video_frame(&video);
			assert!(
				next_catalog(&mut catalog).is_pending(),
				"video alone must not publish the first catalog"
			);
			assert!(negotiate(&audio, &opus_caps()));
			let std::task::Poll::Ready(Ok(Some(first))) = next_catalog(&mut catalog) else {
				panic!("complete first catalog")
			};
			assert!(first.video.renditions.contains_key("video"));
			assert!(first.audio.renditions.contains_key("audio"));
			run.0.set_state(gst::State::Ready).unwrap();
		}
	}

	#[test]
	fn unresolved_terminal_pad_releases_first_catalog_with_streaming_sibling() {
		for terminal in [
			"release",
			"eos",
			"opaque",
			"failed",
			"duplicate",
			"unsupported",
			"h264-eos",
			"bitstream",
		] {
			let run = CatalogRun::new();
			let audio = run.pad("audio");
			let pending = run.pad("pending");
			let mut catalog = run.start();
			assert!(negotiate(&audio, &opus_caps()));
			assert!(
				next_catalog(&mut catalog).is_pending(),
				"{terminal} pad still owes its config"
			);
			match terminal {
				"release" => run.0.release_request_pad(&pending),
				"eos" => {
					assert!(pending.send_event(gst::event::Eos::new()));
				}
				"opaque" => {
					assert!(negotiate(
						&pending,
						&gst::Caps::new_empty_simple("application/octet-stream")
					));
				}
				"failed" => {
					let caps = gst::Caps::builder("audio/mpeg")
						.field("mpegversion", 4i32)
						.field("stream-format", "raw")
						.build();
					assert!(!negotiate(&pending, &caps));
				}
				"duplicate" => {
					pending.set_property("track", "audio");
					assert!(!negotiate(&pending, &opus_caps()));
				}
				"unsupported" => {
					assert!(!run.0.imp().handle_event(
						&pending,
						gst::event::Caps::new(&gst::Caps::new_empty_simple("video/x-raw"))
					));
				}
				"h264-eos" => {
					assert!(negotiate(&pending, &h264_caps()));
					assert!(pending.send_event(gst::event::Eos::new()));
				}
				"bitstream" => {
					assert!(negotiate(&pending, &h264_caps()));
					assert!(
						pending.send_event(gst::event::Segment::new(&gst::FormattedSegment::<gst::ClockTime>::new()))
					);
					let mut buffer = gst::Buffer::from_slice([0, 0, 0, 1, 0x67, 0, 0, 0, 1, 0x65]);
					buffer.get_mut().unwrap().set_pts(gst::ClockTime::ZERO);
					assert_eq!(pending.chain(buffer), Ok(gst::FlowSuccess::Ok));
				}
				_ => unreachable!(),
			}
			let std::task::Poll::Ready(Ok(Some(first))) = next_catalog(&mut catalog) else {
				panic!("{terminal} kept the first catalog gated")
			};
			assert!(first.audio.renditions.contains_key("audio"));
			assert!(first.video.renditions.is_empty());
			assert!(
				completion_of(&run.0).is_open(),
				"sibling keeps streaming after {terminal}"
			);
		}
	}

	#[test]
	fn admitted_pad_is_reserved_before_it_is_visible_and_only_once() {
		for visible in [false, true] {
			let run = CatalogRun::new();
			let audio = run.pad("audio");
			let pending = run.pad("video").downcast::<MoqSinkPad>().unwrap();
			if !visible {
				run.0.remove_pad(&pending).unwrap();
			}
			run.0.imp().control.lock().unwrap().admissions.insert(pending.clone());
			let mut catalog = run.start();
			assert!(negotiate(&audio, &opus_caps()));
			assert!(
				next_catalog(&mut catalog).is_pending(),
				"admitted pad was missed (visible={visible})"
			);
			if !visible {
				run.0.add_pad(&pending).unwrap();
				pending.set_active(true).unwrap();
			}
			// CAPS can be reentrant from child_added, before confirm_admission.
			assert!(negotiate(pending.upcast_ref(), &h264_caps()));
			video_frame(pending.upcast_ref());
			let (admission, finished) = run.0.imp().confirm_admission(&pending);
			run.0.imp().publish_finished(finished);
			assert!(matches!(admission, Admission::Accepted));
			let std::task::Poll::Ready(Ok(Some(first))) = next_catalog(&mut catalog) else {
				panic!("a duplicate reservation kept the first catalog gated")
			};
			assert!(first.audio.renditions.contains_key("audio"));
			assert!(first.video.renditions.contains_key("video"));
			assert!(next_catalog(&mut catalog).is_pending(), "one complete first catalog");
		}
	}

	#[test]
	fn cancelling_an_unconfirmed_pad_releases_its_slot() {
		let run = CatalogRun::new();
		let audio = run.pad("audio");
		let pending = run.pad("pending").downcast::<MoqSinkPad>().unwrap();
		run.0.remove_pad(&pending).unwrap();
		run.0.imp().control.lock().unwrap().admissions.insert(pending.clone());
		let mut catalog = run.start();
		assert!(negotiate(&audio, &opus_caps()));
		assert!(next_catalog(&mut catalog).is_pending());
		run.0.imp().cancel_admission(&pending);
		assert!(matches!(
			next_catalog(&mut catalog),
			std::task::Poll::Ready(Ok(Some(_)))
		));
		assert!(run.0.imp().control.lock().unwrap().admissions.is_empty());
	}

	#[test]
	fn resolved_eos_waits_with_its_rendition_while_a_sibling_is_unresolved() {
		let run = CatalogRun::new();
		let audio = run.pad("audio");
		let pending = run.pad("pending");
		let mut catalog = run.start();
		assert!(negotiate(&audio, &opus_caps()));
		assert!(audio.send_event(gst::event::Eos::new()));
		assert!(next_catalog(&mut catalog).is_pending());
		assert!(negotiate(&pending, &h264_caps()));
		video_frame(&pending);
		let std::task::Poll::Ready(Ok(Some(first))) = next_catalog(&mut catalog) else {
			panic!("both resolved renditions")
		};
		assert!(
			first.audio.renditions.contains_key("audio"),
			"resolved EOS must keep its rendition"
		);
		assert!(first.video.renditions.contains_key("pending"));
		assert!(completion_of(&run.0).is_open());
	}

	#[test]
	fn individual_eos_releases_and_resume_restores_its_bandwidth() {
		let run = CatalogRun::new();
		let first = run.pad("first");
		let second = run.pad("second");
		first.set_property("bandwidth-ceiling", 4_000_000u64);
		second.set_property("bandwidth-ceiling", 4_000_000u64);
		let _catalog = run.start();

		let estimate = moq_net::bandwidth::Producer::new();
		estimate
			.set(Some(moq_net::bandwidth::Rate::from_bps(4_000_000)))
			.unwrap();
		run.0
			.imp()
			.control
			.lock()
			.unwrap()
			.live
			.as_mut()
			.unwrap()
			.session
			.replace_bandwidth(moq_net::bandwidth::Allocator::new(estimate.consume()));

		assert!(negotiate(&first, &h264_caps()));
		assert!(negotiate(&second, &h264_caps()));
		let broadcast = run
			.0
			.imp()
			.control
			.lock()
			.unwrap()
			.live
			.as_ref()
			.unwrap()
			.broadcast
			.consume();
		let _first_reader = broadcast.track("first").unwrap().subscribe(None);
		let _second_reader = broadcast.track("second").unwrap().subscribe(None);
		video_frame(&first);
		video_frame(&second);
		let grant = |pad: &gst::Pad| {
			pad.property::<Option<gst::Structure>>("track-demand")
				.expect("active demand")
				.get::<u64>("grant-bps")
				.expect("available grant")
		};

		assert_eq!(grant(&first), 2_000_000);
		assert_eq!(grant(&second), 2_000_000);
		assert!(first.send_event(gst::event::Eos::new()));
		assert!(first.property::<Option<gst::Structure>>("track-demand").is_none());
		assert_eq!(
			grant(&second),
			4_000_000,
			"the live sibling receives the released share"
		);

		assert!(first.send_event(gst::event::StreamStart::new("first-resumed")));
		video_frame(&first);
		let resumed = first
			.property::<Option<gst::Structure>>("track-demand")
			.expect("the resumed track restores demand reporting");
		assert_eq!(
			resumed.get::<bool>("wanted"),
			Ok(true),
			"the retained producer keeps its subscriber across EOS and resume"
		);
		assert_eq!(
			resumed.get::<u64>("grant-bps"),
			Ok(2_000_000),
			"the retained track reserves again when it resumes"
		);
		assert_eq!(
			grant(&second),
			2_000_000,
			"the resumed watched track again shares the estimate with its sibling"
		);
	}

	#[test]
	fn codec_renegotiation_rebinds_bandwidth_to_the_new_track() {
		let run = CatalogRun::new();
		let video = run.pad("camera");
		video.set_property("bandwidth-ceiling", 4_000_000u64);
		let _catalog = run.start();

		let estimate = moq_net::bandwidth::Producer::new();
		estimate
			.set(Some(moq_net::bandwidth::Rate::from_bps(4_000_000)))
			.unwrap();
		run.0
			.imp()
			.control
			.lock()
			.unwrap()
			.live
			.as_mut()
			.unwrap()
			.session
			.replace_bandwidth(moq_net::bandwidth::Allocator::new(estimate.consume()));

		assert!(negotiate(&video, &h264_caps()));
		let broadcast = run
			.0
			.imp()
			.control
			.lock()
			.unwrap()
			.live
			.as_ref()
			.unwrap()
			.broadcast
			.consume();
		let _old_reader = broadcast.track("camera").unwrap().subscribe(None);
		assert_eq!(
			video
				.property::<Option<gst::Structure>>("track-demand")
				.unwrap()
				.get::<bool>("wanted"),
			Ok(true)
		);

		let vp8 = gst::Caps::builder("video/x-vp8").build();
		assert!(video.send_event(gst::event::Caps::new(&vp8)));
		let _new_reader = broadcast.track("camera/1").unwrap().subscribe(None);
		let rebound = video
			.property::<Option<gst::Structure>>("track-demand")
			.expect("renegotiation keeps demand reporting active");
		assert_eq!(rebound.get::<bool>("wanted"), Ok(true));
		assert_eq!(rebound.get::<u64>("grant-bps"), Ok(4_000_000));
	}

	#[test]
	fn terminal_session_failure_retires_active_pad_bandwidth() {
		let run = CatalogRun::new();
		let first = run.pad("first");
		let second = run.pad("second");
		first.set_property("bandwidth-ceiling", 4_000_000u64);
		second.set_property("bandwidth-ceiling", 4_000_000u64);
		let _catalog = run.start();

		let estimate = moq_net::bandwidth::Producer::new();
		estimate
			.set(Some(moq_net::bandwidth::Rate::from_bps(4_000_000)))
			.unwrap();
		let completion = {
			let mut control = run.0.imp().control.lock().unwrap();
			let state = control.live.as_mut().unwrap();
			state
				.session
				.replace_bandwidth(moq_net::bandwidth::Allocator::new(estimate.consume()));
			state.session.completion()
		};

		assert!(negotiate(&first, &h264_caps()));
		assert!(negotiate(&second, &h264_caps()));
		let broadcast = run
			.0
			.imp()
			.control
			.lock()
			.unwrap()
			.live
			.as_ref()
			.unwrap()
			.broadcast
			.consume();
		let _first_reader = broadcast.track("first").unwrap().subscribe(None);
		let _second_reader = broadcast.track("second").unwrap().subscribe(None);
		assert!(first.property::<Option<gst::Structure>>("track-demand").is_some());
		assert!(second.property::<Option<gst::Structure>>("track-demand").is_some());

		assert!(completion.fail());
		run.0
			.imp()
			.post_session_error(&completion, "authentication refused".to_string());

		for pad in [&first, &second] {
			assert!(pad.property::<Option<gst::Structure>>("track-demand").is_none());
			assert_eq!(pad.property::<Status>("track-status"), Status::Error);
			assert_eq!(
				pad.property::<Option<String>>("track-error").as_deref(),
				Some("session failed: authentication refused")
			);
		}
	}

	// GStreamer keeps CAPS sticky across a flush, so a resumed pad gets no new CAPS event. EOS ended its
	// track, so the resumed one takes a fresh name.
	#[test]
	fn an_unresolved_pad_resumed_after_eos_publishes_without_new_caps() {
		use futures::FutureExt;
		for resume in ["flush", "stream-start"] {
			let run = CatalogRun::new();
			let audio = run.pad("audio");
			let video = run.pad("video");
			let mut catalog = run.start();
			assert!(negotiate(&audio, &opus_caps()));
			assert!(negotiate(&video, &h264_caps()));
			assert!(video.send_event(gst::event::Eos::new()));
			let std::task::Poll::Ready(Ok(Some(first))) = next_catalog(&mut catalog) else {
				panic!("the ended pad must release the first catalog ({resume})")
			};
			assert!(first.video.renditions.is_empty());

			match resume {
				"flush" => {
					assert!(video.send_event(gst::event::FlushStart::new()));
					assert!(video.send_event(gst::event::FlushStop::new(true)));
				}
				"stream-start" => assert!(video.send_event(gst::event::StreamStart::new("second"))),
				_ => unreachable!(),
			}
			video_frame(&video);

			let std::task::Poll::Ready(Ok(Some(next))) = next_catalog(&mut catalog) else {
				panic!("the resumed pad must publish its rendition ({resume})")
			};
			assert!(next.video.renditions.contains_key("video/1"), "{resume}");
			let broadcast = run
				.0
				.imp()
				.control
				.lock()
				.unwrap()
				.live
				.as_ref()
				.unwrap()
				.broadcast
				.consume();
			let mut track = broadcast
				.track("video/1")
				.expect("the resumed track is published")
				.subscribe(None)
				.now_or_never()
				.expect("subscribed without waiting")
				.expect("subscribe to the resumed track")
				.ordered();
			let mut group = track
				.next_group()
				.now_or_never()
				.expect("a group is ready")
				.unwrap()
				.expect("the resumed keyframe opens a group");
			let frame = group
				.read_frame()
				.now_or_never()
				.expect("a frame is ready")
				.unwrap()
				.expect("the resumed keyframe is published");
			assert!(!frame.payload.is_empty(), "{resume}");
		}
	}

	#[test]
	fn a_pad_resumed_before_its_caps_gates_the_first_catalog_again() {
		for resume in ["flush", "stream-start"] {
			let run = CatalogRun::new();
			let audio = run.pad("audio");
			let video = run.pad("video");
			let mut catalog = run.start();
			assert!(video.send_event(gst::event::Eos::new()));
			// The sibling resolves inside the flush window, where the pad already counts as live.
			match resume {
				"flush" => {
					assert!(video.send_event(gst::event::FlushStart::new()));
					assert!(negotiate(&audio, &opus_caps()));
					assert!(video.send_event(gst::event::FlushStop::new(true)));
				}
				"stream-start" => {
					assert!(video.send_event(gst::event::StreamStart::new("second")));
					assert!(negotiate(&audio, &opus_caps()));
				}
				_ => unreachable!(),
			}
			assert!(
				next_catalog(&mut catalog).is_pending(),
				"the resumed pad still owes its rendition ({resume})"
			);
			assert!(negotiate(&video, &h264_caps()));
			video_frame(&video);
			let std::task::Poll::Ready(Ok(Some(first))) = next_catalog(&mut catalog) else {
				panic!("the resumed pad must resolve the first catalog ({resume})")
			};
			assert!(first.audio.renditions.contains_key("audio"), "{resume}");
			assert!(first.video.renditions.contains_key("video"), "{resume}");
		}
	}

	#[test]
	fn a_midrun_pending_pad_does_not_gate_later_catalogs() {
		let run = CatalogRun::new();
		let audio = run.pad("audio");
		let mut catalog = run.start();
		assert!(negotiate(&audio, &opus_caps()));
		assert!(matches!(
			next_catalog(&mut catalog),
			std::task::Poll::Ready(Ok(Some(_)))
		));
		let _pending = run.pad("pending");
		let text = run.pad("text");
		assert!(negotiate(
			&text,
			&gst::Caps::builder("text/x-raw").field("format", "utf8").build()
		));
		let std::task::Poll::Ready(Ok(Some(next))) = next_catalog(&mut catalog) else {
			panic!("later catalogs must not wait")
		};
		assert!(next.text.renditions.contains_key("text"));
	}

	#[test]
	fn releasing_a_pad_during_a_live_run_still_retires_its_rendition() {
		let run = CatalogRun::new();
		let audio = run.pad("audio");
		let text = run.pad("text");
		let mut catalog = run.start();
		assert!(negotiate(&audio, &opus_caps()));
		assert!(negotiate(
			&text,
			&gst::Caps::builder("text/x-raw").field("format", "utf8").build()
		));
		assert!(matches!(
			next_catalog(&mut catalog),
			std::task::Poll::Ready(Ok(Some(_)))
		));
		run.0.release_request_pad(&text);
		let std::task::Poll::Ready(Ok(Some(next))) = next_catalog(&mut catalog) else {
			panic!("live release must publish the removal")
		};
		assert!(next.text.renditions.is_empty());
		assert!(next.audio.renditions.contains_key("audio"));
	}

	fn spec(element: &super::super::MoqSink, name: &str) -> glib::ParamSpec {
		element.find_property(name).unwrap()
	}

	/// The live publication's completion, for a test that needs to race it.
	fn completion_of(sink: &super::super::MoqSink) -> super::super::session::CompletionHandle {
		sink.imp()
			.control
			.lock()
			.unwrap()
			.live
			.as_ref()
			.expect("live session")
			.session
			.completion()
	}

	// A pad marked for release is still the element's until it is removed, so the confirmation has to
	// say it refused. Inferring it from `owns_pad` would hand the caller a pad already on its way out.
	#[test]
	fn confirming_an_admission_refuses_a_pad_already_releasing() {
		gst::init().unwrap();
		let sink = sink();
		sink.set_property("url", "https://127.0.0.1:1");
		sink.set_property("broadcast", "test");
		let element = sink.clone().upcast::<gst::Element>();
		let pad = element.request_pad_simple("sink_0").expect("request sink_0");
		element.set_state(gst::State::Paused).expect("start session");
		let pad = pad.downcast::<MoqSinkPad>().expect("MoqSinkPad");

		pad.lifecycle().releasing = true;
		sink.imp().control.lock().unwrap().admissions.insert(pad.clone());
		let (admission, finished) = sink.imp().confirm_admission(&pad);
		sink.imp().publish_finished(finished);

		assert!(matches!(admission, Admission::Rejected), "the confirmation refused it");
		assert!(
			sink.imp().owns_pad(&pad),
			"and it did so while the pad still belonged to the element, which is the window \
			 `owns_pad` cannot see"
		);
		pad.lifecycle().releasing = false;
		let _ = element.set_state(gst::State::Null);
	}

	// The transition is a compare-exchange, not a store: whoever arrives second changes nothing and is
	// told so. A store would let a late session error rewrite an EOS the element already earned.
	#[test]
	fn only_the_first_terminal_transition_wins() {
		let completion = CompletionState::new();
		assert!(completion.finish(), "the first transition wins");
		assert!(!completion.fail(), "the second is refused");
		assert_eq!(completion.get(), Completion::Eos);
		assert!(!completion.is_open());

		let completion = CompletionState::new();
		assert!(completion.fail());
		assert!(!completion.finish(), "a clean end cannot follow a failure");
		assert_eq!(completion.get(), Completion::Failed);
	}

	// A pad admitted while the publication was open must not be handed back once it failed underneath:
	// it would only refuse every buffer. A clean EOS is the opposite case, covered by the pad-added
	// tests that let a pad end the publication and still read its own status.
	#[test]
	fn a_pad_is_withdrawn_when_the_publication_fails_while_it_is_admitted() {
		gst::init().unwrap();
		let sink = sink();
		sink.set_property("url", "https://127.0.0.1:1");
		sink.set_property("broadcast", "test");
		let element = sink.clone().upcast::<gst::Element>();
		element.set_state(gst::State::Paused).expect("start session");

		let failing = sink.clone();
		element.connect_pad_added(move |_, _| {
			completion_of(&failing).fail();
		});

		assert!(
			element.request_pad_simple("sink_0").is_none(),
			"the pad is withdrawn instead of returned unusable"
		);
		assert_eq!(element.num_sink_pads(), 0, "and it is not left on the element");
		let _ = element.set_state(gst::State::Null);
	}

	// A task outliving its session writes into the handle nobody reads any more. Scoping falls out of
	// the topology: no id comparison guards the state the pads see.
	#[test]
	fn a_stale_completion_cannot_end_the_next_publication() {
		gst::init().unwrap();
		let sink = sink();
		sink.set_property("url", "https://127.0.0.1:1");
		sink.set_property("broadcast", "test");
		let element = sink.clone().upcast::<gst::Element>();
		let pad = element.request_pad_simple("sink_0").expect("request sink_0");

		element.set_state(gst::State::Paused).expect("first session");
		let stale = completion_of(&sink);
		element.set_state(gst::State::Ready).expect("stop the first session");
		element.set_state(gst::State::Paused).expect("second session");

		assert!(stale.fail(), "the old task still owns its own handle");
		assert!(
			completion_of(&sink).is_open(),
			"the publication the pads read is untouched"
		);
		let pad = pad.downcast::<MoqSinkPad>().expect("MoqSinkPad");
		let linked = pad.lifecycle().completion.clone().expect("session link");
		assert!(!Arc::ptr_eq(&linked, &stale), "the pad was relinked to the new session");
		assert!(linked.is_open());
		let _ = element.set_state(gst::State::Null);
	}

	#[test]
	fn an_admission_defers_finalization_until_membership_is_stable() {
		gst::init().unwrap();
		let sink = sink();
		sink.set_property("url", "https://127.0.0.1:1");
		sink.set_property("broadcast", "test");
		let element = sink.clone().upcast::<gst::Element>();
		let pad = element.request_pad_simple("sink_0").expect("request sink_0");
		element.set_state(gst::State::Paused).expect("start session");
		assert!(pad.send_event(gst::event::StreamStart::new("test")));
		let caps = gst::Caps::builder("video/x-h264")
			.field("stream-format", "byte-stream")
			.field("alignment", "au")
			.build();
		assert!(pad.send_event(gst::event::Caps::new(&caps)));

		sink.imp()
			.control
			.lock()
			.unwrap()
			.admissions
			.insert(pad.clone().downcast::<MoqSinkPad>().unwrap());
		assert!(pad.send_event(gst::event::Eos::new()));
		let pad = pad.downcast::<MoqSinkPad>().expect("MoqSinkPad");
		assert_eq!(pad.property::<Status>("track-status"), Status::Active);

		let finished = sink
			.imp()
			.finish_admission_locked(&mut sink.imp().control.lock().unwrap(), &pad);
		sink.imp().publish_finished(finished);
		assert_eq!(pad.property::<Status>("track-status"), Status::Ended);
		let _ = element.set_state(gst::State::Null);
	}

	#[test]
	fn confirming_an_admission_replaces_and_clears_stale_session_links() {
		gst::init().unwrap();
		let sink = sink();
		sink.set_property("url", "https://127.0.0.1:1");
		sink.set_property("broadcast", "test");
		let element = sink.clone().upcast::<gst::Element>();
		let pad = element
			.request_pad_simple("sink_0")
			.expect("request sink_0")
			.downcast::<MoqSinkPad>()
			.expect("MoqSinkPad");
		element.set_state(gst::State::Paused).expect("start session");

		let expected = sink
			.imp()
			.control
			.lock()
			.unwrap()
			.live
			.as_ref()
			.expect("live session")
			.session
			.completion();
		let stale = CompletionState::new();
		stale.fail();
		pad.lifecycle().completion = Some(stale.clone());
		sink.imp().control.lock().unwrap().admissions.insert(pad.clone());
		let (_, finished) = sink.imp().confirm_admission(&pad);
		sink.imp().publish_finished(finished);

		let actual = pad.lifecycle().completion.clone().expect("session link");
		assert!(Arc::ptr_eq(&actual, &expected));
		assert!(!Arc::ptr_eq(&actual, &stale));

		element.set_state(gst::State::Ready).expect("stop session");
		pad.lifecycle().completion = Some(stale);
		sink.imp().control.lock().unwrap().admissions.insert(pad.clone());
		let (_, finished) = sink.imp().confirm_admission(&pad);
		sink.imp().publish_finished(finished);
		assert!(pad.lifecycle().completion.is_none());
		let _ = element.set_state(gst::State::Null);
	}

	#[test]
	fn internal_release_tolerates_an_already_detached_pad() {
		gst::init().unwrap();
		let sink = sink();
		let element = sink.clone().upcast::<gst::Element>();
		let pad = element.request_pad_simple("sink_0").expect("request sink_0");

		sink.imp().release_pad(&pad);
		assert!(pad.parent().is_none());
		sink.imp().release_pad(&pad);

		let pad = pad.downcast::<MoqSinkPad>().expect("MoqSinkPad");
		assert_eq!(pad.property::<Status>("track-status"), Status::Pending);
		assert!(pad.parent().is_none());
	}

	/// A finalize pass can apply clean and failed outcomes before notifying either pad.
	#[test]
	fn a_partial_finalization_reports_both_outcomes() {
		gst::init().unwrap();
		let sink = sink();
		let element = sink.clone().upcast::<gst::Element>();
		let ended = element.request_pad_simple("sink_0").expect("request sink_0");
		let failed = element.request_pad_simple("sink_1").expect("request sink_1");
		let ended = ended.downcast::<MoqSinkPad>().expect("sink_0 is a MoqSinkPad");
		let failed = failed.downcast::<MoqSinkPad>().expect("sink_1 is a MoqSinkPad");

		let ended_changes = ended.lifecycle().end();
		let failed_changes = failed.lifecycle().fail("finalize sink_1: closed".to_string());
		sink.imp().notify_updates(vec![
			PadUpdate {
				pad: ended.clone(),
				changes: ended_changes,
			},
			PadUpdate {
				pad: failed.clone(),
				changes: failed_changes,
			},
		]);

		assert_eq!(ended.property::<Status>("track-status"), Status::Ended);
		assert_eq!(ended.property::<Option<String>>("track-error"), None);
		assert_eq!(
			failed.property::<Status>("track-status"),
			Status::Error,
			"a producer that would not close leaves its pad in error, not ended"
		);
		assert_eq!(
			failed.property::<Option<String>>("track-error").as_deref(),
			Some("finalize sink_1: closed")
		);
	}

	// The element still owes the bus a failure, even though each pad reported its own.
	#[test]
	fn a_finalize_failure_reaches_the_bus() {
		gst::init().unwrap();
		let sink = sink();
		sink.set_property("url", "https://127.0.0.1:1");
		sink.set_property("broadcast", "test");
		let element = sink.clone().upcast::<gst::Element>();
		let bus = gst::Bus::new();
		element.set_bus(Some(&bus));
		element.set_state(gst::State::Paused).expect("start session");
		let session = sink
			.imp()
			.control
			.lock()
			.unwrap()
			.live
			.as_ref()
			.expect("live session")
			.session
			.completion();
		while bus.pop().is_some() {}

		sink.imp().post_current(Posted::Message {
			session,
			kind: MessageKind::FinalizeError("finalize sink_1: closed".to_string()),
		});

		let message = bus.pop().expect("the element posted a message");
		let gst::MessageView::Error(error) = message.view() else {
			panic!("expected an error message, got {:?}", message.type_());
		};
		assert!(
			error.debug().is_some_and(|debug| debug.contains("closed")),
			"the reason travels with the message"
		);
		let _ = element.set_state(gst::State::Null);
	}

	// A clean end keeps the broadcast up for subscribers that arrive late, and the EOS waits for it:
	// `gst-launch` exits on that message, which would close the broadcast under them. The real sink
	// lingers on its own runtime, so this drives the same task on a paused clock over a stand-in track.
	#[tokio::test(start_paused = true)]
	async fn the_eos_waits_for_the_linger() {
		gst::init().unwrap();
		let sink = sink();
		sink.set_property("url", "https://127.0.0.1:1");
		sink.set_property("broadcast", "linger-test");
		let element = sink.clone().upcast::<gst::Element>();
		let bus = gst::Bus::new();
		element.set_bus(Some(&bus));
		element.set_state(gst::State::Playing).expect("start playing");
		let (broadcast, session) = {
			let control = sink.imp().control.lock().unwrap();
			let state = control.live.as_ref().expect("live session");
			(state.broadcast.broadcast().clone(), state.session.completion())
		};
		let consumer = broadcast.consume();
		let track = broadcast.create_track("video", None).unwrap();
		track.append_group().unwrap().finish().unwrap();
		track.finish().unwrap();
		assert!(session.finish());
		while bus.pop().is_some() {}

		let linger = Duration::from_secs(30);
		let start = moq_clock::now();
		let finished = moq_mux::Linger::new([broadcast], track, linger);
		let lingering = tokio::spawn(linger_task(finished, sink.downgrade(), session));

		moq_clock::sleep(linger - Duration::from_secs(1)).await;
		let mut late = consumer.track("video").unwrap().subscribe(None).await.unwrap();
		assert!(
			late.recv_group().await.unwrap().is_some(),
			"a late subscriber drains the tail"
		);
		assert!(bus.pop_filtered(&[gst::MessageType::Eos]).is_none(), "the EOS waits");

		lingering.await.unwrap();
		assert_eq!(moq_clock::now() - start, linger);
		assert!(consumer.is_closed());
		assert!(
			bus.pop_filtered(&[gst::MessageType::Eos]).is_some(),
			"the EOS follows the linger"
		);
		let _ = element.set_state(gst::State::Null);
	}

	#[test]
	fn errors_are_scoped_to_the_session_that_produced_them() {
		gst::init().unwrap();
		let sink = sink();
		sink.set_property("url", "https://127.0.0.1:1");
		sink.set_property("broadcast", "test");
		let element = sink.clone().upcast::<gst::Element>();
		let bus = gst::Bus::new();
		element.set_bus(Some(&bus));
		let pad = element.request_pad_simple("sink_0").expect("request sink_0");
		element.set_state(gst::State::Paused).expect("start first session");
		let first = sink
			.imp()
			.control
			.lock()
			.unwrap()
			.live
			.as_ref()
			.expect("first session")
			.session
			.completion();

		element.set_state(gst::State::Ready).expect("stop first session");
		element
			.set_state(gst::State::Paused)
			.expect("start replacement session");
		let replacement = sink
			.imp()
			.control
			.lock()
			.unwrap()
			.live
			.as_ref()
			.expect("replacement session")
			.session
			.completion();
		assert!(negotiate(&pad, &h264_caps()));
		while bus.pop().is_some() {}
		sink.imp().post_current(Posted::Message {
			session: first.clone(),
			kind: MessageKind::FinalizeError("old finalization failed".to_string()),
		});
		sink.imp().post_session_error(&first, "old session failed".to_string());

		assert!(
			bus.pop_filtered(&[gst::MessageType::Error]).is_none(),
			"the stopped session posted an error into its replacement"
		);
		assert_eq!(
			pad.property::<Status>("track-status"),
			Status::Active,
			"the stopped session did not fail a replacement pad"
		);

		sink.imp()
			.post_session_error(&replacement, "current session failed".to_string());
		let message = bus
			.pop_filtered(&[gst::MessageType::Error])
			.expect("the current session posted its error");
		let gst::MessageView::Error(error) = message.view() else {
			unreachable!();
		};
		assert!(
			error
				.debug()
				.is_some_and(|debug| debug.contains("current session failed"))
		);
		assert_eq!(pad.property::<Status>("track-status"), Status::Error);
		let _ = element.set_state(gst::State::Null);
	}

	#[test]
	fn startup_properties_declare_their_window() {
		gst::init().unwrap();
		let sink = sink();
		for name in [
			"url",
			"broadcast",
			"linger",
			"tls-disable-verify",
			"quic-idle-timeout",
			"quic-keep-alive",
			"quic-keylog",
		] {
			assert!(
				spec(&sink, name).flags().contains(gst::PARAM_FLAG_MUTABLE_READY),
				"{name} does not declare MUTABLE_READY"
			);
		}
		for (name, value) in [("quic-idle-timeout", 10_000), ("quic-keep-alive", 3_000)] {
			assert_eq!(spec(&sink, name).value_type(), u64::static_type());
			assert_eq!(sink.property::<u64>(name), value);
		}
		assert_eq!(spec(&sink, "linger").default_value().get::<u64>(), Ok(30_000));
		for name in [
			"status",
			"connected",
			"moq-version",
			"estimated-send-rate",
			"estimated-recv-rate",
			"connection-stats",
			"sessions",
		] {
			assert!(
				!spec(&sink, name).flags().contains(glib::ParamFlags::WRITABLE),
				"{name} is writable"
			);
		}
		assert!(sink.property::<Option<gst::Structure>>("connection-stats").is_none());
		let sessions = sink.property::<gst::Structure>("sessions");
		assert_eq!(sessions.get::<u64>("started"), Ok(0));
		assert_eq!(sessions.get::<u64>("ended"), Ok(0));
	}

	#[test]
	fn duration_millis_saturates() {
		assert_eq!(crate::connection::duration_millis(Duration::MAX), u64::MAX);
	}

	#[test]
	fn quic_properties_reach_the_client_config() {
		gst::init().unwrap();
		let sink = sink();
		sink.set_property("url", "https://relay.example.com/anon");
		sink.set_property("broadcast", "test");
		sink.set_property("quic-idle-timeout", 15_000u64);
		sink.set_property("quic-keep-alive", 3_000u64);

		let resolved = ResolvedSettings::try_from(sink.imp().settings.lock().unwrap().clone()).unwrap();
		let quic = super::super::session::quic_config(&resolved).resolve();
		assert_eq!(quic.idle_timeout, Duration::from_secs(15));
		assert_eq!(quic.keep_alive, Some(Duration::from_secs(3)));
		assert!(!sink.property::<bool>("quic-keylog"));
		assert!(quic.keylog.is_none());

		sink.set_property("quic-keep-alive", 0u64);
		sink.set_property("quic-idle-timeout", 0u64);
		sink.set_property("quic-keylog", true);
		let resolved = ResolvedSettings::try_from(sink.imp().settings.lock().unwrap().clone()).unwrap();
		let quic = super::super::session::quic_config(&resolved).resolve();
		assert_eq!(quic.idle_timeout, Duration::ZERO);
		assert_eq!(quic.keep_alive, Some(Duration::ZERO));
		assert!(quic.keylog.is_some());
	}

	/// The sink retries without a budget, so an outage of any length is ridden out
	/// (the session tests' relay loss covers the redial itself).
	#[test]
	fn the_sink_retries_without_a_budget() {
		gst::init().unwrap();
		let sink = sink();
		sink.set_property("url", "https://relay.example.com/anon");
		sink.set_property("broadcast", "test");
		let resolved = ResolvedSettings::try_from(sink.imp().settings.lock().unwrap().clone()).unwrap();
		let config = super::super::session::connect_config(&resolved);
		assert_eq!(
			config.backoff.timeout,
			Some((Duration::ZERO).into()),
			"zero is unlimited"
		);
	}

	#[test]
	fn a_started_element_keeps_its_startup_properties() {
		gst::init().unwrap();
		let sink = sink();
		sink.set_property("url", "https://127.0.0.1:1");
		sink.set_property("broadcast", "before");
		sink.set_property("quic-idle-timeout", 15_000u64);
		sink.set_property("quic-keep-alive", 3_000u64);
		assert_eq!(sink.property::<String>("broadcast"), "before");

		sink.set_state(gst::State::Paused).unwrap();
		sink.set_property("broadcast", "after");
		sink.set_property("quic-idle-timeout", 20_000u64);
		sink.set_property("quic-keep-alive", 4_000u64);
		assert_eq!(
			sink.property::<String>("broadcast"),
			"before",
			"a write above READY must not be stored: it would read back without taking effect"
		);
		assert_eq!(sink.property::<u64>("quic-idle-timeout"), 15_000);
		assert_eq!(sink.property::<u64>("quic-keep-alive"), 3_000);

		// MUTABLE_READY means configurable on every run, not just before the first.
		sink.set_state(gst::State::Ready).unwrap();
		sink.set_property("broadcast", "after");
		sink.set_property("quic-idle-timeout", 20_000u64);
		sink.set_property("quic-keep-alive", 4_000u64);
		assert_eq!(sink.property::<String>("broadcast"), "after");
		assert_eq!(sink.property::<u64>("quic-idle-timeout"), 20_000);
		assert_eq!(sink.property::<u64>("quic-keep-alive"), 4_000);
		sink.set_state(gst::State::Null).unwrap();
	}

	#[test]
	fn pad_containers_reach_the_catalog_through_caps() {
		gst::init().unwrap();
		let sink = sink();
		sink.set_property("url", "https://127.0.0.1:1");
		sink.set_property("broadcast", "test");
		let pad = sink.request_pad_simple("sink_0").unwrap();
		pad.set_property("track", "camera");
		pad.set_property("container", MediaContainer::Loc);
		let legacy_pad = sink.request_pad_simple("sink_1").unwrap();
		legacy_pad.set_property("track", "legacy");

		sink.set_state(gst::State::Paused).unwrap();
		let caps = gst::Caps::builder("video/x-h264")
			.field("stream-format", "byte-stream")
			.field("alignment", "au")
			.build();
		for (pad, stream) in [(&pad, "loc"), (&legacy_pad, "legacy")] {
			assert!(pad.send_event(gst::event::StreamStart::new(stream)));
			assert!(pad.send_event(gst::event::Caps::new(&caps)));
			assert!(pad.send_event(gst::event::Segment::new(
				&gst::FormattedSegment::<gst::ClockTime>::new(),
			)));
			let mut buffer = gst::Buffer::from_slice(super::super::pad::h264_keyframe_au());
			buffer.get_mut().unwrap().set_pts(Some(gst::ClockTime::ZERO));
			assert_eq!(pad.chain(buffer), Ok(gst::FlowSuccess::Ok));
		}

		let snapshot = {
			let control = sink.imp().control.lock().unwrap();
			control.live.as_ref().unwrap().catalog.as_ref().unwrap().snapshot()
		};
		assert_eq!(
			snapshot.video.renditions.get("camera").unwrap().container,
			hang::catalog::Container::Loc
		);
		assert_eq!(
			snapshot.video.renditions.get("legacy").unwrap().container,
			hang::catalog::Container::Legacy
		);
		sink.set_state(gst::State::Null).unwrap();
	}
}
