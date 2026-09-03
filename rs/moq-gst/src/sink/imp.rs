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

use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use hang::moq_net;

use super::pad::{CapsOutcome, ProducerOptions, PushOutcome, caps_supported};
use super::request_pad::{MoqSinkPad, Notifications};
use super::session::{
	CAT, Completion, CompletionHandle, ConnectionStatus, RUNTIME, ResolvedSettings, Session, SessionRegistration,
};

#[derive(Debug, Clone, Default)]
struct Settings {
	url: Option<String>,
	broadcast: Option<String>,
	tls_disable_verify: bool,
	quic_idle_timeout: Option<Duration>,
	quic_keep_alive: Option<Duration>,
}

fn duration_millis(duration: Duration) -> u64 {
	duration.as_millis().try_into().unwrap_or(u64::MAX)
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
			quic_idle_timeout: value.quic_idle_timeout,
			quic_keep_alive: value.quic_keep_alive,
		})
	}
}

/// Live state for one READY to PAUSED run.
struct State {
	session: Session,
	broadcast: moq_net::broadcast::Producer,
	catalog: Option<moq_mux::catalog::Producer>,
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
	admissions: usize,
	/// Whether the element completed its transition into PLAYING. Tracked here rather than read from
	/// GStreamer: the current state is not committed until `change_state` returns, so a message earned
	/// inside that window would read PAUSED and be held with nobody left to release it.
	playing: bool,
}

/// Claim this publication's EOS, if it is owed and may go out now.
///
/// One claim per publication, so whichever arrives first (the transition into PLAYING, or the pad that
/// ended it) is the only one that posts. Called under `control`, which is what serializes the two.
fn claim_eos(control: &mut Control) -> Posted {
	if !control.playing {
		return Posted::Nothing;
	}
	let Some(state) = control.live.as_mut() else {
		return Posted::Nothing;
	};
	if state.eos_delivered || state.session.completion().get() != Completion::Eos {
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
	/// Every pad ended and the producers closed.
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
			let quic = moq_tokio::quic::Resolved::default();
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
				glib::ParamSpecBoolean::builder("tls-disable-verify")
					.nick("TLS disable verify")
					.blurb("Disable TLS verification")
					.default_value(false)
					.mutable_ready()
					.build(),
				glib::ParamSpecUInt64::builder("quic-idle-timeout")
					.nick("QUIC idle timeout")
					.blurb("QUIC idle timeout in milliseconds, 0 to disable locally")
					.default_value(duration_millis(quic.idle_timeout))
					.mutable_ready()
					.build(),
				glib::ParamSpecUInt64::builder("quic-keep-alive")
					.nick("QUIC keep-alive")
					.blurb("QUIC keep-alive interval in milliseconds, 0 to disable; ignored by iroh")
					.default_value(quic.keep_alive.map(duration_millis).unwrap_or(0))
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
				glib::ParamSpecUInt64::builder("estimated-send-bitrate")
					.nick("Estimated send bitrate")
					.blurb("Estimated send bitrate in bits per second (congestion controller), 0 when unavailable")
					.read_only()
					.build(),
				glib::ParamSpecUInt64::builder("estimated-recv-bitrate")
					.nick("Estimated receive bitrate")
					.blurb("Estimated receive bitrate in bits per second, 0 when unavailable")
					.read_only()
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
			"quic-idle-timeout" => settings.quic_idle_timeout = Some(Duration::from_millis(value.get().unwrap())),
			"quic-keep-alive" => settings.quic_keep_alive = Some(Duration::from_millis(value.get().unwrap())),
			_ => unreachable!(),
		}
	}

	fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
		match pspec.name() {
			"status" | "connected" | "moq-version" | "estimated-send-bitrate" | "estimated-recv-bitrate" => {
				let control = self.control.lock().unwrap();
				let session = control.live.as_ref().map(|s| &s.session);
				match pspec.name() {
					"status" => session.map(|s| s.status().status()).unwrap_or_default().to_value(),
					"connected" => session.is_some_and(|s| s.status().connected()).to_value(),
					"moq-version" => session.and_then(|s| s.status().version()).to_value(),
					"estimated-send-bitrate" => session.map(|s| s.send_bitrate()).unwrap_or(0).to_value(),
					"estimated-recv-bitrate" => session.map(|s| s.recv_bitrate()).unwrap_or(0).to_value(),
					_ => unreachable!(),
				}
			}
			name => {
				let settings = self.settings.lock().unwrap();
				match name {
					"url" => settings.url.to_value(),
					"broadcast" => settings.broadcast.to_value(),
					"tls-disable-verify" => settings.tls_disable_verify.to_value(),
					"quic-idle-timeout" => duration_millis(
						settings
							.quic_idle_timeout
							.unwrap_or_else(|| moq_tokio::quic::Resolved::default().idle_timeout),
					)
					.to_value(),
					"quic-keep-alive" => settings
						.quic_keep_alive
						.or_else(|| moq_tokio::quic::Resolved::default().keep_alive)
						.map(duration_millis)
						.unwrap_or(0)
						.to_value(),
					_ => unreachable!(),
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
			control.admissions += 1;
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
	/// Create the session and producers before any buffer flows.
	fn start_session(&self) -> Result<SessionRegistration, gst::StateChangeError> {
		let settings = ResolvedSettings::try_from(self.settings.lock().unwrap().clone()).map_err(|err| {
			gst::error!(CAT, obj = self.obj(), "invalid settings: {err:#}");
			gst::StateChangeError
		})?;
		let (session, registration, broadcast, catalog) =
			Session::start(settings, self.obj().downgrade()).map_err(|err| {
				gst::error!(CAT, obj = self.obj(), "failed to start session: {err:?}");
				gst::StateChangeError
			})?;
		let completion = session.completion();
		let updates = {
			let mut control = self.control.lock().unwrap();
			control.live = Some(State {
				session,
				broadcast,
				catalog: Some(catalog),
				eos_delivered: false,
			});
			self.obj()
				.sink_pads()
				.into_iter()
				.filter_map(|pad| pad.downcast::<MoqSinkPad>().ok())
				.map(|pad| {
					let changes = {
						let mut lifecycle = pad.lifecycle();
						let changes = lifecycle.reset();
						lifecycle.completion = Some(completion.clone());
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
		let (mut state, updates, mut failure) = {
			let mut control = self.control.lock().unwrap();
			let Some(state) = control.live.take() else {
				return;
			};
			let _rt = RUNTIME.enter();
			let mut failure = None;
			let updates = self
				.obj()
				.sink_pads()
				.into_iter()
				.filter_map(|pad| pad.downcast::<MoqSinkPad>().ok())
				.map(|pad| {
					let changes = {
						let mut lifecycle = pad.lifecycle();
						if let Err(err) = lifecycle.media.finalize() {
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
			(state, updates, failure)
		};
		let _rt = RUNTIME.enter();
		if let Some(mut catalog) = state.catalog.take()
			&& let Err(err) = catalog.finish().context("finalize catalog")
			&& failure.is_none()
		{
			failure = Some(err);
		}
		if let Some(err) = failure {
			gst::warning!(CAT, "finalize on stop: {err:?}");
		}
		// Finish the broadcast (a deliberate end, so no dropped-without-finish
		// warning) before reaping the session task.
		state.broadcast.finish();
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
		let pts = buffer.pts();
		// Only subtitles use this: a cue needs an explicit end, unlike a media frame.
		let duration = buffer.duration();
		let current_running_time = self.obj().current_running_time();
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
			let outcome = lifecycle.media.push_buffer(data, pts, duration, current_running_time);
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
						let completion = state.session.completion();
						let mut lifecycle = sink_pad.lifecycle();
						if lifecycle.releasing {
							return false;
						}
						let requested = lifecycle.requested().map(str::to_owned);
						let mut options = ProducerOptions::new(&caps).with_container(lifecycle.container().into());
						if let Some(track) = requested.as_deref() {
							options = options.with_track(track);
						}
						let outcome = lifecycle.media.observe_caps(&state.broadcast, catalog, options);
						lifecycle.completion = Some(completion);
						Some(match outcome {
							CapsOutcome::Active(track) => (Some(lifecycle.commit(track)), true),
							CapsOutcome::Failed(reason) => (Some(lifecycle.fail(reason)), false),
							CapsOutcome::Unchanged => (None, true),
						})
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
				let finished = {
					let mut control = self.control.lock().unwrap();
					if control.live.as_ref().is_some_and(|state| state.is_open()) {
						let mut lifecycle = sink_pad.lifecycle();
						if !lifecycle.releasing {
							lifecycle.ended = true;
						}
					}
					self.maybe_finish_locked(&mut control)
				};
				self.publish_finished(finished);
				gst::Pad::event_default(pad, Some(&*self.obj()), event)
			}
			// A pad that starts flushing stops counting towards EOS immediately, not when the flush
			// finishes. Waiting for FLUSH_STOP lets another pad's EOS complete the element inside the
			// flush window, and the finalize that follows is not something FLUSH_STOP can undo.
			gst::EventView::FlushStart(_) => {
				let control = self.control.lock().unwrap();
				if control.live.as_ref().is_some_and(|state| state.is_open()) {
					let mut lifecycle = sink_pad.lifecycle();
					if !lifecycle.releasing {
						lifecycle.ended = false;
					}
				}
				drop(control);
				gst::Pad::event_default(pad, Some(&*self.obj()), event)
			}
			gst::EventView::FlushStop(_) => {
				let control = self.control.lock().unwrap();
				if control.live.as_ref().is_some_and(|state| state.is_open()) {
					let mut lifecycle = sink_pad.lifecycle();
					if !lifecycle.releasing {
						lifecycle.ended = false;
						lifecycle.media.flush();
					}
				}
				drop(control);
				gst::Pad::event_default(pad, Some(&*self.obj()), event)
			}
			// A new stream on this pad is a fresh start: it no longer counts as ended, and its timeline is
			// re-anchored like a flush. Without that the old stream's segment base stays current, and a
			// new stream restarting at zero reads as a rewind, which invalidates the pad and silently
			// drops its buffers.
			gst::EventView::StreamStart(_) => {
				let control = self.control.lock().unwrap();
				if control.live.as_ref().is_some_and(|state| state.is_open()) {
					let mut lifecycle = sink_pad.lifecycle();
					if !lifecycle.releasing {
						lifecycle.ended = false;
						lifecycle.media.flush();
					}
				}
				drop(control);
				gst::Pad::event_default(pad, Some(&*self.obj()), event)
			}
			_ => gst::Pad::event_default(pad, Some(&*self.obj()), event),
		}
	}

	fn owns_pad(&self, pad: &MoqSinkPad) -> bool {
		pad.parent().as_ref() == Some(self.obj().upcast_ref::<gst::Object>())
	}

	fn cancel_admission(&self, pad: &MoqSinkPad) {
		let finished = self.finish_admission();
		self.publish_finished(finished);
		if self.owns_pad(pad) {
			self.release_pad(pad.upcast_ref());
		} else {
			pad.reset_detached();
		}
	}

	fn finish_admission(&self) -> Finished {
		let mut control = self.control.lock().unwrap();
		self.finish_admission_locked(&mut control)
	}

	fn finish_admission_locked(&self, control: &mut Control) -> Finished {
		debug_assert!(control.admissions > 0);
		control.admissions = control.admissions.saturating_sub(1);
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
		let finished = self.finish_admission_locked(&mut control);
		(admission, finished)
	}

	fn maybe_finish_locked(&self, control: &mut Control) -> Finished {
		if control.admissions > 0 {
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
		for pad in pads {
			let mut lifecycle = pad.lifecycle();
			if lifecycle.releasing || !self.owns_pad(&pad) {
				continue;
			}
			let result = lifecycle.media.finalize();
			let changes = match result {
				Ok(true) => Some(lifecycle.end()),
				Ok(false) => None,
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
		if let Some(mut catalog) = state.catalog.take()
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
					session: state.session.completion(),
					kind: MessageKind::FinalizeError(format!("{err:?}")),
				},
				false => Posted::Nothing,
			},
			None => {
				completion.finish();
				claim_eos(control)
			}
		};
		Finished { updates, message }
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
		self.control.lock().unwrap().playing = false;
	}

	fn publish_finished(&self, finished: Finished) {
		self.notify_updates(finished.updates);
		self.post_current(finished.message);
	}

	/// Route a terminal reconnect error through the same session gate as finalization messages.
	pub(super) fn post_session_error(&self, session: &CompletionHandle, error: String) {
		self.publish_finished(Finished {
			updates: Vec::new(),
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
				gst::info!(CAT, "all pads ended, posting EOS");
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

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use super::super::MediaContainer;
	use super::super::request_pad::Status;
	use super::super::session::CompletionState;
	use super::*;

	fn sink() -> super::super::MoqSink {
		glib::Object::builder::<super::super::MoqSink>().build()
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
		sink.imp().control.lock().unwrap().admissions += 1;
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

		sink.imp().control.lock().unwrap().admissions += 1;
		assert!(pad.send_event(gst::event::Eos::new()));
		let pad = pad.downcast::<MoqSinkPad>().expect("MoqSinkPad");
		assert_eq!(pad.property::<Status>("track-status"), Status::Active);

		let finished = sink.imp().finish_admission();
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
		sink.imp().control.lock().unwrap().admissions += 1;
		let (_, finished) = sink.imp().confirm_admission(&pad);
		sink.imp().publish_finished(finished);

		let actual = pad.lifecycle().completion.clone().expect("session link");
		assert!(Arc::ptr_eq(&actual, &expected));
		assert!(!Arc::ptr_eq(&actual, &stale));

		element.set_state(gst::State::Ready).expect("stop session");
		pad.lifecycle().completion = Some(stale);
		sink.imp().control.lock().unwrap().admissions += 1;
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

	#[test]
	fn errors_are_scoped_to_the_session_that_produced_them() {
		gst::init().unwrap();
		let sink = sink();
		sink.set_property("url", "https://127.0.0.1:1");
		sink.set_property("broadcast", "test");
		let element = sink.clone().upcast::<gst::Element>();
		let bus = gst::Bus::new();
		element.set_bus(Some(&bus));
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
		while bus.pop().is_some() {}
		sink.imp().post_current(Posted::Message {
			session: first.clone(),
			kind: MessageKind::FinalizeError("old finalization failed".to_string()),
		});
		sink.imp().post_session_error(&first, "old session failed".to_string());

		assert!(
			bus.timed_pop_filtered(gst::ClockTime::ZERO, &[gst::MessageType::Error])
				.is_none(),
			"the stopped session posted an error into its replacement"
		);

		sink.imp()
			.post_session_error(&replacement, "current session failed".to_string());
		let message = bus
			.timed_pop_filtered(gst::ClockTime::ZERO, &[gst::MessageType::Error])
			.expect("the current session posted its error");
		let gst::MessageView::Error(error) = message.view() else {
			unreachable!();
		};
		assert!(
			error
				.debug()
				.is_some_and(|debug| debug.contains("current session failed"))
		);
		let _ = element.set_state(gst::State::Null);
	}

	#[test]
	fn startup_properties_declare_their_window() {
		gst::init().unwrap();
		let sink = sink();
		for name in [
			"url",
			"broadcast",
			"tls-disable-verify",
			"quic-idle-timeout",
			"quic-keep-alive",
		] {
			assert!(
				spec(&sink, name).flags().contains(gst::PARAM_FLAG_MUTABLE_READY),
				"{name} does not declare MUTABLE_READY"
			);
		}
		for (name, value) in [("quic-idle-timeout", 30_000), ("quic-keep-alive", 5_000)] {
			assert_eq!(spec(&sink, name).value_type(), u64::static_type());
			assert_eq!(sink.property::<u64>(name), value);
		}
		for name in [
			"status",
			"connected",
			"moq-version",
			"estimated-send-bitrate",
			"estimated-recv-bitrate",
		] {
			assert!(
				!spec(&sink, name).flags().contains(glib::ParamFlags::WRITABLE),
				"{name} is writable"
			);
		}
	}

	#[test]
	fn duration_millis_saturates() {
		assert_eq!(duration_millis(Duration::MAX), u64::MAX);
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

		sink.set_property("quic-keep-alive", 0u64);
		sink.set_property("quic-idle-timeout", 0u64);
		let resolved = ResolvedSettings::try_from(sink.imp().settings.lock().unwrap().clone()).unwrap();
		let quic = super::super::session::quic_config(&resolved).resolve();
		assert_eq!(quic.idle_timeout, Duration::ZERO);
		assert_eq!(quic.keep_alive, None);
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
