//! GObject shell, deliberately a parallel element (`moqsinkspike`) so production `moqsink` is untouched.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use anyhow::{Context, Result};
use bytes::Bytes;
use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use tokio::sync::watch;

use super::session::{
	caps_supported, send_or_flush, DataMsg, FlushSignal, ResolvedSettings, SendOutcome, SessionHandle, Status, CAT,
};

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
pub struct MoqSinkSpike {
	settings: Mutex<Settings>,
	session: Mutex<Option<SessionHandle>>,
	status: Arc<Status>,
	// Monotonic per-pad generation; a pad recreated with the same name gets a fresh value so the
	// worker discards the previous incarnation's in-flight messages.
	next_generation: AtomicU64,
	pad_generations: Mutex<HashMap<String, u64>>,
	// Per-pad FLUSH gate: FLUSH_START flips it true to cut a blocked send (the chain holds a cloned
	// receiver); FLUSH_STOP flips it false. Per-pad so flushing one input never cancels another's send.
	// Keyed with the generation so a stale FLUSH from a previous incarnation cannot flip the live pad.
	pad_flush: Mutex<HashMap<String, (u64, watch::Sender<bool>)>>,
}

#[glib::object_subclass]
impl ObjectSubclass for MoqSinkSpike {
	const NAME: &'static str = "MoqSinkSpike";
	type Type = super::MoqSinkSpike;
	type ParentType = gst::Element;
}

impl ObjectImpl for MoqSinkSpike {
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
				// Read-only, served from the shared Status the task writes.
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
			"connected" => self.status.connected().to_value(),
			"moq-version" => self.status.version().to_value(),
			"estimated-send-bitrate" => self.status.send_bitrate().to_value(),
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

impl GstObjectImpl for MoqSinkSpike {}

impl ElementImpl for MoqSinkSpike {
	fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
		static METADATA: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
			gst::subclass::ElementMetadata::new(
				"MoQ Sink (spike)",
				"Sink/Network/MoQ",
				"Transmits media over MoQ (spike)",
				"Ariel Molina <ariel@edis.mx>",
			)
		});
		Some(&*METADATA)
	}

	fn pad_templates() -> &'static [gst::PadTemplate] {
		static PAD_TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
			// Every codec that converges on moq_mux::import::Framed; per-codec specifics are validated
			// when the producer is built from caps, not here.
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
			caps.merge(gst::Caps::builder("audio/x-opus").build());

			let templ =
				gst::PadTemplate::new("sink_%u", gst::PadDirection::Sink, gst::PadPresence::Request, &caps).unwrap();
			vec![templ]
		});
		PAD_TEMPLATES.as_ref()
	}

	fn request_new_pad(
		&self,
		templ: &gst::PadTemplate,
		name: Option<&str>,
		_caps: Option<&gst::Caps>,
	) -> Option<gst::Pad> {
		// Fixed per pad incarnation and captured here, so a buffer in flight from a released pad never
		// reads a successor's generation.
		let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
		// One flush watch per pad: the sender lives in `pad_flush` (toggled by FLUSH_START/STOP), each
		// pad function carries its own cloned receiver to cancel a blocked send.
		let (flush_tx, flush_rx) = watch::channel(false);
		let chain_flush = flush_rx.clone();
		let event_flush = flush_rx;
		let pad_builder = gst::Pad::builder_from_template(templ)
			.chain_function(move |pad, parent, buffer| {
				let mut flush = chain_flush.clone();
				MoqSinkSpike::catch_panic_pad_function(
					parent,
					|| Err(gst::FlowError::Error),
					|sink| sink.forward_buffer(pad, generation, &mut flush, buffer),
				)
			})
			.event_function(move |pad, parent, event| {
				let mut flush = event_flush.clone();
				MoqSinkSpike::catch_panic_pad_function(
					parent,
					|| false,
					|sink| sink.handle_event(pad, generation, &mut flush, event),
				)
			});

		let pad = match name {
			Some(name) => pad_builder.name(name).build(),
			None => pad_builder.generated_name().build(),
		};

		// Populate the maps BEFORE the pad is visible to GStreamer, so a concurrent start_session seed
		// never sees a pad without its generation. Capture the previous holders so a failed add_pad (a
		// duplicate name, or a concurrent request that lost the race) rolls back without orphaning the
		// live pad, and announce AddPad only after add_pad succeeds so the worker never gets a phantom.
		//
		// Known limitation (documented, not closed): two concurrent requests for the SAME name racing a
		// start_session seed can leave the seed reading the loser's generation for the winner's pad, so
		// the winner's events are then dropped as stale. Closing it would mean holding a lock across
		// add_pad, which emits pad-added synchronously and would deadlock a reentrant handler; that risk
		// is worse than the bug, whose trigger (same-name concurrent request_new_pad) apps do not produce.
		let name = pad.name().to_string();
		let prev_gen = self.pad_generations.lock().unwrap().insert(name.clone(), generation);
		let prev_flush = self
			.pad_flush
			.lock()
			.unwrap()
			.insert(name.clone(), (generation, flush_tx));

		if self.obj().add_pad(&pad).is_err() {
			// Roll back, but only if this attempt still owns the entry. A concurrent same-name request
			// that won add_pad may have overwritten it; restoring our captured `prev` (or removing) would
			// then clobber the live pad it just registered. Touch the maps only while they still hold our
			// own generation.
			{
				let mut gens = self.pad_generations.lock().unwrap();
				if gens.get(name.as_str()) == Some(&generation) {
					match prev_gen {
						Some(g) => gens.insert(name.clone(), g),
						None => gens.remove(&name),
					};
				}
			}
			{
				let mut flushes = self.pad_flush.lock().unwrap();
				if flushes.get(name.as_str()).map(|(g, _)| *g) == Some(generation) {
					match prev_flush {
						Some(f) => flushes.insert(name.clone(), f),
						None => flushes.remove(&name),
					};
				}
			}
			return None;
		}

		// A request pad is linked by the caller only after this returns, so its CAPS/buffers cannot reach
		// the worker ahead of this AddPad.
		let sender = self.session.lock().unwrap().as_ref().map(SessionHandle::sender);
		if let Some(sender) = sender {
			let _ = sender.blocking_send(DataMsg::AddPad { pad: name, generation });
		}
		Some(pad)
	}

	fn release_pad(&self, pad: &gst::Pad) {
		let name = pad.name().to_string();
		let generation = self.pad_generations.lock().unwrap().remove(&name);
		// Dropping the watch sender wakes any send still blocked on this pad (changed() errors -> Flushing).
		self.pad_flush.lock().unwrap().remove(&name);
		// Drop the session guard before blocking_send: holding it across a full-channel block deadlocks stop_session.
		let sender = {
			let session = self.session.lock().unwrap();
			session.as_ref().map(SessionHandle::sender)
		};
		if let (Some(sender), Some(generation)) = (sender, generation) {
			// Uncancellable: a full data channel or an in-progress connect stalls release here, and the
			// per-pad flush watch (removed just above) cannot cut it. Known control-path-blocking limit,
			// shared with the AddPad send; out of scope for FLUSH.
			let _ = sender.blocking_send(DataMsg::DropPad { pad: name, generation });
		}
		let _ = self.obj().remove_pad(pad);
	}

	fn change_state(&self, transition: gst::StateChange) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
		match transition {
			gst::StateChange::ReadyToPaused => {
				self.start_session().map_err(|err| {
					gst::error!(CAT, obj = self.obj(), "failed to start session: {err:#}");
					gst::StateChangeError
				})?;
			}
			gst::StateChange::PausedToReady => self.stop_session(),
			_ => (),
		}

		self.parent_change_state(transition)
	}
}

impl MoqSinkSpike {
	fn start_session(&self) -> Result<()> {
		// Synchronous settings validation surfaces as a StateChangeError; async failures go to the bus.
		let settings = ResolvedSettings::try_from(self.settings.lock().unwrap().clone())?;
		// Seed pads requested before the session existed; the data channel is created inside start().
		let seed = {
			let gens = self.pad_generations.lock().unwrap();
			self.obj()
				.pads()
				.iter()
				.map(|pad| {
					let name = pad.name().to_string();
					let generation = gens.get(&name).copied().unwrap_or(0);
					(name, generation)
				})
				.collect()
		};
		let handle = SessionHandle::start(settings, self.status.clone(), self.obj().downgrade(), seed);
		*self.session.lock().unwrap() = Some(handle);
		Ok(())
	}

	fn stop_session(&self) {
		if let Some(handle) = self.session.lock().unwrap().take() {
			handle.stop();
		}
	}

	/// Clone the sender, release the lock, then blocking-send: never apply backpressure under the session lock.
	fn forward_buffer(
		&self,
		pad: &gst::Pad,
		generation: u64,
		flush: &mut watch::Receiver<bool>,
		buffer: gst::Buffer,
	) -> Result<gst::FlowSuccess, gst::FlowError> {
		// The worker marks a pad failed after rejecting its data; surface that to GStreamer instead of
		// silently dropping. Because the worker is async, this lands on the buffer after the bad one.
		if self.status.is_failed(pad.name().as_str(), generation) {
			return Err(gst::FlowError::NotNegotiated);
		}

		// Bound the per-frame allocation before copying: a buffer past the frame limit cannot be
		// consumed and would let hostile input drive an unbounded copy.
		if buffer.size() > MAX_FRAME_BYTES {
			gst::warning!(
				CAT,
				"rejecting {}-byte buffer on pad {} (exceeds frame limit)",
				buffer.size(),
				pad.name()
			);
			return Err(gst::FlowError::Error);
		}

		// Skip the map + copy if the pad is already flushing; send_or_flush re-checks for a flush that
		// starts mid-send, but during a flush repeated buffers would otherwise burn a copy each before
		// being dropped.
		if *flush.borrow() {
			return Err(gst::FlowError::Flushing);
		}

		let sender = self
			.session
			.lock()
			.unwrap()
			.as_ref()
			.map(|handle| handle.sender())
			.ok_or(gst::FlowError::Flushing)?;

		let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
		let data = Bytes::copy_from_slice(map.as_slice());
		let pts = buffer.pts();

		let msg = DataMsg::Buffer {
			pad: pad.name().to_string(),
			generation,
			data,
			pts,
		};
		// FLUSH_START on this pad cuts a send blocked on a full channel (returns Flushing), instead of
		// stalling the streaming thread until the relay drains.
		match send_or_flush(&sender, msg, flush) {
			SendOutcome::Sent => Ok(gst::FlowSuccess::Ok),
			SendOutcome::Flushed | SendOutcome::Closed => Err(gst::FlowError::Flushing),
		}
	}

	fn handle_event(
		&self,
		pad: &gst::Pad,
		generation: u64,
		flush: &mut watch::Receiver<bool>,
		event: gst::Event,
	) -> bool {
		let sender = self.session.lock().unwrap().as_ref().map(|handle| handle.sender());

		match event.view() {
			gst::EventView::Caps(caps) => {
				let caps = caps.caps();
				// Reject unsupported caps synchronously (NotNegotiated) before handing off to the worker.
				if !caps_supported(caps) {
					gst::warning!(CAT, "rejecting unsupported caps on pad {}", pad.name());
					return false;
				}
				let Some(sender) = sender else { return false };
				let msg = DataMsg::Caps {
					pad: pad.name().to_string(),
					generation,
					caps: caps.to_owned(),
				};
				match send_or_flush(&sender, msg, flush) {
					SendOutcome::Sent => gst::Pad::event_default(pad, Some(&*self.obj()), event),
					SendOutcome::Flushed | SendOutcome::Closed => false,
				}
			}
			gst::EventView::Segment(segment) => {
				let Some(sender) = sender else { return false };
				let msg = DataMsg::Segment {
					pad: pad.name().to_string(),
					generation,
					segment: segment.segment().to_owned(),
				};
				match send_or_flush(&sender, msg, flush) {
					SendOutcome::Sent => gst::Pad::event_default(pad, Some(&*self.obj()), event),
					SendOutcome::Flushed | SendOutcome::Closed => false,
				}
			}
			gst::EventView::Eos(_) => {
				let Some(sender) = sender else { return false };
				let msg = DataMsg::Eos {
					pad: pad.name().to_string(),
					generation,
				};
				matches!(send_or_flush(&sender, msg, flush), SendOutcome::Sent)
			}
			// FLUSH_START arrives out of band on the flushing thread: flip this pad's watch to cut a send
			// blocked in the chain, and tell the worker to re-anchor the timeline. Never blocks.
			gst::EventView::FlushStart(_) => {
				toggle_pad_flush(&self.pad_flush.lock().unwrap(), pad.name().as_str(), generation, true);
				if let Some(handle) = self.session.lock().unwrap().as_ref() {
					let _ = handle.flush_sender().send(FlushSignal {
						pad: pad.name().to_string(),
						generation,
					});
				}
				gst::Pad::event_default(pad, Some(&*self.obj()), event)
			}
			// FLUSH_STOP clears the gate so the chain resumes; the trailing SEGMENT re-anchors the worker.
			gst::EventView::FlushStop(_) => {
				toggle_pad_flush(&self.pad_flush.lock().unwrap(), pad.name().as_str(), generation, false);
				gst::Pad::event_default(pad, Some(&*self.obj()), event)
			}
			_ => gst::Pad::event_default(pad, Some(&*self.obj()), event),
		}
	}
}

// Flip a pad's FLUSH watch only when the generation matches, so a stale FLUSH from a previous
// incarnation cannot cancel the live pad's sends. Mirrors the worker's generation discipline.
fn toggle_pad_flush(flush: &HashMap<String, (u64, watch::Sender<bool>)>, name: &str, generation: u64, value: bool) {
	if let Some((gen, tx)) = flush.get(name) {
		if *gen == generation {
			let _ = tx.send(value);
		}
	}
}

#[cfg(test)]
mod tests {
	use std::collections::HashMap;

	use tokio::sync::watch;

	use super::toggle_pad_flush;

	#[test]
	fn pad_flush_toggle_ignores_stale_generation() {
		let mut map = HashMap::new();
		let (tx, rx) = watch::channel(false);
		map.insert("video".to_string(), (1u64, tx));
		// A stale generation must not flip the live pad's watch.
		toggle_pad_flush(&map, "video", 0, true);
		assert!(!*rx.borrow(), "stale-generation flush is ignored");
		// The current generation flips it.
		toggle_pad_flush(&map, "video", 1, true);
		assert!(*rx.borrow(), "current-generation flush flips the watch");
	}

	// A failed pad add must leave membership untouched. request_new_pad adds the pad before announcing
	// AddPad or mutating the maps, so a duplicate name (or a concurrent request that lost the race) that
	// fails add_pad does not corrupt the live pad: its generation and flush sender survive. Announcing
	// for a name already held would otherwise make the worker finalize the live pad's producer and
	// overwrite its generation. Exercised via two direct request_new_pad calls (the concurrent path).
	#[test]
	fn failed_duplicate_pad_keeps_membership_consistent() {
		use gst::prelude::*;
		use gst::subclass::prelude::*;
		gst::init().unwrap();

		let obj = gst::glib::Object::new::<super::super::MoqSinkSpike>();
		let imp = obj.imp();
		let templ = obj.pad_template("sink_%u").expect("sink template");

		let p0 = imp.request_new_pad(&templ, Some("sink_0"), None);
		let p1 = imp.request_new_pad(&templ, Some("sink_0"), None);
		assert!(p0.is_some(), "first request succeeds");
		assert!(p1.is_none(), "duplicate name fails to add");

		let gen = imp.pad_generations.lock().unwrap().get("sink_0").copied();
		let flush_gen = imp.pad_flush.lock().unwrap().get("sink_0").map(|(g, _)| *g);
		// After the failed duplicate the maps must still describe the LIVE pad (generation 0).
		assert_eq!(
			gen,
			Some(0),
			"live pad's generation must survive a failed duplicate (got {gen:?})"
		);
		assert_eq!(
			flush_gen,
			Some(0),
			"live pad's flush sender must survive a failed duplicate (got {flush_gen:?})"
		);
	}
}
