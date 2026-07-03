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
use super::session::{CAT, RUNTIME, ResolvedSettings, Session};

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
	broadcast: moq_net::BroadcastProducer,
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
}

impl ObjectImpl for MoqSink {
	fn properties() -> &'static [glib::ParamSpec] {
		static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
			vec![
				glib::ParamSpecString::builder("url")
					.nick("Source URL")
					.blurb("Connect to the given URL")
					.build(),
				glib::ParamSpecString::builder("broadcast")
					.nick("Broadcast")
					.blurb("The name of the broadcast to publish")
					.build(),
				glib::ParamSpecBoolean::builder("tls-disable-verify")
					.nick("TLS disable verify")
					.blurb("Disable TLS verification")
					.default_value(false)
					.build(),
				// Read-only, served from the live session's status.
				glib::ParamSpecBoolean::builder("connected")
					.nick("Connected")
					.blurb("Whether the session is currently connected")
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
		match pspec.name() {
			"connected" | "moq-version" | "estimated-send-bitrate" => {
				let state = self.state.lock().unwrap();
				let status = state.as_ref().map(|s| s.session.status());
				match pspec.name() {
					"connected" => status.is_some_and(|s| s.connected()).to_value(),
					"moq-version" => status.and_then(|s| s.version()).to_value(),
					"estimated-send-bitrate" => status.map(|s| s.send_bitrate()).unwrap_or(0).to_value(),
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
		let pad_builder = gst::Pad::builder_from_template(templ)
			.chain_function(|pad, parent, buffer| {
				MoqSink::catch_panic_pad_function(
					parent,
					|| Err(gst::FlowError::Error),
					|this| this.forward_buffer(pad, buffer),
				)
			})
			.event_function(|pad, parent, event| {
				MoqSink::catch_panic_pad_function(parent, || false, |this| this.handle_event(pad, event))
			});

		let pad = match name {
			Some(name) => pad_builder.name(name).build(),
			None => pad_builder.generated_name().build(),
		};
		self.obj().add_pad(&pad).ok()?;
		Some(pad)
	}

	fn release_pad(&self, pad: &gst::Pad) {
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
		let _ = self.obj().remove_pad(pad);
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
		// Drop the broadcast (closing it) before reaping the session task.
		drop(state.broadcast);
		state.session.stop();
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
		match event.view() {
			gst::EventView::Caps(caps) => {
				let caps = caps.caps().to_owned();
				// Reject unsupported caps synchronously (NotNegotiated) before building a producer.
				if !caps_supported(&caps) {
					gst::warning!(CAT, "rejecting unsupported caps on pad {}", pad.name());
					return false;
				}
				let _rt = RUNTIME.enter();
				if let Some(state) = self.state.lock().unwrap().as_mut() {
					let State {
						broadcast,
						catalog,
						pads,
						..
					} = state;
					if let Some(catalog) = catalog.as_ref() {
						pads.entry(pad.name().to_string())
							.or_insert_with(Pad::new)
							.observe_caps(broadcast, catalog, &caps);
					}
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
				gst::Pad::event_default(pad, Some(&*self.obj()), event)
			}
			gst::EventView::Eos(_) => {
				self.handle_eos(pad);
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
				gst::Pad::event_default(pad, Some(&*self.obj()), event)
			}
			_ => gst::Pad::event_default(pad, Some(&*self.obj()), event),
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
