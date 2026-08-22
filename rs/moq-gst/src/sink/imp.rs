//! GObject shell for the moqsink element, on a bare GstElement.
//!
//! Each request pad has its own chain function that writes buffers straight into the moq producers
//! from the streaming thread. There is no intermediate channel and no worker task: `moq_net`'s producer
//! writes are synchronous (an in-memory append, bounded by group eviction), so the streaming thread
//! never blocks on the network. A thin async task only owns connect and the session lifetime. Pads are
//! fully independent: one pad's chain never waits on another's data.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

use anyhow::{Context, Result};
use bytes::Bytes;
use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use hang::moq_net;

use super::pad::{Pad, caps_supported};
use super::request_pad::MoqSinkPad;
use super::session::{CAT, ConnectionStatus, RUNTIME, ResolvedSettings, Session};

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

/// Live state, present only while started. The producers are created up front (so frames buffered
/// before connect are sent once it completes); the catalog is `Option` because it is taken on the first
/// finalize. Per-pad media lives in `pads`; `ended` tracks EOS for element-level EOS aggregation.
struct State {
	session: Session,
	broadcast: moq_net::broadcast::Producer,
	catalog: Option<moq_mux::catalog::Producer>,
	pads: HashMap<String, Pad>,
	ended: HashSet<String>,
	eos_posted: bool,
}

impl State {
	/// Finalize every live producer once, catalog last; runs on EOS and on stop. Idempotent. The names of
	/// the producers finalized are accumulated into the `Ok` order until the first error, which is logged
	/// and then surfaced as the returned `Err`.
	fn finalize_all(&mut self) -> Result<Vec<String>> {
		let mut result: Result<Vec<String>> = Ok(Vec::new());
		for (name, pad) in self.pads.iter_mut() {
			match pad.finalize() {
				Ok(true) => {
					if let Ok(order) = result.as_mut() {
						order.push(name.clone());
					}
				}
				Ok(false) => {}
				Err(err) => {
					gst::warning!(CAT, "finalize {name}: {err:?}");
					if result.is_ok() {
						result = Err(err);
					}
				}
			}
		}
		if let Some(mut catalog) = self.catalog.take() {
			match catalog.finish().context("finalize catalog") {
				Ok(()) => {
					if let Ok(order) = result.as_mut() {
						order.push("catalog".to_string());
					}
				}
				Err(err) => {
					if result.is_ok() {
						result = Err(err);
					}
				}
			}
		}
		result
	}
}

/// The `moqsink` element implementation: its GObject properties plus the live session state.
#[derive(Default)]
pub struct MoqSink {
	settings: Mutex<Settings>,
	/// Live state between Ready->Paused and Paused->Ready. One Mutex, not Arc<Mutex>: glib already owns
	/// and shares the subclass instance across GStreamer's threads, so we need interior mutability but
	/// not a second ownership layer. Held only briefly per buffer, so independent pad threads barely
	/// contend.
	state: Mutex<Option<State>>,
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
				let state = self.state.lock().unwrap();
				let session = state.as_ref().map(|s| &s.session);
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

			let sink =
				gst::PadTemplate::new("sink_%u", gst::PadDirection::Sink, gst::PadPresence::Request, &caps).unwrap();
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
		self.obj().add_pad(&pad).ok()?;
		self.obj().child_added(&pad, pad.name().as_str());
		Some(pad.upcast())
	}

	fn release_pad(&self, pad: &gst::Pad) {
		// CAPS takes the pad lock before the element state lock. Keep the same order here so a CAPS
		// handler cannot insert a producer after this removal.
		let sink_pad = pad.downcast_ref::<MoqSinkPad>();
		let retirement = sink_pad.map(MoqSinkPad::retire_track);
		{
			let _rt = RUNTIME.enter();
			if let Some(state) = self.state.lock().unwrap().as_mut() {
				let name = pad.name();
				if let Some(mut media) = state.pads.remove(name.as_str())
					&& let Err(err) = media.finalize()
				{
					gst::warning!(CAT, "finalize on release {name}: {err:?}");
				}
				state.ended.remove(name.as_str());
			}
		}
		// The producer is gone, so the pad no longer holds a reservation: an application
		// keeping the released pad reads what it asked for, not a name nothing publishes.
		if let Some(retirement) = retirement {
			retirement.release();
		}
		if self.obj().remove_pad(pad).is_ok() {
			self.obj().child_removed(pad, pad.name().as_str());
		}
		if let Some(sink_pad) = sink_pad {
			sink_pad.finish_release();
		}
		// Removing a still-active pad can leave only already-ended pads, which now satisfies EOS.
		self.maybe_post_eos();
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
		*self.state.lock().unwrap() = Some(State {
			session,
			broadcast,
			catalog: Some(catalog),
			pads: HashMap::new(),
			ended: HashSet::new(),
			eos_posted: false,
		});
		Ok(())
	}

	/// Finalize the producers (catalog last) and tear down the session. Finalize is best-effort: we are
	/// tearing down regardless.
	fn stop_session(&self) {
		let Some(mut state) = self.state.lock().unwrap().take() else {
			return;
		};
		let _rt = RUNTIME.enter();
		if let Err(err) = state.finalize_all() {
			gst::warning!(CAT, "finalize on stop: {err:?}");
		}
		// Finish the broadcast (a deliberate end, so no dropped-without-finish
		// warning) before reaping the session task.
		state.broadcast.finish();
		state.session.stop();
		// The producers are gone, so each pad's name is configurable again for the next run.
		for pad in self.obj().sink_pads() {
			if let Some(pad) = pad.downcast_ref::<MoqSinkPad>() {
				pad.release_track();
			}
		}
	}

	/// Write one buffer straight into its pad's producer. Per-pad failures (bad caps/bitstream) drop
	/// quietly so the session and other pads keep going; an unmappable buffer or a dead session is a hard
	/// error on this pad's streaming thread.
	fn forward_buffer(&self, pad: &gst::Pad, buffer: gst::Buffer) -> Result<gst::FlowSuccess, gst::FlowError> {
		// Map and copy outside the lock: neither needs shared state, so the per-pad lock is held only for
		// the producer write. An oversized buffer is still copied here (it already exists upstream), but
		// moq-net rejects it (FrameTooLarge) before reserving its own group slot, and that error invalidates
		// just this pad.
		let pts = buffer.pts();
		let map = buffer.map_readable().map_err(|_| {
			gst::error!(CAT, "failed to map buffer on pad {}", pad.name());
			gst::FlowError::Error
		})?;
		let data = Bytes::copy_from_slice(map.as_slice());
		drop(map);
		let Some(activity) = pad.downcast_ref::<MoqSinkPad>().and_then(MoqSinkPad::reserve_track) else {
			return Err(gst::FlowError::Flushing);
		};

		// Producer writes can touch tokio time (group eviction), so hold the runtime context here.
		let _rt = RUNTIME.enter();
		let mut guard = self.state.lock().unwrap();
		let Some(state) = guard.as_mut() else {
			return Err(gst::FlowError::Flushing); // not started
		};
		if state.session.errored() {
			return Err(gst::FlowError::Error);
		}

		// The pad almost always exists already (caps arrive before buffers), so look it up without
		// allocating an owned name; only the rare first-buffer insert pays for the key.
		let name = pad.name();
		let media = match state.pads.get_mut(name.as_str()) {
			Some(media) => media,
			None => state.pads.entry(name.to_string()).or_insert_with(Pad::new),
		};
		if media.is_failed() {
			return Ok(gst::FlowSuccess::Ok); // drop quietly; the pad already reported its failure
		}

		let no_segment = media.push_buffer(data, pts);
		drop(guard);
		drop(activity);

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
		let Some(reservation) = pad.downcast_ref::<MoqSinkPad>().and_then(MoqSinkPad::reserve_track) else {
			return false;
		};
		match event.view() {
			gst::EventView::Caps(caps) => {
				let caps = caps.caps().to_owned();
				// Reject unsupported caps synchronously (NotNegotiated) before building a producer.
				if !caps_supported(&caps) {
					gst::warning!(CAT, "rejecting unsupported caps on pad {}", pad.name());
					return false;
				}
				let reserved = {
					let _rt = RUNTIME.enter();
					let mut guard = self.state.lock().unwrap();
					guard.as_mut().and_then(|state| {
						let State {
							broadcast,
							catalog,
							pads,
							..
						} = state;
						let catalog = catalog.as_ref()?;
						pads.entry(pad.name().to_string())
							.or_insert_with(Pad::new)
							.observe_caps(broadcast, catalog, &caps, reservation.requested())
					})
				};
				if let Some(track) = reserved {
					reservation.commit(track);
				}
				gst::Pad::event_default(pad, Some(&*self.obj()), event)
			}
			gst::EventView::Segment(segment) => {
				if let Some(state) = self.state.lock().unwrap().as_mut() {
					state
						.pads
						.entry(pad.name().to_string())
						.or_insert_with(Pad::new)
						.observe_segment(segment.segment().to_owned());
				}
				drop(reservation);
				gst::Pad::event_default(pad, Some(&*self.obj()), event)
			}
			gst::EventView::Eos(_) => {
				self.handle_eos(pad);
				drop(reservation);
				gst::Pad::event_default(pad, Some(&*self.obj()), event)
			}
			// FLUSH_STOP re-anchors the timeline; the trailing SEGMENT is accepted fresh. The producer is
			// kept (FLUSH is not EOS).
			gst::EventView::FlushStop(_) => {
				if let Some(state) = self.state.lock().unwrap().as_mut()
					&& let Some(media) = state.pads.get_mut(pad.name().as_str())
				{
					media.flush();
				}
				drop(reservation);
				gst::Pad::event_default(pad, Some(&*self.obj()), event)
			}
			_ => {
				let handled = gst::Pad::event_default(pad, Some(&*self.obj()), event);
				drop(reservation);
				handled
			}
		}
	}

	/// Mark a pad ended, then post the element EOS if that was the last active pad.
	fn handle_eos(&self, pad: &gst::Pad) {
		if let Some(state) = self.state.lock().unwrap().as_mut() {
			state.ended.insert(pad.name().to_string());
		}
		self.maybe_post_eos();
	}

	/// Finalize and post the element EOS once every active sink pad has ended. Locks internally and is
	/// idempotent via `eos_posted`, so both the EOS handler and `release_pad` (releasing the last active
	/// pad can satisfy aggregation for pads that already ended) can call it.
	fn maybe_post_eos(&self) {
		let _rt = RUNTIME.enter();
		let mut guard = self.state.lock().unwrap();
		let Some(state) = guard.as_mut() else {
			return;
		};
		let sink_pads = self.obj().sink_pads();
		let all_ended = !sink_pads.is_empty() && sink_pads.iter().all(|p| state.ended.contains(p.name().as_str()));
		if !all_ended || state.eos_posted {
			return;
		}
		state.eos_posted = true;
		let result = state.finalize_all();
		drop(guard);

		match result {
			Ok(order) => {
				gst::debug!(CAT, "finalized on EOS: {order:?}");
				gst::info!(CAT, "all pads ended, posting EOS");
				let obj = self.obj();
				let _ = obj.post_message(gst::message::Eos::builder().src(&*obj).build());
			}
			Err(err) => {
				gst::element_error!(self.obj(), gst::CoreError::Failed, ("finalize failed"), ["{err:?}"]);
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sink() -> super::super::MoqSink {
		glib::Object::builder::<super::super::MoqSink>().build()
	}

	fn spec(element: &super::super::MoqSink, name: &str) -> glib::ParamSpec {
		element.find_property(name).unwrap()
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
