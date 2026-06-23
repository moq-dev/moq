//! GObject shell for the moqsink element, built on GstAggregator.
//!
//! Aggregator owns the per-pad input queues, FLUSH/EOS/SEGMENT handling, and the single aggregate
//! thread. The element just drains buffers in `aggregate` and writes them synchronously into the moq
//! producers. There is no data channel, no backpressure cancellation, and no per-pad generation
//! bookkeeping: serialized events and buffers for a pad arrive in order on one thread.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use anyhow::{Context, Result};
use bytes::Bytes;
use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gst_base::prelude::*;
use gst_base::subclass::prelude::*;
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

/// Everything that lives only while the element is started, written from the aggregate thread. The
/// producers are created up front (so frames buffered before connect are sent once it completes); the
/// catalog is `Option` because it is taken on the first finalize. Bundled so the aggregate thread takes
/// one lock per buffer.
struct State {
	session: Session,
	broadcast: moq_net::BroadcastProducer,
	catalog: Option<moq_mux::catalog::Producer>,
	pads: HashMap<String, Pad>,
	/// Set once the element has posted EOS, so completion is idempotent.
	eos_posted: bool,
}

impl State {
	/// Import one popped buffer into its pad's producer, building the producer lazily from the pad's
	/// current caps. Per-pad failures (bad caps/bitstream, oversized frame) drop quietly; only a buffer we
	/// cannot even map is surfaced as an error. Returns `true` the first time the pad drops a buffer for
	/// lack of a TIME segment, so the caller can surface that once on the bus.
	fn write_buffer(&mut self, agg_pad: &gst_base::AggregatorPad, buffer: gst::Buffer) -> Result<bool, gst::FlowError> {
		let name = agg_pad.name();
		let caps = agg_pad.current_caps();
		let segment = agg_pad.segment();
		let pts = buffer.pts();

		// Disjoint field borrows: the pad entry borrows `pads` while observe_caps reads broadcast/catalog.
		let Self {
			broadcast,
			catalog,
			pads,
			..
		} = self;
		let pad = pads.entry(name.to_string()).or_insert_with(Pad::new);
		// Check failure before mapping/copying: a failed pad must not pay the copy, nor be able to fail
		// the whole element on an unmappable buffer it would have dropped anyway.
		if pad.is_failed() {
			return Ok(false);
		}

		let map = buffer.map_readable().map_err(|_| {
			gst::error!(CAT, "failed to map buffer on pad {name}");
			gst::FlowError::Error
		})?;
		let data = Bytes::copy_from_slice(map.as_slice());

		if let (Some(caps), Some(catalog)) = (caps.as_ref(), catalog.as_ref()) {
			pad.observe_caps(broadcast, catalog, caps);
		}
		pad.observe_segment(segment);
		Ok(pad.push_buffer(data, pts))
	}

	/// Finalize every live producer once, catalog last; runs on EOS and on stop. Idempotent: a finalized
	/// pad returns `Ok(false)` and the catalog is taken on the first call. A pad finalize error is logged
	/// (the others still finalize) and surfaced as the returned `Err`; a catalog error takes precedence.
	fn finalize_all(&mut self) -> Result<Vec<String>> {
		let mut order = Vec::new();
		let mut failure = None;
		for (name, pad) in self.pads.iter_mut() {
			match pad.finalize() {
				Ok(true) => order.push(name.clone()),
				Ok(false) => {}
				Err(err) => {
					gst::warning!(CAT, "finalize {name}: {err:?}");
					failure.get_or_insert(err);
				}
			}
		}
		// finish() closes both the hang and MSF tracks; a bare drop would not.
		if let Some(mut catalog) = self.catalog.take() {
			catalog.finish().context("finalize catalog")?;
			order.push("catalog".to_string());
		}
		match failure {
			Some(err) => Err(err),
			None => Ok(order),
		}
	}
}

/// The `moqsink` element implementation: its GObject properties plus the live session state.
#[derive(Default)]
pub struct MoqSink {
	settings: Mutex<Settings>,
	/// Live session state, present only between start() and stop(). One Mutex, not Arc<Mutex>: glib
	/// already owns and shares the subclass instance across GStreamer's threads (the aggregate task,
	/// property reads, state changes), so we need interior mutability but not a second ownership layer.
	state: Mutex<Option<State>>,
}

#[glib::object_subclass]
impl ObjectSubclass for MoqSink {
	const NAME: &'static str = "MoqSink";
	type Type = super::MoqSink;
	type ParentType = gst_base::Aggregator;
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
			// Every codec that converges on moq_mux::import::Framed. The structural fields here
			// (byte-stream/au, AAC mpegversion/stream-format) are what negotiation enforces, so the
			// producer build does not re-check them.
			let mut sink_caps = gst::Caps::new_empty();
			sink_caps.merge(
				gst::Caps::builder("video/x-h264")
					.field("stream-format", "byte-stream")
					.field("alignment", "au")
					.build(),
			);
			sink_caps.merge(
				gst::Caps::builder("video/x-h265")
					.field("stream-format", "byte-stream")
					.field("alignment", "au")
					.build(),
			);
			sink_caps.merge(gst::Caps::builder("video/x-av1").build());
			sink_caps.merge(gst::Caps::builder("video/x-vp8").build());
			sink_caps.merge(gst::Caps::builder("video/x-vp9").build());
			sink_caps.merge(
				gst::Caps::builder("audio/mpeg")
					.field("mpegversion", 4i32)
					.field("stream-format", "raw")
					.build(),
			);
			sink_caps.merge(gst::Caps::builder("audio/x-opus").build());

			// Aggregator requires a src pad template named "src"; we never push on it (this is a sink), so
			// its caps are unconstrained.
			let src = gst::PadTemplate::with_gtype(
				"src",
				gst::PadDirection::Src,
				gst::PadPresence::Always,
				&gst::Caps::new_any(),
				gst_base::AggregatorPad::static_type(),
			)
			.unwrap();
			let sink = gst::PadTemplate::with_gtype(
				"sink_%u",
				gst::PadDirection::Sink,
				gst::PadPresence::Request,
				&sink_caps,
				gst_base::AggregatorPad::static_type(),
			)
			.unwrap();
			vec![src, sink]
		});
		PAD_TEMPLATES.as_ref()
	}

	/// Finalize and forget a released pad's producer. A release is reconfiguration, not EOS, so it never
	/// completes the element. Done after the parent removes the pad, so the aggregate thread no longer
	/// sees it.
	fn release_pad(&self, pad: &gst::Pad) {
		self.parent_release_pad(pad);
		let _rt = RUNTIME.enter();
		if let Some(state) = self.state.lock().unwrap().as_mut()
			&& let Some(mut media) = state.pads.remove(pad.name().as_str())
			&& let Err(err) = media.finalize()
		{
			gst::warning!(CAT, "finalize on release {}: {err:?}", pad.name());
		}
	}
}

impl AggregatorImpl for MoqSink {
	/// Start the session and create the producers before any buffer flows.
	fn start(&self) -> Result<(), gst::ErrorMessage> {
		let settings = ResolvedSettings::try_from(self.settings.lock().unwrap().clone())
			.map_err(|err| gst::error_msg!(gst::CoreError::Failed, ["invalid settings: {err:#}"]))?;
		let (session, broadcast, catalog) = Session::start(settings, self.obj().downgrade())
			.map_err(|err| gst::error_msg!(gst::CoreError::Failed, ["failed to start session: {err:?}"]))?;
		*self.state.lock().unwrap() = Some(State {
			session,
			broadcast,
			catalog: Some(catalog),
			pads: HashMap::new(),
			eos_posted: false,
		});
		Ok(())
	}

	/// Finalize the producers (catalog last) and tear down the session. The src task is already stopped
	/// here, so there is no race with `aggregate`. Finalize is best-effort: we are tearing down regardless.
	fn stop(&self) -> Result<(), gst::ErrorMessage> {
		let Some(mut state) = self.state.lock().unwrap().take() else {
			return Ok(());
		};
		let _rt = RUNTIME.enter();
		if let Err(err) = state.finalize_all() {
			gst::warning!(CAT, "finalize on stop: {err:?}");
		}
		// Drop the broadcast (closing it) before reaping the session task.
		drop(state.broadcast);
		state.session.stop();
		Ok(())
	}

	/// Drain every ready pad and write its buffers into the producers. Returns EOS once every pad has
	/// ended, or an error if the session died or a buffer could not be handled.
	fn aggregate(&self, _timeout: bool) -> Result<gst::FlowSuccess, gst::FlowError> {
		// Producer writes can touch tokio time (group eviction), so hold the runtime context here.
		let _rt = RUNTIME.enter();
		let mut guard = self.state.lock().unwrap();
		let Some(state) = guard.as_mut() else {
			return Ok(gst::FlowSuccess::Ok);
		};
		if state.session.errored() {
			return Err(gst::FlowError::Error);
		}

		let sink_pads = self.obj().sink_pads();
		let mut all_ended = !sink_pads.is_empty();
		// Pads that produced buffers with no TIME segment; reported once each, off the lock.
		let mut no_segment_pads: Vec<glib::GString> = Vec::new();
		for pad in &sink_pads {
			let agg_pad = pad
				.downcast_ref::<gst_base::AggregatorPad>()
				.expect("sink pad is an AggregatorPad");
			while let Some(buffer) = agg_pad.pop_buffer() {
				if state.write_buffer(agg_pad, buffer)? {
					no_segment_pads.push(agg_pad.name());
				}
			}
			if !agg_pad.is_eos() {
				all_ended = false;
			}
		}

		// Finalize once if every pad has ended; capture the outcome to act on after releasing the lock.
		let eos = all_ended.then(|| {
			let result = state.finalize_all();
			let post = !state.eos_posted;
			state.eos_posted = true;
			(result, post)
		});
		drop(guard);

		// Strict timeline (no raw-PTS fallback): a pad with no TIME segment publishes nothing, so say so
		// once on the bus rather than dropping every buffer in silence.
		for pad in no_segment_pads {
			gst::element_warning!(
				self.obj(),
				gst::StreamError::Format,
				(
					"pad {} received buffers with no TIME segment; nothing is published for it",
					pad
				)
			);
		}

		let Some((result, post)) = eos else {
			return Ok(gst::FlowSuccess::Ok);
		};
		match result {
			Ok(order) => {
				gst::debug!(CAT, "finalized on EOS: {order:?}");
				if post {
					gst::info!(CAT, "all pads ended, posting EOS");
					let obj = self.obj();
					let _ = obj.post_message(gst::message::Eos::builder().src(&*obj).build());
				}
				Err(gst::FlowError::Eos)
			}
			Err(err) => {
				gst::element_error!(self.obj(), gst::CoreError::Failed, ("finalize failed"), ["{err:?}"]);
				Err(gst::FlowError::Error)
			}
		}
	}

	/// Reject unsupported caps synchronously so negotiation fails fast; everything else (SEGMENT, EOS,
	/// caps storage) goes to the parent, which queues serialized events for the aggregate thread.
	fn sink_event(&self, pad: &gst_base::AggregatorPad, event: gst::Event) -> bool {
		if let gst::EventView::Caps(caps) = event.view()
			&& !caps_supported(caps.caps())
		{
			gst::warning!(CAT, "rejecting unsupported caps on pad {}", pad.name());
			return false;
		}
		self.parent_sink_event(pad, event)
	}

	/// FLUSH re-anchors every pad's timeline so post-flush frames are not dropped as regressions. The
	/// producers are kept (FLUSH is not EOS).
	fn flush(&self) -> Result<gst::FlowSuccess, gst::FlowError> {
		if let Some(state) = self.state.lock().unwrap().as_mut() {
			for pad in state.pads.values_mut() {
				pad.flush();
			}
		}
		self.parent_flush()
	}
}
