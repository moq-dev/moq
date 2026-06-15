//! Async core: the session/pads/timeline seams, isolated from the GObject shell.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use anyhow::{ensure, Context, Result};
use bytes::Bytes;
use gst::glib;
use gst::prelude::*;
use tokio::sync::{mpsc, watch};

use hang::moq_net;

use super::timeline::{classify_segment, frame_micros, FrameDecision, SegmentDecision, SegmentInfo};
use super::MoqSinkSpike as Element;

static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
	tokio::runtime::Builder::new_multi_thread()
		.enable_all()
		.build()
		.expect("spawn tokio runtime")
});

pub static CAT: LazyLock<gst::DebugCategory> =
	LazyLock::new(|| gst::DebugCategory::new("moq-sink-spike", gst::DebugColorFlags::empty(), Some("MoQ Sink spike")));

/// Handoff, not a buffer: a full channel must block the streaming thread, not grow.
const DATA_CHANNEL_BOUND: usize = 8;

/// Read by the element's getters without touching the task; reset on every exit.
#[derive(Default)]
pub struct Status {
	connected: AtomicBool,
	version: Mutex<Option<String>>,
	send_bitrate: AtomicU64,
	/// Pads whose data the worker rejected; the chain reads this to return a FlowError instead of
	/// silently dropping. Cleared per pad on recreate, and wholesale on session exit.
	failed: Mutex<HashSet<String>>,
}

impl Status {
	fn set_connected(&self, value: bool) {
		self.connected.store(value, Ordering::Relaxed);
	}

	fn mark_failed(&self, pad: &str) {
		self.failed.lock().unwrap().insert(pad.to_string());
	}

	fn clear_failed(&self, pad: &str) {
		self.failed.lock().unwrap().remove(pad);
	}

	pub fn is_failed(&self, pad: &str) -> bool {
		self.failed.lock().unwrap().contains(pad)
	}

	fn reset_failed(&self) {
		self.failed.lock().unwrap().clear();
	}

	pub fn connected(&self) -> bool {
		self.connected.load(Ordering::Relaxed)
	}

	fn set_version(&self, value: Option<String>) {
		*self.version.lock().unwrap() = value;
	}

	pub fn version(&self) -> Option<String> {
		self.version.lock().unwrap().clone()
	}

	fn set_send_bitrate(&self, bits_per_sec: u64) {
		self.send_bitrate.store(bits_per_sec, Ordering::Relaxed);
	}

	pub fn send_bitrate(&self) -> u64 {
		self.send_bitrate.load(Ordering::Relaxed)
	}
}

/// Ordered; shutdown is deliberately elsewhere (cancellation channel) so it can cut a blocked send.
/// Every message carries its pad's generation so a pad recreated with the same name discards the
/// previous incarnation's in-flight messages.
pub enum DataMsg {
	AddPad {
		pad: String,
		generation: u64,
	},
	Caps {
		pad: String,
		generation: u64,
		caps: gst::Caps,
	},
	Segment {
		pad: String,
		generation: u64,
		segment: gst::Segment,
	},
	Buffer {
		pad: String,
		generation: u64,
		data: Bytes,
		pts: Option<gst::ClockTime>,
	},
	Eos {
		pad: String,
		generation: u64,
	},
	DropPad {
		pad: String,
		generation: u64,
	},
}

pub struct ResolvedSettings {
	pub url: url::Url,
	pub broadcast: String,
	pub tls_disable_verify: bool,
}

pub struct SessionHandle {
	data: mpsc::Sender<DataMsg>,
	shutdown: watch::Sender<bool>,
	join: tokio::task::JoinHandle<()>,
}

impl SessionHandle {
	pub fn start(
		settings: ResolvedSettings,
		status: Arc<Status>,
		element: glib::WeakRef<Element>,
		seed: Vec<(String, u64)>,
	) -> Self {
		let (data_tx, data_rx) = mpsc::channel(DATA_CHANNEL_BOUND);
		let (shutdown_tx, shutdown_rx) = watch::channel(false);

		let join = RUNTIME.spawn(async move {
			// Only a remote close reaches the bus as an error; a local shutdown returns Ok and stays quiet.
			if let Err(err) = run_session(settings, status, seed, data_rx, shutdown_rx, element.clone()).await {
				if let Some(obj) = element.upgrade() {
					gst::element_error!(obj, gst::CoreError::Failed, ("session error"), ["{err:?}"]);
				}
			}
		});

		Self {
			data: data_tx,
			shutdown: shutdown_tx,
			join,
		}
	}

	/// Cloned out so the element blocking-sends without holding the session lock (else a full channel deadlocks stop).
	pub fn sender(&self) -> mpsc::Sender<DataMsg> {
		self.data.clone()
	}

	pub fn stop(self) {
		// Cancel first so a send blocked on a full channel wakes via the dropped receiver; reap off-thread.
		let _ = self.shutdown.send(true);
		RUNTIME.spawn(async move {
			if let Err(err) = self.join.await {
				gst::warning!(CAT, "session task ended with error: {err:?}");
			}
		});
	}
}

async fn run_session(
	settings: ResolvedSettings,
	status: Arc<Status>,
	seed: Vec<(String, u64)>,
	mut data: mpsc::Receiver<DataMsg>,
	mut shutdown: watch::Receiver<bool>,
	element: glib::WeakRef<Element>,
) -> Result<()> {
	let mut config = moq_native::ClientConfig::default();
	config.tls.disable_verify = Some(settings.tls_disable_verify);
	let client = config.init()?;

	let origin = moq_net::Origin::random().produce();
	let mut broadcast = moq_net::Broadcast::new().produce();
	let broadcast_consumer = broadcast.consume();
	let catalog = moq_mux::catalog::Producer::new(&mut broadcast)?;
	ensure!(
		origin.publish_broadcast(&settings.broadcast, broadcast_consumer),
		"failed to publish broadcast {}",
		settings.broadcast
	);
	let client = client.with_publish(origin.consume());

	// Cancellation covers connect: a shutdown while connecting is a clean local close, not an error.
	let session = tokio::select! {
		result = client.connect(settings.url.clone()) => result?,
		_ = shutdown.changed() => return Ok(()),
	};
	status.set_connected(true);
	status.set_version(Some(session.version().to_string()));
	notify_connected(&element);
	gst::info!(CAT, "session connected to {}", settings.url);

	let mut pad_set = PadSet::new(broadcast, catalog, status.clone());
	// Pads requested before the session task existed are seeded into the authoritative set.
	for (name, generation) in seed {
		pad_set.add_pad(&name, generation);
	}
	let result = run_loop(session, &mut data, &mut shutdown, &mut pad_set, &element, &status).await;

	// Finalize every live producer once on the way out, catalog last; runs on every exit path.
	let finalized = pad_set.finalize_all();
	gst::debug!(CAT, "finalized on exit: {finalized:?}");
	// Reset the whole observable surface on exit, not just connected.
	status.set_connected(false);
	status.set_version(None);
	status.set_send_bitrate(0);
	status.reset_failed();
	notify_connected(&element);
	result
}

/// Only on the connect/disconnect edges, never per sample.
fn notify_connected(element: &glib::WeakRef<Element>) {
	if let Some(obj) = element.upgrade() {
		obj.notify("connected");
	}
}

/// Posted once when every active pad has ended.
fn post_element_eos(element: &glib::WeakRef<Element>) {
	gst::info!(CAT, "all pads ended, posting EOS");
	if let Some(obj) = element.upgrade() {
		let _ = obj.post_message(gst::message::Eos::builder().src(&obj).build());
	}
}

async fn run_loop(
	session: moq_net::Session,
	data: &mut mpsc::Receiver<DataMsg>,
	shutdown: &mut watch::Receiver<bool>,
	pad_set: &mut PadSet,
	element: &glib::WeakRef<Element>,
	status: &Status,
) -> Result<()> {
	// Congestion-controller send estimate; None when unavailable, then this arm parks forever.
	let mut send_bandwidth = session.send_bandwidth();

	// Resolves to Err when the transport dies; pinned so the select polls it each iteration.
	let closed = session.closed();
	tokio::pin!(closed);

	loop {
		tokio::select! {
			// Local close: quiet Ok, no ERROR.
			_ = shutdown.changed() => return Ok(()),
			// Remote death: propagate the Err so the wrapper posts ERROR to the bus.
			result = &mut closed => {
				result?;
				return Ok(());
			}
			// A closed estimate stops the polling.
			bitrate = async {
				match send_bandwidth.as_mut() {
					Some(bw) => bw.changed().await,
					None => std::future::pending::<Option<u64>>().await,
				}
			} => match bitrate {
				Some(rate) => status.set_send_bitrate(rate),
				None => send_bandwidth = None,
			},
			msg = data.recv() => match msg {
				Some(DataMsg::AddPad { pad, generation }) => {
					pad_set.add_pad(&pad, generation);
					// A fresh incarnation is not failed even if a previous one was.
					status.clear_failed(&pad);
				}
				Some(DataMsg::Caps { pad, generation, caps }) => pad_set.caps(&pad, generation, &caps)?,
				Some(DataMsg::Segment { pad, generation, segment }) => pad_set.segment(&pad, generation, segment),
				Some(DataMsg::Buffer { pad, generation, data, pts }) => pad_set.buffer(&pad, generation, data, pts)?,
				Some(DataMsg::Eos { pad, generation }) => {
					if pad_set.eos(&pad, generation)? {
						post_element_eos(element);
						return Ok(());
					}
				}
				// A release can complete the element if the remaining pads have all ended.
				Some(DataMsg::DropPad { pad, generation }) => {
					if pad_set.drop_pad(&pad, generation) {
						post_element_eos(element);
						return Ok(());
					}
				}
				// The element dropped the sender (state change to READY) without a shutdown signal.
				None => return Ok(()),
			},
		}
	}
}

/// Running time is shared, not per-pad, so there is no anchor to drift.
struct PadSet {
	broadcast: moq_net::BroadcastProducer,
	catalog: Option<moq_mux::catalog::Producer>,
	/// Shared with the element so the chain can read which pads the worker has failed.
	status: Arc<Status>,
	/// Authoritative membership: name -> current generation. EOS aggregation counts against this, not
	/// the lazily-created `pads`, so a pad that ends before CAPS still counts.
	active: HashMap<String, u64>,
	/// Producer state, created lazily on the first CAPS (a member without CAPS has no entry here).
	pads: HashMap<String, Pad>,
	eos: HashSet<String>,
}

impl PadSet {
	fn new(broadcast: moq_net::BroadcastProducer, catalog: moq_mux::catalog::Producer, status: Arc<Status>) -> Self {
		Self {
			broadcast,
			catalog: Some(catalog),
			status,
			active: HashMap::new(),
			pads: HashMap::new(),
			eos: HashSet::new(),
		}
	}

	/// Declares (or re-declares) a pad's membership. A recreated pad (new generation) starts fresh:
	/// any producer and EOS mark from a previous incarnation are dropped.
	fn add_pad(&mut self, pad: &str, generation: u64) {
		if let Some(mut old) = self.pads.remove(pad) {
			let _ = old.finalize();
		}
		self.eos.remove(pad);
		self.active.insert(pad.to_string(), generation);
	}

	/// A message is current only if its generation matches the pad's active generation; otherwise it
	/// belongs to a previous incarnation (or an unknown pad) and is dropped.
	fn is_current(&self, pad: &str, generation: u64) -> bool {
		self.active.get(pad) == Some(&generation)
	}

	/// Every active pad has ended (and there is at least one).
	fn all_ended(&self) -> bool {
		!self.active.is_empty() && self.active.keys().all(|name| self.eos.contains(name))
	}

	/// `Err` is session-fatal (the catalog is gone); a bad-caps failure invalidates only this pad.
	fn caps(&mut self, pad: &str, generation: u64, caps: &gst::Caps) -> Result<()> {
		if !self.is_current(pad, generation) {
			gst::warning!(CAT, "caps for stale or unknown pad {pad}, dropping");
			return Ok(());
		}
		let broadcast = self.broadcast.clone();
		let catalog = self.catalog.clone().context("catalog already finalized")?;
		let result = self
			.pads
			.entry(pad.to_string())
			.or_insert_with(Pad::new)
			.set_caps(broadcast, catalog, caps);
		if let Err(err) = result {
			gst::warning!(CAT, "invalidating pad {pad}: {err:?}");
			self.fail_pad(pad);
		}
		Ok(())
	}

	/// Drops a pad's producer (closing its track) and marks it failed so the chain returns a FlowError
	/// on its next buffer. The pad stays a member, so the session and the other pads keep going.
	fn fail_pad(&mut self, pad: &str) {
		if let Some(mut p) = self.pads.remove(pad) {
			if let Err(err) = p.finalize() {
				gst::warning!(CAT, "finalize on failed pad {pad}: {err:?}");
			}
		}
		self.status.mark_failed(pad);
	}

	fn segment(&mut self, pad: &str, generation: u64, segment: gst::Segment) {
		if !self.is_current(pad, generation) {
			gst::warning!(CAT, "segment for stale or unknown pad {pad}, dropping");
			return;
		}
		// SEGMENT may arrive before CAPS (independent sticky events); this only records timing.
		self.pads
			.entry(pad.to_string())
			.or_insert_with(Pad::new)
			.set_segment(segment);
	}

	fn buffer(&mut self, pad: &str, generation: u64, data: Bytes, pts: Option<gst::ClockTime>) -> Result<()> {
		if !self.is_current(pad, generation) {
			gst::warning!(CAT, "buffer for stale or unknown pad {pad}, dropping");
			return Ok(());
		}
		if self.eos.contains(pad) {
			gst::warning!(CAT, "buffer after EOS on pad {pad}, dropping");
			return Ok(());
		}
		let result = match self.pads.get_mut(pad) {
			Some(p) => p.push_buffer(data, pts),
			None => {
				gst::warning!(CAT, "buffer before caps on pad {pad}, dropping");
				return Ok(());
			}
		};
		// A bad bitstream invalidates only this pad; the session and other pads continue.
		if let Err(err) = result {
			gst::warning!(CAT, "invalidating pad {pad}: {err:?}");
			self.fail_pad(pad);
		}
		Ok(())
	}

	/// Returns whether every active pad has now ended, so the caller posts the element EOS once.
	fn eos(&mut self, pad: &str, generation: u64) -> Result<bool> {
		if !self.is_current(pad, generation) {
			gst::warning!(CAT, "EOS for stale or unknown pad {pad}, ignoring");
			return Ok(false);
		}
		// Duplicate EOS is idempotent: do not re-finalize, just re-report completion.
		if self.eos.contains(pad) {
			return Ok(self.all_ended());
		}
		if let Some(p) = self.pads.get_mut(pad) {
			p.finalize()?;
		}
		self.eos.insert(pad.to_string());
		Ok(self.all_ended())
	}

	/// Returns whether the remaining active pads have all ended (a release can complete the element).
	fn drop_pad(&mut self, pad: &str, generation: u64) -> bool {
		if self.active.get(pad) != Some(&generation) {
			gst::warning!(CAT, "DropPad for stale or unknown pad {pad}, ignoring");
			return false;
		}
		if let Some(mut p) = self.pads.remove(pad) {
			if let Err(err) = p.finalize() {
				gst::warning!(CAT, "finalize on drop {pad}: {err:?}");
			}
		}
		self.active.remove(pad);
		self.eos.remove(pad);
		self.all_ended()
	}

	/// Idempotent (skips already-finalized pads); the returned order proves "catalog last".
	fn finalize_all(&mut self) -> Vec<String> {
		let mut order = Vec::new();
		for (name, pad) in self.pads.iter_mut() {
			match pad.finalize() {
				Ok(true) => order.push(name.clone()),
				Ok(false) => {}
				Err(err) => gst::warning!(CAT, "finalize {name}: {err:?}"),
			}
		}
		// finish() closes both the hang and MSF tracks; a bare drop would not.
		if let Some(mut catalog) = self.catalog.take() {
			match catalog.finish() {
				Ok(()) => order.push("catalog".to_string()),
				Err(err) => gst::warning!(CAT, "finalize catalog: {err:?}"),
			}
		}
		order
	}
}

/// Per-pad timeline state. Buffers only map and emit while `Active`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PadState {
	/// No valid SEGMENT seen yet.
	NoSegment,
	/// A valid timeline is anchored.
	Active,
	/// A live timeline broke (discontinuity, non-TIME, or rate != 1.0); buffers drop until a valid
	/// SEGMENT re-anchors the pad.
	Invalid,
}

struct Pad {
	framed: Option<moq_mux::import::Framed>,
	caps: Option<gst::Caps>,
	state: PadState,
	segment_info: Option<SegmentInfo>,
	// Kept only to map a buffer PTS to a running time.
	segment: Option<gst::FormattedSegment<gst::ClockTime>>,
}

impl Pad {
	fn new() -> Self {
		Self {
			framed: None,
			caps: None,
			state: PadState::NoSegment,
			segment_info: None,
			segment: None,
		}
	}

	fn set_caps(
		&mut self,
		broadcast: moq_net::BroadcastProducer,
		catalog: moq_mux::catalog::Producer,
		caps: &gst::Caps,
	) -> Result<()> {
		let structure = caps.structure(0).context("empty caps")?;
		ensure!(
			caps_supported(caps),
			"spike only carries H.264 byte-stream, got {}",
			structure.name()
		);
		// Identical caps re-sent (sticky event): keep the live producer, don't finalize and recreate.
		if self.framed.is_some() && self.caps.as_ref() == Some(caps) {
			return Ok(());
		}
		// Renegotiation: finalize the previous producer before replacing it (closed once, not abandoned).
		self.finalize()?;
		let mut empty = Bytes::new();
		self.framed = Some(moq_mux::import::Framed::new(
			broadcast,
			catalog,
			moq_mux::import::FramedFormat::Avc3,
			&mut empty,
		)?);
		self.caps = Some(caps.clone());
		Ok(())
	}

	fn set_segment(&mut self, segment: gst::Segment) {
		let info = segment_info(&segment);
		// Only an Active pad enforces continuity against its previous segment; NoSegment and Invalid
		// re-anchor from scratch on the next valid one.
		let prev = match self.state {
			PadState::Active => self.segment_info.as_ref(),
			PadState::NoSegment | PadState::Invalid => None,
		};
		match classify_segment(prev, &info) {
			SegmentDecision::Accept => {
				self.segment_info = Some(info);
				self.segment = segment.downcast::<gst::ClockTime>().ok();
				self.state = PadState::Active;
			}
			SegmentDecision::Reject(reason) => {
				gst::warning!(CAT, "rejecting segment: {reason}");
				// A break only invalidates a live timeline; a bad segment before any valid one leaves
				// the pad in NoSegment.
				if self.state == PadState::Active {
					self.state = PadState::Invalid;
				}
			}
		}
	}

	/// Pure of the importer, so it can be tested with real segments and no codec.
	fn frame_timestamp(&self, pts: Option<gst::ClockTime>) -> FrameDecision {
		match self.state {
			PadState::Active => {
				// to_running_time_full is signed: a buffer before the segment returns Negative, which
				// frame_micros drops; to_running_time would instead clip it to None and lose the reason.
				let running_time = self
					.segment
					.as_ref()
					.zip(pts)
					.and_then(|(segment, pts)| segment.to_running_time_full(pts))
					.map(signed_nanos);
				frame_micros(running_time)
			}
			PadState::NoSegment => FrameDecision::Drop("buffer before a valid SEGMENT"),
			PadState::Invalid => FrameDecision::Drop("buffer on an invalidated timeline"),
		}
	}

	fn push_buffer(&mut self, mut data: Bytes, pts: Option<gst::ClockTime>) -> Result<()> {
		let decision = self.frame_timestamp(pts);
		let Some(framed) = self.framed.as_mut() else {
			gst::warning!(CAT, "dropping buffer received before caps");
			return Ok(());
		};

		match decision {
			FrameDecision::Emit(micros) => {
				let ts = hang::container::Timestamp::from_micros(micros).ok();
				framed.decode_frame(&mut data, ts).map_err(|err| anyhow::anyhow!(err))
			}
			FrameDecision::Drop(reason) => {
				gst::warning!(CAT, "dropping frame: {reason}");
				Ok(())
			}
		}
	}

	/// Consumes the producer so a second call is a no-op (`Framed::finish()` is not idempotent).
	fn finalize(&mut self) -> Result<bool> {
		match self.framed.take() {
			Some(mut framed) => {
				framed.finish()?;
				Ok(true)
			}
			None => Ok(false),
		}
	}
}

/// The spike only carries H.264 byte-stream. Checked synchronously at the event boundary (so an
/// unsupported caps is rejected with NotNegotiated) and again in `set_caps` as the worker's defence.
pub(super) fn caps_supported(caps: &gst::CapsRef) -> bool {
	caps.structure(0).map(|s| s.name() == "video/x-h264").unwrap_or(false)
}

fn segment_info(segment: &gst::Segment) -> SegmentInfo {
	match segment.downcast_ref::<gst::ClockTime>() {
		Some(time) => SegmentInfo {
			time_format: true,
			rate: time.rate(),
			base_nanos: time.base().map(|c| c.nseconds()).unwrap_or(0),
		},
		None => SegmentInfo {
			time_format: false,
			rate: segment.rate(),
			base_nanos: 0,
		},
	}
}

/// Flattens a signed running time to nanos, keeping the sign so the timeline can drop negatives.
fn signed_nanos(running_time: gst::Signed<gst::ClockTime>) -> i64 {
	match running_time {
		gst::Signed::Positive(time) => time.nseconds() as i64,
		gst::Signed::Negative(time) => -(time.nseconds() as i64),
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;

	fn pad_set() -> PadSet {
		let mut broadcast = moq_net::Broadcast::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();
		PadSet::new(broadcast, catalog, Arc::new(Status::default()))
	}

	fn h264_caps() -> gst::Caps {
		gst::Caps::builder("video/x-h264")
			.field("stream-format", "byte-stream")
			.field("alignment", "au")
			.build()
	}

	fn audio_caps() -> gst::Caps {
		gst::Caps::builder("audio/x-raw").build()
	}

	// EOS/new-caps/drop/shutdown converge on exactly one finalize per producer; catalog last.
	#[test]
	fn finalize_all_finishes_pads_then_catalog_once() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("video", 0);
		set.add_pad("audio", 0);
		set.caps("video", 0, &h264_caps()).unwrap();
		set.caps("audio", 0, &h264_caps()).unwrap();

		let order = set.finalize_all();
		assert_eq!(
			order.last().map(String::as_str),
			Some("catalog"),
			"catalog must finalize last"
		);
		assert!(order.contains(&"video".to_string()) && order.contains(&"audio".to_string()));

		// A second pass finalizes nothing again.
		assert!(set.finalize_all().is_empty());
	}

	#[test]
	fn eos_then_shutdown_does_not_double_finalize() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("video", 0);
		set.caps("video", 0, &h264_caps()).unwrap();

		assert!(set.eos("video", 0).unwrap());
		// Only the catalog is left; the pad is not finalized twice.
		assert_eq!(set.finalize_all(), vec!["catalog".to_string()]);
	}

	#[test]
	fn identical_caps_keep_one_live_producer() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("video", 0);
		set.caps("video", 0, &h264_caps()).unwrap();
		// Re-sent identical caps are a no-op; the pad still holds exactly one live producer.
		set.caps("video", 0, &h264_caps()).unwrap();
		assert_eq!(set.pads.len(), 1);
		assert!(set.pads["video"].framed.is_some());
	}

	#[test]
	fn buffer_for_unknown_pad_is_dropped_without_error() {
		let mut set = pad_set();
		assert!(set
			.buffer("ghost", 0, Bytes::from_static(b"x"), Some(gst::ClockTime::ZERO))
			.is_ok());
	}

	// AddPad declares membership independent of CAPS, and generation discriminates incarnations.
	#[test]
	fn add_pad_makes_a_member_independent_of_caps() {
		let mut set = pad_set();
		set.add_pad("video", 0);
		assert!(set.is_current("video", 0));
		assert!(!set.is_current("video", 1), "a different generation is not current");
		assert!(!set.is_current("audio", 0), "an unknown pad is not current");
	}

	// EOS before any CAPS still counts: a member with no producer has nothing to finalize but ends.
	#[test]
	fn eos_before_caps_counts_toward_completion() {
		let mut set = pad_set();
		set.add_pad("video", 0);
		assert!(set.eos("video", 0).unwrap());
	}

	// The element completes only once every member has ended; a still-open member holds it open.
	#[test]
	fn element_completes_only_after_all_members_eos() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("a", 0);
		set.add_pad("b", 0);
		set.caps("a", 0, &h264_caps()).unwrap();
		set.caps("b", 0, &h264_caps()).unwrap();
		assert!(!set.eos("a", 0).unwrap(), "one of two members ended, not complete");
		assert!(set.eos("b", 0).unwrap(), "both members ended, complete");
	}

	// Duplicate EOS is idempotent: it neither re-finalizes the producer nor changes completion.
	#[test]
	fn duplicate_eos_is_idempotent() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("video", 0);
		set.caps("video", 0, &h264_caps()).unwrap();
		assert!(set.eos("video", 0).unwrap());
		assert!(set.eos("video", 0).unwrap(), "duplicate EOS stays complete");
		// Finalized exactly once: only the catalog remains for the exit sweep.
		assert_eq!(set.finalize_all(), vec!["catalog".to_string()]);
	}

	// A buffer that arrives after the pad's EOS is dropped (the producer is already finalized).
	#[test]
	fn buffer_after_eos_is_dropped() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("video", 0);
		set.caps("video", 0, &h264_caps()).unwrap();
		set.eos("video", 0).unwrap();
		assert!(set
			.buffer("video", 0, Bytes::from_static(b"x"), Some(gst::ClockTime::ZERO))
			.is_ok());
	}

	// A pad recreated with the same name (new generation) discards the previous incarnation's messages.
	#[test]
	fn stale_generation_messages_are_dropped() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("video", 0);
		set.caps("video", 0, &h264_caps()).unwrap();
		set.add_pad("video", 1);
		// Old-generation messages no longer match the active generation.
		assert!(set
			.buffer("video", 0, Bytes::from_static(b"x"), Some(gst::ClockTime::ZERO))
			.is_ok());
		assert!(
			!set.eos("video", 0).unwrap(),
			"a stale EOS must not complete the element"
		);
		// The current generation still completes it.
		assert!(set.eos("video", 1).unwrap());
	}

	#[test]
	fn caps_supported_accepts_only_h264() {
		gst::init().unwrap();
		assert!(caps_supported(&h264_caps()));
		assert!(!caps_supported(&audio_caps()));
	}

	// A pad with unsupported caps is invalidated alone: marked failed and its producer dropped, while
	// the session and the other pad keep going (a pad error is not a session error).
	#[test]
	fn invalid_caps_fails_only_that_pad() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("video", 0);
		set.add_pad("data", 0);
		set.caps("video", 0, &h264_caps()).unwrap();

		assert!(
			set.caps("data", 0, &audio_caps()).is_ok(),
			"a pad error must not kill the session"
		);
		assert!(set.status.is_failed("data"), "the bad pad is marked failed");
		assert!(!set.status.is_failed("video"), "the good pad is untouched");
		assert!(set.pads.contains_key("video"), "the good pad keeps its producer");
		assert!(!set.pads.contains_key("data"), "the failed pad's producer is dropped");
	}

	// A finalized catalog is a session failure: caps returns Err so the worker tears the session down.
	#[test]
	fn caps_after_catalog_finalized_is_a_session_error() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("video", 0);
		set.finalize_all();
		assert!(
			set.caps("video", 0, &h264_caps()).is_err(),
			"a gone catalog is session-fatal"
		);
	}

	// SEGMENT before CAPS: the pad is created and the segment retained when CAPS arrives.
	#[test]
	fn segment_before_caps_is_retained() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("video", 0);
		set.segment("video", 0, time_segment());
		set.caps("video", 0, &h264_caps()).unwrap();

		let pad = &set.pads["video"];
		assert!(pad.segment_info.is_some(), "segment kept across a later caps");
		assert!(pad.framed.is_some());
	}

	// Firing the watch drops the receiver, waking a sender parked on the full channel with Err.
	#[test]
	fn shutdown_via_watch_releases_a_blocked_send() {
		let runtime = tokio::runtime::Runtime::new().unwrap();
		let (data_tx, data_rx) = mpsc::channel::<u8>(DATA_CHANNEL_BOUND);
		let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

		for _ in 0..DATA_CHANNEL_BOUND {
			data_tx.blocking_send(0).unwrap(); // fill to capacity
		}

		// Hold the receiver without draining (as if busy in a branch body), then return on the watch.
		let loop_task = runtime.spawn(async move {
			let _held = data_rx;
			shutdown_rx.changed().await.ok();
		});

		let sender = data_tx.clone();
		let blocked = std::thread::spawn(move || sender.blocking_send(1));
		std::thread::sleep(Duration::from_millis(50)); // let the send park

		shutdown_tx.send(true).unwrap();
		runtime.block_on(loop_task).unwrap();

		assert!(
			blocked.join().unwrap().is_err(),
			"a send blocked on the full channel must wake with Err when shutdown drops the receiver"
		);
	}

	// Two pads, real PTS via to_running_time_full: the A/V offset survives because running time is shared.
	#[test]
	fn two_pads_keep_av_aligned_through_real_segments() {
		gst::init().unwrap();
		let mut video = Pad::new();
		let mut audio = Pad::new();
		video.set_segment(time_segment());
		audio.set_segment(time_segment());

		assert_eq!(
			video.frame_timestamp(Some(gst::ClockTime::from_mseconds(7))),
			FrameDecision::Emit(7_000)
		);
		assert_eq!(
			audio.frame_timestamp(Some(gst::ClockTime::from_mseconds(5))),
			FrameDecision::Emit(5_000)
		);
	}

	// A pad with no SEGMENT yet drops buffers (NoSegment), distinct from an invalidated timeline.
	#[test]
	fn pad_without_segment_drops_buffers() {
		let pad = Pad::new();
		assert_eq!(pad.state, PadState::NoSegment);
		assert!(matches!(
			pad.frame_timestamp(Some(gst::ClockTime::from_mseconds(5))),
			FrameDecision::Drop(_)
		));
	}

	// The fix: a moved media start stays continuous as long as the running-time base advances. The
	// old start-equality rule would have rejected this and stalled the pad.
	#[test]
	fn moved_start_with_advancing_base_stays_continuous() {
		gst::init().unwrap();
		let mut pad = Pad::new();
		pad.set_segment(time_segment_at(0, 0));
		assert_eq!(pad.state, PadState::Active);
		pad.set_segment(time_segment_at(30_000, 5_000));
		assert_eq!(pad.state, PadState::Active);
	}

	// A buffer before the segment start yields a negative running time: drop it, never clamp to zero.
	#[test]
	fn frame_before_segment_start_is_dropped_not_clamped() {
		gst::init().unwrap();
		let mut pad = Pad::new();
		pad.set_segment(time_segment_at(10_000, 0));
		assert!(matches!(
			pad.frame_timestamp(Some(gst::ClockTime::from_mseconds(5_000))),
			FrameDecision::Drop(_)
		));
		// A PTS at or after the start maps to a non-negative running time and emits.
		assert_eq!(
			pad.frame_timestamp(Some(gst::ClockTime::from_mseconds(12_000))),
			FrameDecision::Emit(2_000_000)
		);
	}

	// A discontinuity invalidates the pad (drops), and the next valid SEGMENT re-anchors it to Active.
	#[test]
	fn invalid_segment_drops_then_a_valid_one_recovers() {
		gst::init().unwrap();
		let mut pad = Pad::new();
		pad.set_segment(time_segment_at(0, 5_000));
		assert_eq!(pad.state, PadState::Active);

		// base rewinds -> discontinuous -> Invalid; buffers drop.
		pad.set_segment(time_segment_at(0, 0));
		assert_eq!(pad.state, PadState::Invalid);
		assert!(matches!(
			pad.frame_timestamp(Some(gst::ClockTime::from_mseconds(6_000))),
			FrameDecision::Drop(_)
		));

		// A valid SEGMENT re-anchors -> Active; buffers emit again.
		pad.set_segment(time_segment_at(0, 10_000));
		assert_eq!(pad.state, PadState::Active);
		assert_eq!(
			pad.frame_timestamp(Some(gst::ClockTime::ZERO)),
			FrameDecision::Emit(10_000_000)
		);
	}

	fn time_segment() -> gst::Segment {
		let mut segment = gst::FormattedSegment::<gst::ClockTime>::new();
		segment.set_start(gst::ClockTime::ZERO);
		segment.upcast()
	}

	fn time_segment_at(start_ms: u64, base_ms: u64) -> gst::Segment {
		let mut segment = gst::FormattedSegment::<gst::ClockTime>::new();
		segment.set_start(gst::ClockTime::from_mseconds(start_ms));
		segment.set_base(gst::ClockTime::from_mseconds(base_ms));
		segment.upcast()
	}
}
