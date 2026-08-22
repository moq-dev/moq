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

use std::sync::atomic::Ordering;
use std::sync::{LazyLock, Mutex};

use anyhow::{Context, Result};
use bytes::Bytes;
use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use hang::moq_net;

use super::pad::{CapsOutcome, PushOutcome, caps_supported};
use super::request_pad::{MoqSinkPad, Notifications};
use super::session::{CAT, ConnectionStatus, RUNTIME, ResolvedSettings, Session, SessionId};

#[derive(Debug, Clone, Default)]
struct Settings {
	url: Option<String>,
	broadcast: Option<String>,
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

/// Live state for one READY to PAUSED run.
struct State {
	session: Session,
	broadcast: moq_net::broadcast::Producer,
	catalog: Option<moq_mux::catalog::Producer>,
	eos_posted: bool,
}

/// Element state that survives session replacement.
#[derive(Default)]
struct Control {
	live: Option<State>,
	admissions: usize,
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
	/// A message earned by one publishing session.
	Message { session: SessionId, kind: MessageKind },
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
				glib::ParamSpecBoolean::builder("tls-disable-verify")
					.nick("TLS disable verify")
					.blurb("Disable TLS verification")
					.default_value(false)
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
			if control.live.as_ref().is_some_and(|state| state.eos_posted) {
				gst::warning!(CAT, "refusing a pad after EOS: the session is finalized");
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
		let finished = self.confirm_admission(&pad);
		self.publish_finished(finished);
		if !self.owns_pad(&pad) {
			pad.reset_detached();
			return None;
		}
		Some(pad.upcast())
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
			gst::StateChange::ReadyToPaused => self.start_session()?,
			gst::StateChange::PausedToReady => self.stop_session(),
			_ => {}
		}
		self.parent_change_state(transition)
	}
}

impl MoqSink {
	/// Create the session and producers before any buffer flows.
	fn start_session(&self) -> Result<(), gst::StateChangeError> {
		let settings = ResolvedSettings::try_from(self.settings.lock().unwrap().clone()).map_err(|err| {
			gst::error!(CAT, obj = self.obj(), "invalid settings: {err:#}");
			gst::StateChangeError
		})?;
		let (session, broadcast, catalog) = Session::start(settings, self.obj().downgrade()).map_err(|err| {
			gst::error!(CAT, obj = self.obj(), "failed to start session: {err:?}");
			gst::StateChangeError
		})?;
		let error = session.error_flag();
		let updates = {
			let mut control = self.control.lock().unwrap();
			control.live = Some(State {
				session,
				broadcast,
				catalog: Some(catalog),
				eos_posted: false,
			});
			self.obj()
				.sink_pads()
				.into_iter()
				.filter_map(|pad| pad.downcast::<MoqSinkPad>().ok())
				.map(|pad| {
					let changes = {
						let mut lifecycle = pad.lifecycle();
						let changes = lifecycle.reset();
						lifecycle.session_error = Some(error.clone());
						changes
					};
					PadUpdate { pad, changes }
				})
				.collect::<Vec<_>>()
		};
		self.notify_updates(updates);
		Ok(())
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
			if lifecycle.releasing || lifecycle.session_error.is_none() {
				return Err(gst::FlowError::Flushing);
			}
			if lifecycle
				.session_error
				.as_ref()
				.is_some_and(|error| error.load(Ordering::Relaxed))
			{
				return Err(gst::FlowError::Error);
			}
			if lifecycle.media.is_failed() {
				return Ok(gst::FlowSuccess::Ok);
			}
			let outcome = lifecycle.media.push_buffer(data, pts);
			let changes = match &outcome {
				PushOutcome::Failed(reason) => Some(lifecycle.fail(reason.clone())),
				_ => None,
			};
			(outcome, changes)
		};
		let no_segment = outcome == PushOutcome::NoSegment;
		if let Some(changes) = changes {
			pad.notify_changes(changes);
		}

		if no_segment {
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
				if !caps_supported(&caps) {
					gst::warning!(CAT, "rejecting unsupported caps on pad {}", pad.name());
					let changes = {
						let _rt = RUNTIME.enter();
						let mut lifecycle = sink_pad.lifecycle();
						lifecycle.media.invalidate();
						lifecycle.fail(format!("unsupported caps: {caps}"))
					};
					sink_pad.notify_changes(changes);
					return false;
				}
				let (changes, accepted) = {
					let _rt = RUNTIME.enter();
					let control = self.control.lock().unwrap();
					let Some(state) = control.live.as_ref().filter(|state| !state.eos_posted) else {
						return gst::Pad::event_default(pad, Some(&*self.obj()), event);
					};
					let Some(catalog) = state.catalog.as_ref() else {
						return gst::Pad::event_default(pad, Some(&*self.obj()), event);
					};
					let error = state.session.error_flag();
					let mut lifecycle = sink_pad.lifecycle();
					if lifecycle.releasing {
						return false;
					}
					let requested = lifecycle.requested().map(str::to_owned);
					let outcome = lifecycle
						.media
						.observe_caps(&state.broadcast, catalog, &caps, requested.as_deref());
					lifecycle.session_error = Some(error);
					match outcome {
						CapsOutcome::Active(track) => (Some(lifecycle.commit(track)), true),
						CapsOutcome::Failed(reason) => (Some(lifecycle.fail(reason)), false),
						CapsOutcome::Unchanged => (None, true),
					}
				};
				if let Some(changes) = changes {
					sink_pad.notify_changes(changes);
				}
				accepted && gst::Pad::event_default(pad, Some(&*self.obj()), event)
			}
			gst::EventView::Segment(segment) => {
				let control = self.control.lock().unwrap();
				let error = control
					.live
					.as_ref()
					.filter(|state| !state.eos_posted)
					.map(|state| state.session.error_flag());
				let mut lifecycle = sink_pad.lifecycle();
				if !lifecycle.releasing {
					lifecycle.session_error = error;
					if lifecycle.session_error.is_some() {
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
					if control.live.as_ref().is_some_and(|state| !state.eos_posted) {
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
			gst::EventView::FlushStop(_) => {
				let control = self.control.lock().unwrap();
				if control.live.as_ref().is_some_and(|state| !state.eos_posted) {
					let mut lifecycle = sink_pad.lifecycle();
					if !lifecycle.releasing {
						lifecycle.ended = false;
						lifecycle.media.flush();
					}
				}
				drop(control);
				gst::Pad::event_default(pad, Some(&*self.obj()), event)
			}
			gst::EventView::StreamStart(_) => {
				let control = self.control.lock().unwrap();
				if control.live.as_ref().is_some_and(|state| !state.eos_posted) {
					let mut lifecycle = sink_pad.lifecycle();
					if !lifecycle.releasing {
						lifecycle.ended = false;
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

	fn confirm_admission(&self, pad: &MoqSinkPad) -> Finished {
		let mut control = self.control.lock().unwrap();
		let error = control
			.live
			.as_ref()
			.filter(|state| !state.eos_posted)
			.map(|state| state.session.error_flag());
		let mut lifecycle = pad.lifecycle();
		if !lifecycle.releasing {
			lifecycle.session_error = error;
		}
		drop(lifecycle);
		self.finish_admission_locked(&mut control)
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
		if !all_ended || state.eos_posted {
			return Finished::default();
		}

		state.eos_posted = true;
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
		let kind = match failure {
			Some(err) => MessageKind::FinalizeError(format!("{err:?}")),
			None => MessageKind::Eos,
		};
		let message = Posted::Message {
			session: state.session.id().clone(),
			kind,
		};
		Finished { updates, message }
	}

	fn notify_updates(&self, updates: Vec<PadUpdate>) {
		for update in updates {
			update.pad.notify_changes(update.changes);
		}
	}

	fn publish_finished(&self, finished: Finished) {
		self.notify_updates(finished.updates);
		self.post_current(finished.message);
	}

	/// Route a terminal reconnect error through the same session gate as finalization messages.
	pub(super) fn post_session_error(&self, session: &SessionId, error: String) {
		self.post_current(Posted::Message {
			session: session.clone(),
			kind: MessageKind::SessionError(error),
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
			.is_some_and(|state| state.session.id().matches(&session));
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
	use std::sync::atomic::AtomicBool;

	use super::super::request_pad::Status;
	use super::*;

	fn sink() -> super::super::MoqSink {
		glib::Object::builder::<super::super::MoqSink>().build()
	}

	fn spec(element: &super::super::MoqSink, name: &str) -> glib::ParamSpec {
		element.find_property(name).unwrap()
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
		assert_eq!(pad.property::<Status>("status"), Status::Active);

		let finished = sink.imp().finish_admission();
		sink.imp().publish_finished(finished);
		assert_eq!(pad.property::<Status>("status"), Status::Ended);
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
			.error_flag();
		let stale = Arc::new(AtomicBool::new(true));
		pad.lifecycle().session_error = Some(stale.clone());
		sink.imp().control.lock().unwrap().admissions += 1;
		let finished = sink.imp().confirm_admission(&pad);
		sink.imp().publish_finished(finished);

		let actual = pad.lifecycle().session_error.clone().expect("session link");
		assert!(Arc::ptr_eq(&actual, &expected));
		assert!(!Arc::ptr_eq(&actual, &stale));

		element.set_state(gst::State::Ready).expect("stop session");
		pad.lifecycle().session_error = Some(stale);
		sink.imp().control.lock().unwrap().admissions += 1;
		let finished = sink.imp().confirm_admission(&pad);
		sink.imp().publish_finished(finished);
		assert!(pad.lifecycle().session_error.is_none());
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
		assert_eq!(pad.property::<Status>("status"), Status::Pending);
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

		assert_eq!(ended.property::<Status>("status"), Status::Ended);
		assert_eq!(ended.property::<Option<String>>("track-error"), None);
		assert_eq!(
			failed.property::<Status>("status"),
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
			.id()
			.clone();
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
			.id()
			.clone();

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
			.id()
			.clone();
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
		for name in ["url", "broadcast", "tls-disable-verify"] {
			assert!(
				spec(&sink, name).flags().contains(gst::PARAM_FLAG_MUTABLE_READY),
				"{name} does not declare MUTABLE_READY"
			);
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
	fn a_started_element_keeps_its_startup_properties() {
		gst::init().unwrap();
		let sink = sink();
		sink.set_property("url", "https://127.0.0.1:1");
		sink.set_property("broadcast", "before");
		assert_eq!(sink.property::<String>("broadcast"), "before");

		sink.set_state(gst::State::Paused).unwrap();
		sink.set_property("broadcast", "after");
		assert_eq!(
			sink.property::<String>("broadcast"),
			"before",
			"a write above READY must not be stored: it would read back without taking effect"
		);

		// MUTABLE_READY means configurable on every run, not just before the first.
		sink.set_state(gst::State::Ready).unwrap();
		sink.set_property("broadcast", "after");
		assert_eq!(sink.property::<String>("broadcast"), "after");
		sink.set_state(gst::State::Null).unwrap();
	}
}
