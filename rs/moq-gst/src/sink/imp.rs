//! GObject shell for the moqsink element, built on GstAggregator.
//!
//! Aggregator owns the per-pad input queues, FLUSH/EOS/SEGMENT handling, and the single aggregate
//! thread. The element just drains buffers in `aggregate` and writes them synchronously into the moq
//! producers. There is no data channel, no backpressure cancellation, and no per-pad generation
//! bookkeeping: serialized events and buffers for a pad arrive in order on one thread.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

use anyhow::{Context, Result};
use bytes::Bytes;
use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gst_base::prelude::*;
use gst_base::subclass::prelude::*;
use hang::moq_net;

use super::pad::{caps_supported, Pad};
use super::session::{ResolvedSettings, Session, CAT, RUNTIME};

/// Reject a frame past the MoQ frame limit (moq-net's MAX_FRAME_SIZE, 16 MiB): it could not be
/// consumed anyway, and copying it would let hostile input drive an unbounded allocation.
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

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

#[derive(Default)]
pub struct MoqSink {
	settings: Mutex<Settings>,
	/// The live session (connect task + status). Replaced on every start, so the property getters and
	/// the aggregate thread always read the current one.
	session: Mutex<Option<Session>>,
	/// Producers written from the aggregate thread. Created in `start`, dropped in `stop`.
	broadcast: Mutex<Option<moq_net::BroadcastProducer>>,
	catalog: Mutex<Option<moq_mux::catalog::Producer>>,
	/// Per-pad media state, keyed by pad name. Touched only by the aggregate thread, `flush`, and pad
	/// release, all serialized by this mutex.
	pads: Mutex<HashMap<String, Pad>>,
	/// Guards against posting EOS more than once.
	eos_posted: AtomicBool,
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
				let session = self.session.lock().unwrap();
				let status = session.as_ref().map(Session::status);
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
				"Ariel Molina <ariel@edis.mx>",
			)
		});
		Some(&*METADATA)
	}

	fn pad_templates() -> &'static [gst::PadTemplate] {
		static PAD_TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
			// Every codec that converges on moq_mux::import::Framed; per-codec specifics are validated
			// when the producer is built from caps, not here.
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
		if let Some(mut media) = self.pads.lock().unwrap().remove(pad.name().as_str()) {
			if let Err(err) = media.finalize() {
				gst::warning!(CAT, "finalize on release {}: {err:?}", pad.name());
			}
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
		*self.session.lock().unwrap() = Some(session);
		*self.broadcast.lock().unwrap() = Some(broadcast);
		*self.catalog.lock().unwrap() = Some(catalog);
		self.eos_posted.store(false, Ordering::Relaxed);
		Ok(())
	}

	/// Finalize the producers (catalog last) and tear down the session. The src task is already stopped
	/// here, so there is no race with `aggregate`.
	fn stop(&self) -> Result<(), gst::ErrorMessage> {
		{
			let _rt = RUNTIME.enter();
			if let Err(err) = self.finalize_all() {
				gst::warning!(CAT, "finalize on stop: {err:?}");
			}
		}
		self.broadcast.lock().unwrap().take();
		self.pads.lock().unwrap().clear();
		if let Some(session) = self.session.lock().unwrap().take() {
			session.stop();
		}
		Ok(())
	}

	/// Drain every ready pad and write its buffers into the producers. Returns EOS once every pad has
	/// ended, or an error if the session died.
	fn aggregate(&self, _timeout: bool) -> Result<gst::FlowSuccess, gst::FlowError> {
		if self.session.lock().unwrap().as_ref().is_some_and(Session::errored) {
			return Err(gst::FlowError::Error);
		}

		// Producer writes can touch tokio time (group eviction), so hold the runtime context here.
		let _rt = RUNTIME.enter();
		let broadcast = self.broadcast.lock().unwrap().clone();
		let catalog = self.catalog.lock().unwrap().clone();

		let sink_pads = self.obj().sink_pads();
		let mut all_ended = !sink_pads.is_empty();
		for pad in &sink_pads {
			let agg_pad = pad
				.downcast_ref::<gst_base::AggregatorPad>()
				.expect("sink pad is an AggregatorPad");
			while let Some(buffer) = agg_pad.pop_buffer() {
				self.write_buffer(agg_pad, broadcast.as_ref(), catalog.as_ref(), buffer);
			}
			if !agg_pad.is_eos() {
				all_ended = false;
			}
		}

		if all_ended {
			self.complete();
			return Err(gst::FlowError::Eos);
		}
		Ok(gst::FlowSuccess::Ok)
	}

	/// Reject unsupported caps synchronously so negotiation fails fast; everything else (SEGMENT, EOS,
	/// caps storage) goes to the parent, which queues serialized events for the aggregate thread.
	fn sink_event(&self, pad: &gst_base::AggregatorPad, event: gst::Event) -> bool {
		if let gst::EventView::Caps(caps) = event.view() {
			if !caps_supported(caps.caps()) {
				gst::warning!(CAT, "rejecting unsupported caps on pad {}", pad.name());
				return false;
			}
		}
		self.parent_sink_event(pad, event)
	}

	/// FLUSH re-anchors every pad's timeline so post-flush frames are not dropped as regressions. The
	/// producers are kept (FLUSH is not EOS).
	fn flush(&self) -> Result<gst::FlowSuccess, gst::FlowError> {
		for pad in self.pads.lock().unwrap().values_mut() {
			pad.flush();
		}
		self.parent_flush()
	}
}

impl MoqSink {
	/// Import one popped buffer into its pad's producer, building the producer lazily from the pad's
	/// current caps. Every failure path is per-pad (drop the buffer); none tears down the session.
	fn write_buffer(
		&self,
		agg_pad: &gst_base::AggregatorPad,
		broadcast: Option<&moq_net::BroadcastProducer>,
		catalog: Option<&moq_mux::catalog::Producer>,
		buffer: gst::Buffer,
	) {
		// Bound the per-frame allocation before copying.
		if buffer.size() > MAX_FRAME_BYTES {
			gst::warning!(
				CAT,
				"rejecting {}-byte buffer on pad {} (exceeds frame limit)",
				buffer.size(),
				agg_pad.name()
			);
			return;
		}
		let (Some(broadcast), Some(catalog)) = (broadcast, catalog) else {
			return; // No session producers (not started); drop.
		};

		let mut pads = self.pads.lock().unwrap();
		let pad = pads.entry(agg_pad.name().to_string()).or_insert_with(Pad::new);
		if pad.is_failed() {
			return;
		}
		if let Some(caps) = agg_pad.current_caps() {
			pad.observe_caps(broadcast, catalog, &caps);
		}
		pad.observe_segment(agg_pad.segment());

		let Ok(map) = buffer.map_readable() else {
			gst::warning!(CAT, "failed to map buffer on pad {}", agg_pad.name());
			return;
		};
		let data = Bytes::copy_from_slice(map.as_slice());
		pad.push_buffer(data, buffer.pts());
	}

	/// Finalize every live producer once, catalog last; runs on EOS and on stop. Idempotent: a
	/// finalized pad returns `Ok(false)` and the catalog is taken on the first call.
	fn finalize_all(&self) -> Result<Vec<String>> {
		let mut order = Vec::new();
		{
			let mut pads = self.pads.lock().unwrap();
			for (name, pad) in pads.iter_mut() {
				match pad.finalize() {
					Ok(true) => order.push(name.clone()),
					Ok(false) => {}
					Err(err) => gst::warning!(CAT, "finalize {name}: {err:?}"),
				}
			}
		}
		// finish() closes both the hang and MSF tracks; a bare drop would not.
		if let Some(mut catalog) = self.catalog.lock().unwrap().take() {
			catalog.finish().context("finalize catalog")?;
			order.push("catalog".to_string());
		}
		Ok(order)
	}

	/// Finalize on EOS and post the element EOS once the catalog closed cleanly; a finalize failure
	/// surfaces as an element error instead.
	fn complete(&self) {
		if self.eos_posted.swap(true, Ordering::Relaxed) {
			return;
		}
		match self.finalize_all() {
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
