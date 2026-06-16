//! Async core: the session/pads/timeline seams, isolated from the GObject shell.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};

use anyhow::{ensure, Context, Result};
use bytes::Bytes;
use gst::glib;
use gst::prelude::*;
use tokio::sync::{mpsc, watch};

use hang::moq_net;
use moq_mux::import::{Framed, FramedFormat};

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

/// Shared observable state, read by the element's getters and the chain without touching the worker
/// task. One `Mutex` covers the session generation plus every gated field, so "check the live
/// generation, then write" is one indivisible step: a stale session (one whose task lingers after a
/// newer one started) cannot clobber the live status even by racing the check against `begin_session`.
#[derive(Default)]
struct StatusInner {
	/// Bumped by `begin_session` on every new session; the latest value is the live generation.
	generation: u64,
	connected: bool,
	version: Option<String>,
	send_bitrate: u64,
	/// Pads the worker rejected, keyed by name -> (session generation, pad generation) of the failed
	/// incarnation. `is_failed` matches only the live session, so neither a recreated pad nor a
	/// recreated session (same pad name and pad generation across a restart) inherits a stale failure.
	failed: HashMap<String, (u64, u64)>,
}

#[derive(Default)]
pub struct Status {
	inner: Mutex<StatusInner>,
}

impl Status {
	/// Claim a fresh generation for a starting session; the returned value is now the live generation.
	fn begin_session(&self) -> u64 {
		let mut s = self.inner.lock().unwrap();
		s.generation += 1;
		s.generation
	}

	/// True only while `generation` is the live session. A point-in-time check (only the setters gate
	/// atomically); use it where a stale result is harmless, e.g. skipping a notify.
	fn is_live(&self, generation: u64) -> bool {
		self.inner.lock().unwrap().generation == generation
	}

	fn set_connected(&self, generation: u64, value: bool) {
		let mut s = self.inner.lock().unwrap();
		if s.generation == generation {
			s.connected = value;
		}
	}

	fn set_version(&self, generation: u64, value: Option<String>) {
		let mut s = self.inner.lock().unwrap();
		if s.generation == generation {
			s.version = value;
		}
	}

	fn set_send_bitrate(&self, generation: u64, bits_per_sec: u64) {
		let mut s = self.inner.lock().unwrap();
		if s.generation == generation {
			s.send_bitrate = bits_per_sec;
		}
	}

	/// Reset the observable surface on a session's exit, but only if it is still the live generation.
	/// A newer session may have started before this one's task unwinds, so a stale exit must not clobber
	/// the live status. Returns whether it actually reset (so the caller can skip a spurious notify).
	fn reset_on_exit(&self, generation: u64) -> bool {
		let mut s = self.inner.lock().unwrap();
		if s.generation != generation {
			return false;
		}
		s.connected = false;
		s.version = None;
		s.send_bitrate = 0;
		s.failed.clear();
		true
	}

	fn mark_failed(&self, session_generation: u64, pad: &str, pad_generation: u64) {
		let mut s = self.inner.lock().unwrap();
		if s.generation == session_generation {
			s.failed.insert(pad.to_string(), (session_generation, pad_generation));
		}
	}

	fn clear_failed(&self, session_generation: u64, pad: &str) {
		let mut s = self.inner.lock().unwrap();
		if s.generation == session_generation {
			s.failed.remove(pad);
		}
	}

	pub fn is_failed(&self, pad: &str, pad_generation: u64) -> bool {
		let s = self.inner.lock().unwrap();
		s.failed.get(pad) == Some(&(s.generation, pad_generation))
	}

	pub fn connected(&self) -> bool {
		self.inner.lock().unwrap().connected
	}

	pub fn version(&self) -> Option<String> {
		self.inner.lock().unwrap().version.clone()
	}

	pub fn send_bitrate(&self) -> u64 {
		self.inner.lock().unwrap().send_bitrate
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

/// Out-of-band FLUSH notification to the worker. Carried on its own unbounded channel (not the bounded
/// `DataMsg` FIFO) so FLUSH_START re-anchors promptly and never blocks behind queued buffers.
pub struct FlushSignal {
	pub pad: String,
	pub generation: u64,
}

pub struct ResolvedSettings {
	pub url: url::Url,
	pub broadcast: String,
	pub tls_disable_verify: bool,
}

/// The worker's inbound channels, bundled so `run_session` stays under the argument limit.
struct Inbound {
	data: mpsc::Receiver<DataMsg>,
	flush: mpsc::UnboundedReceiver<FlushSignal>,
	shutdown: watch::Receiver<bool>,
}

pub struct SessionHandle {
	data: mpsc::Sender<DataMsg>,
	/// Out-of-band control path for FLUSH: unbounded so FLUSH_START never blocks behind a full data
	/// channel (the very condition a flush must break), and a discrete pad-targeted event a `watch`
	/// would collapse.
	flush: mpsc::UnboundedSender<FlushSignal>,
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
		// Claim the generation synchronously so a previous session's late exit-reset (running on its own
		// task) sees a newer generation and becomes a no-op instead of clobbering this session's status.
		let generation = status.begin_session();
		let (data_tx, data_rx) = mpsc::channel(DATA_CHANNEL_BOUND);
		let (flush_tx, flush_rx) = mpsc::unbounded_channel();
		let (shutdown_tx, shutdown_rx) = watch::channel(false);

		let join = RUNTIME.spawn(async move {
			// Run the worker on its own task so a panic surfaces as an element_error here, instead of a
			// silent operational death only observed when stop() reaps the outer JoinHandle.
			let worker = RUNTIME.spawn(run_session(
				settings,
				status,
				generation,
				seed,
				Inbound {
					data: data_rx,
					flush: flush_rx,
					shutdown: shutdown_rx,
				},
				element.clone(),
			));
			// Only a remote close reaches the bus as an error; a local shutdown returns Ok and stays quiet.
			// A panic (or cancellation) joins as Err and is surfaced too.
			let outcome = worker
				.await
				.unwrap_or_else(|join_err| Err(anyhow::anyhow!("session worker panicked: {join_err}")));
			if let Err(err) = outcome {
				if let Some(obj) = element.upgrade() {
					gst::element_error!(obj, gst::CoreError::Failed, ("session error"), ["{err:?}"]);
				}
			}
		});

		Self {
			data: data_tx,
			flush: flush_tx,
			shutdown: shutdown_tx,
			join,
		}
	}

	/// Cloned out so the element blocking-sends without holding the session lock (else a full channel deadlocks stop).
	pub fn sender(&self) -> mpsc::Sender<DataMsg> {
		self.data.clone()
	}

	/// Out-of-band FLUSH signal to the worker; unbounded, so it is safe to call from the event thread
	/// even while the data channel is full.
	pub fn flush_sender(&self) -> mpsc::UnboundedSender<FlushSignal> {
		self.flush.clone()
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

/// Outcome of a flush-cancellable send. Both `Flushed` and `Closed` map to `FlowError::Flushing` for
/// the caller; they are split only so the reason stays legible.
pub(super) enum SendOutcome {
	Sent,
	/// FLUSH_START flipped this pad's flush watch (or the watch sender was dropped on release).
	Flushed,
	/// The worker's receiver is gone (the session ended).
	Closed,
}

/// Send `msg` on the bounded data channel, aborting promptly if this pad starts flushing. This is the
/// cancellable replacement for `blocking_send`: the chain runs off the runtime, so `block_on` is safe,
/// and FLUSH_START (signalled on `flush`) can cut a send blocked on a full channel without tearing the
/// session down. The loop re-arms on a FLUSH_STOP (`false`) that lands mid-block, so a quick flush+stop
/// does not drop a still-valid buffer.
pub(super) fn send_or_flush(
	sender: &mpsc::Sender<DataMsg>,
	msg: DataMsg,
	flush: &mut watch::Receiver<bool>,
) -> SendOutcome {
	if *flush.borrow_and_update() {
		return SendOutcome::Flushed;
	}
	RUNTIME.block_on(async move {
		loop {
			tokio::select! {
				// Biased toward flush: FLUSH_START must win a tie with freed capacity, so a blocked chain is
				// cut rather than enqueuing a pre-flush buffer.
				biased;
				changed = flush.changed() => {
					// Sender dropped (release) or now flushing: abort. A `false` change (FLUSH_STOP) falls
					// through and re-arms the wait.
					if changed.is_err() || *flush.borrow() {
						return SendOutcome::Flushed;
					}
				}
				permit = sender.reserve() => match permit {
					Ok(permit) => {
						// Re-check after winning the permit: a flushing pad drops the buffer. This narrows (does
						// not atomically close) the race: a FLUSH_START in the gap before the send below still
						// enqueues one buffer, which the worker re-anchor and the next buffer's check absorb.
						if *flush.borrow_and_update() {
							return SendOutcome::Flushed;
						}
						permit.send(msg);
						return SendOutcome::Sent;
					}
					Err(_) => return SendOutcome::Closed,
				},
			}
		}
	})
}

async fn run_session(
	settings: ResolvedSettings,
	status: Arc<Status>,
	generation: u64,
	seed: Vec<(String, u64)>,
	inbound: Inbound,
	element: glib::WeakRef<Element>,
) -> Result<()> {
	let Inbound {
		mut data,
		mut flush,
		mut shutdown,
	} = inbound;
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
	status.set_connected(generation, true);
	status.set_version(generation, Some(session.version().to_string()));
	if status.is_live(generation) {
		notify_connected(&element);
	}
	gst::info!(CAT, "session connected to {}", settings.url);

	let mut pad_set = PadSet::new(broadcast, catalog, status.clone(), generation);
	// Pads requested before the session task existed are seeded into the authoritative set.
	for (name, generation) in seed {
		pad_set.add_pad(&name, generation);
	}
	let reason = run_loop(
		session,
		generation,
		&mut data,
		&mut flush,
		&mut shutdown,
		&mut pad_set,
		&status,
	)
	.await;

	// Finalize every live producer once on the way out, catalog last; runs on every exit path.
	let finalized = pad_set.finalize_all();
	// Reset the whole observable surface on exit, but only if no newer session has taken over (else a
	// stale exit would clobber the live session's status). Skip the notify when the reset was a no-op.
	if status.reset_on_exit(generation) {
		notify_connected(&element);
	}

	match (reason, finalized) {
		// Clean end: post the element EOS only once the catalog has finalized cleanly.
		(Ok(ExitReason::Ended), Ok(order)) => {
			gst::debug!(CAT, "finalized on exit: {order:?}");
			post_element_eos(&element);
			Ok(())
		}
		(Ok(ExitReason::Stopped), Ok(order)) => {
			gst::debug!(CAT, "finalized on exit: {order:?}");
			Ok(())
		}
		// A finalize failure is surfaced (becomes element_error on the bus), never silently logged.
		(Ok(_), Err(err)) => Err(err),
		// The session already failed; finalize was best-effort.
		(Err(session_err), _) => Err(session_err),
	}
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

/// Why run_loop returned: a clean end (all pads EOS) posts the element EOS after finalize; a stop
/// (local shutdown, dropped sender, or clean remote close) finalizes quietly.
enum ExitReason {
	Ended,
	Stopped,
}

async fn run_loop(
	session: moq_net::Session,
	generation: u64,
	data: &mut mpsc::Receiver<DataMsg>,
	flush: &mut mpsc::UnboundedReceiver<FlushSignal>,
	shutdown: &mut watch::Receiver<bool>,
	pad_set: &mut PadSet,
	status: &Status,
) -> Result<ExitReason> {
	// Congestion-controller send estimate; None when unavailable, then this arm parks forever.
	let mut send_bandwidth = session.send_bandwidth();

	// Resolves to Err when the transport dies; pinned so the select polls it each iteration.
	let closed = session.closed();
	tokio::pin!(closed);

	loop {
		tokio::select! {
			// Biased so a pending FLUSH re-anchors before the post-flush SEGMENT it precedes: the flush
			// signal is enqueued before that SEGMENT on the streaming thread, and biased preserves that
			// order across the two channels. Shutdown/death still preempt everything.
			biased;
			// Local close: quiet stop, no ERROR.
			_ = shutdown.changed() => return Ok(ExitReason::Stopped),
			// Remote death: propagate the Err so the wrapper posts ERROR to the bus.
			result = &mut closed => {
				result?;
				return Ok(ExitReason::Stopped);
			}
			// FLUSH (out of band): re-anchor the pad's timeline before any post-flush data is processed.
			Some(FlushSignal { pad, generation }) = flush.recv() => pad_set.flush(&pad, generation),
			// A closed estimate stops the polling.
			bitrate = async {
				match send_bandwidth.as_mut() {
					Some(bw) => bw.changed().await,
					None => std::future::pending::<Option<u64>>().await,
				}
			} => match bitrate {
				Some(rate) => status.set_send_bitrate(generation, rate),
				None => send_bandwidth = None,
			},
			msg = data.recv() => match msg {
				Some(DataMsg::AddPad { pad, generation }) => {
					// add_pad clears any stale failure for a fresh incarnation (keyed by this session).
					pad_set.add_pad(&pad, generation);
				}
				Some(DataMsg::Caps { pad, generation, caps }) => {
					if pad_set.caps(&pad, generation, &caps)? {
						return Ok(ExitReason::Ended);
					}
				}
				Some(DataMsg::Segment { pad, generation, segment }) => pad_set.segment(&pad, generation, segment),
				Some(DataMsg::Buffer { pad, generation, data, pts }) => {
					if pad_set.buffer(&pad, generation, data, pts)? {
						return Ok(ExitReason::Ended);
					}
				}
				Some(DataMsg::Eos { pad, generation }) => {
					if pad_set.eos(&pad, generation)? {
						return Ok(ExitReason::Ended);
					}
				}
				// A release can complete the element if the remaining pads have all ended.
				Some(DataMsg::DropPad { pad, generation }) => {
					if pad_set.drop_pad(&pad, generation) {
						return Ok(ExitReason::Ended);
					}
				}
				// The element dropped the sender (state change to READY) without a shutdown signal.
				None => return Ok(ExitReason::Stopped),
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
	/// This session's generation; tags failure marks so a stale task's late mark/clear cannot affect the
	/// live session's view of which pads have failed.
	session_generation: u64,
	/// Authoritative membership: name -> current generation. EOS aggregation counts against this, not
	/// the lazily-created `pads`, so a pad that ends before CAPS still counts.
	active: HashMap<String, u64>,
	/// Producer state, created lazily on the first CAPS (a member without CAPS has no entry here).
	pads: HashMap<String, Pad>,
	eos: HashSet<String>,
}

impl PadSet {
	fn new(
		broadcast: moq_net::BroadcastProducer,
		catalog: moq_mux::catalog::Producer,
		status: Arc<Status>,
		session_generation: u64,
	) -> Self {
		Self {
			broadcast,
			catalog: Some(catalog),
			status,
			session_generation,
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
		self.status.clear_failed(self.session_generation, pad);
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

	/// `Ok(true)` means the element is now complete (a pad failure can finish it); `Err` is session-fatal
	/// (the catalog is gone). A bad-caps failure invalidates only this pad.
	fn caps(&mut self, pad: &str, generation: u64, caps: &gst::Caps) -> Result<bool> {
		if !self.is_current(pad, generation) {
			gst::warning!(CAT, "caps for stale or unknown pad {pad}, dropping");
			return Ok(false);
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
			self.fail_pad(pad, generation);
			return Ok(self.all_ended());
		}
		Ok(false)
	}

	/// Drops a pad's producer (closing its track) and marks it failed so the chain returns a FlowError
	/// on its next buffer. The pad stays a member, so the session and the other pads keep going.
	fn fail_pad(&mut self, pad: &str, generation: u64) {
		if let Some(mut p) = self.pads.remove(pad) {
			if let Err(err) = p.finalize() {
				gst::warning!(CAT, "finalize on failed pad {pad}: {err:?}");
			}
		}
		self.status.mark_failed(self.session_generation, pad, generation);
		// The failed pad's track is finalized, so it is terminal for EOS aggregation (counts as ended).
		self.eos.insert(pad.to_string());
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

	/// FLUSH_START re-anchor: drop this pad's timeline so the next SEGMENT is accepted fresh and
	/// post-flush frames are not dropped as regressions. The producer and its track are kept (FLUSH is
	/// not EOS). A flush before CAPS has no producer entry and is a no-op.
	fn flush(&mut self, pad: &str, generation: u64) {
		if !self.is_current(pad, generation) {
			gst::warning!(CAT, "flush for stale or unknown pad {pad}, ignoring");
			return;
		}
		if let Some(p) = self.pads.get_mut(pad) {
			p.flush();
		}
	}

	/// `Ok(true)` means the element is now complete (a pad failure can finish it).
	fn buffer(&mut self, pad: &str, generation: u64, data: Bytes, pts: Option<gst::ClockTime>) -> Result<bool> {
		if !self.is_current(pad, generation) {
			gst::warning!(CAT, "buffer for stale or unknown pad {pad}, dropping");
			return Ok(false);
		}
		if self.eos.contains(pad) {
			gst::warning!(CAT, "buffer after EOS on pad {pad}, dropping");
			return Ok(false);
		}
		let result = match self.pads.get_mut(pad) {
			Some(p) => p.push_buffer(data, pts),
			None => {
				gst::warning!(CAT, "buffer before caps on pad {pad}, dropping");
				return Ok(false);
			}
		};
		// A bad bitstream invalidates only this pad; the session and other pads continue.
		if let Err(err) = result {
			gst::warning!(CAT, "invalidating pad {pad}: {err:?}");
			self.fail_pad(pad, generation);
			return Ok(self.all_ended());
		}
		Ok(false)
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

	/// Idempotent (skips already-finalized pads); the returned order proves "catalog last". `Err` means
	/// the catalog could not be closed cleanly, which the caller surfaces instead of posting EOS.
	fn finalize_all(&mut self) -> Result<Vec<String>> {
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
			catalog.finish().context("finalize catalog")?;
			order.push("catalog".to_string());
		}
		Ok(order)
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
		// Identical caps re-sent (sticky event): keep the live producer, don't finalize and recreate.
		if self.framed.is_some() && self.caps.as_ref() == Some(caps) {
			return Ok(());
		}
		// Renegotiation: finalize the previous producer before replacing it (closed once, not abandoned).
		self.finalize()?;
		// Every codec converges on one Framed; only the caps -> producer construction differs. A bad or
		// unsupported caps is a per-pad error (the caller invalidates just this pad), not session-fatal.
		let framed: Framed = match structure.name().as_str() {
			"video/x-h264" => {
				require_byte_stream_au(structure)?;
				Framed::new(broadcast, catalog, FramedFormat::Avc3, &mut Bytes::new())?
			}
			"video/x-h265" => {
				require_byte_stream_au(structure)?;
				Framed::new(broadcast, catalog, FramedFormat::Hev1, &mut Bytes::new())?
			}
			"video/x-av1" => Framed::new(broadcast, catalog, FramedFormat::Av01, &mut Bytes::new())?,
			"video/x-vp8" => Framed::new(broadcast, catalog, FramedFormat::Vp8, &mut Bytes::new())?,
			"video/x-vp9" => Framed::new(broadcast, catalog, FramedFormat::Vp9, &mut Bytes::new())?,
			"audio/mpeg" => {
				// AAC: the AudioSpecificConfig rides in caps as codec_data, not in the bitstream.
				ensure!(
					structure.get::<i32>("mpegversion").is_ok_and(|v| v == 4),
					"AAC requires mpegversion=4"
				);
				ensure!(
					structure.get::<String>("stream-format").is_ok_and(|f| f == "raw"),
					"AAC requires stream-format=raw"
				);
				let codec_data = structure
					.get::<gst::Buffer>("codec_data")
					.context("AAC caps missing codec_data")?;
				let map = codec_data.map_readable().context("failed to map AAC codec_data")?;
				let mut data = Bytes::copy_from_slice(map.as_slice());
				Framed::new(broadcast, catalog, FramedFormat::Aac, &mut data)?
			}
			"audio/x-opus" => {
				// Opus: GStreamer gives channels/rate in caps (not an OpusHead), so build the config here.
				let channels: i32 = structure.get("channels").unwrap_or(2);
				let rate: i32 = structure.get("rate").unwrap_or(48_000);
				let channel_count = u32::try_from(channels)
					.with_context(|| format!("Opus caps has negative channel count {channels}"))?;
				let sample_rate =
					u32::try_from(rate).with_context(|| format!("Opus caps has negative sample rate {rate}"))?;
				let config = moq_mux::codec::opus::Config {
					sample_rate,
					channel_count,
				};
				moq_mux::codec::opus::Import::new(broadcast, catalog, config)?.into()
			}
			other => anyhow::bail!("unsupported caps: {other}"),
		};
		self.framed = Some(framed);
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

	/// Re-anchor on FLUSH. A flushing seek rewinds running time, so the timeline must restart: dropping
	/// the segment moves the pad to NoSegment (the next SEGMENT is accepted fresh via `prev = None`). The
	/// producer is kept (FLUSH is not EOS); the codec's partial-AU reset is a documented follow-up, not
	/// handled here.
	fn flush(&mut self) {
		self.state = PadState::NoSegment;
		self.segment = None;
		self.segment_info = None;
	}

	/// Pure of the importer, so it can be tested with real segments and no codec. Emits the PTS-derived
	/// running time without enforcing frame-level monotonicity: frames arrive in decode order and the
	/// moq-mux container documents that B-frames carry non-monotonic presentation timestamps, so a PTS
	/// regression is normal reordering, not an error. Timeline breaks are caught at the SEGMENT level
	/// (the `Invalid` state), not per frame.
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
					.and_then(signed_nanos);
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
				// A lazy codec (H.265/AV1/VP8/VP9) given CAPS but no frame never created its track, so
				// there is nothing to flush and finish() would error "not initialized". track() is Ok only
				// once a track exists; a real finish error on an initialized one still surfaces.
				if framed.track().is_ok() {
					framed.finish()?;
				}
				Ok(true)
			}
			None => Ok(false),
		}
	}
}

/// Media types the spike can build a producer for. Checked synchronously at the event boundary (an
/// unsupported caps is rejected with NotNegotiated) and again in `set_caps`. Per-codec specifics
/// (byte-stream/au, AAC codec_data) are enforced when the producer is built, so a bad detail fails
/// that pad, not the session.
pub(super) fn caps_supported(caps: &gst::CapsRef) -> bool {
	let Some(s) = caps.structure(0) else { return false };
	matches!(
		s.name().as_str(),
		"video/x-h264" | "video/x-h265" | "video/x-av1" | "video/x-vp8" | "video/x-vp9" | "audio/mpeg" | "audio/x-opus"
	)
}

/// FramedFormat::Avc3/Hev1 consume Annex-B access units, so H.264/H.265 caps must be byte-stream/au.
fn require_byte_stream_au(s: &gst::StructureRef) -> Result<()> {
	ensure!(
		s.get::<String>("stream-format").is_ok_and(|f| f == "byte-stream"),
		"{} requires stream-format=byte-stream",
		s.name()
	);
	ensure!(
		s.get::<String>("alignment").is_ok_and(|a| a == "au"),
		"{} requires alignment=au",
		s.name()
	);
	Ok(())
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
/// None on overflow of u64 nanos into i64 (unreachable in practice).
fn signed_nanos(running_time: gst::Signed<gst::ClockTime>) -> Option<i64> {
	match running_time {
		gst::Signed::Positive(time) => i64::try_from(time.nseconds()).ok(),
		gst::Signed::Negative(time) => i64::try_from(time.nseconds()).ok().map(|nanos| -nanos),
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;

	fn pad_set() -> PadSet {
		let mut broadcast = moq_net::Broadcast::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();
		PadSet::new(broadcast, catalog, Arc::new(Status::default()), 0)
	}

	// A producer that actually emitted has at least one group. latest() is the cross-crate-visible
	// sync read; moq-net's assert_group helper is gated to that crate's own tests.
	fn emitted_a_frame(set: &PadSet, track: &str) -> bool {
		set.broadcast
			.consume()
			.subscribe_track(&moq_net::Track::new(track))
			.expect("the rendition track is published")
			.latest()
			.is_some()
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

		let order = set.finalize_all().unwrap();
		assert_eq!(
			order.last().map(String::as_str),
			Some("catalog"),
			"catalog must finalize last"
		);
		assert!(order.contains(&"video".to_string()) && order.contains(&"audio".to_string()));

		// A second pass finalizes nothing again.
		assert!(set.finalize_all().unwrap().is_empty());
	}

	#[test]
	fn eos_then_shutdown_does_not_double_finalize() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("video", 0);
		set.caps("video", 0, &h264_caps()).unwrap();

		assert!(set.eos("video", 0).unwrap());
		// Only the catalog is left; the pad is not finalized twice.
		assert_eq!(set.finalize_all().unwrap(), vec!["catalog".to_string()]);
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
		assert_eq!(set.finalize_all().unwrap(), vec!["catalog".to_string()]);
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

	// caps_supported is media-type only; per-codec specifics are the factory's/template's job.
	#[test]
	fn caps_supported_accepts_known_media_types() {
		gst::init().unwrap();
		for media in [
			"video/x-h264",
			"video/x-h265",
			"video/x-av1",
			"video/x-vp8",
			"video/x-vp9",
			"audio/mpeg",
			"audio/x-opus",
		] {
			assert!(
				caps_supported(&gst::Caps::builder(media).build()),
				"{media} must be accepted"
			);
		}
		assert!(!caps_supported(&audio_caps()), "an unsupported media type is rejected");
	}

	// SPS (the importer's own proven bytes) + PPS + a type-5 IDR slice: drives the real decode_frame
	// path and emits one keyframe. The slice header is not fully parsed for IDR.
	fn h264_keyframe_au() -> Bytes {
		let sps: &[u8] = &[
			0x67, 0x42, 0xc0, 0x1f, 0xda, 0x01, 0x40, 0x16, 0xe9, 0xb8, 0x08, 0x08, 0x0a, 0x00, 0x00, 0x07, 0xd0, 0x00,
			0x01, 0xd4, 0xc0, 0x80,
		];
		let pps: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
		let idr: &[u8] = &[0x65, 0x88, 0x84, 0x00, 0x21];
		let mut au = Vec::new();
		for nal in [sps, pps, idr] {
			au.extend_from_slice(&[0, 0, 0, 1]);
			au.extend_from_slice(nal);
		}
		Bytes::from(au)
	}

	// Frame-through (real init + emit): a keyframe AU emits a frame to the rendition track, not just
	// the SPS-published rendition.
	#[test]
	fn frame_through_h264_emits_a_frame() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("video", 0);
		set.caps("video", 0, &h264_caps()).unwrap();
		set.segment("video", 0, time_segment());

		set.buffer("video", 0, h264_keyframe_au(), Some(gst::ClockTime::ZERO))
			.unwrap();

		let snapshot = set.catalog.as_ref().unwrap().snapshot();
		let track = snapshot.video.renditions.keys().next().expect("a video rendition");
		// The discriminant: a real frame reached the track. The rendition alone would pass on the SPS.
		assert!(emitted_a_frame(&set, track), "the IDR AU emitted a frame to the track");
		assert_eq!(
			snapshot.video.renditions.len(),
			1,
			"the SPS published exactly one rendition"
		);
	}

	// Frame-through for Opus: the rendition publishes from caps, then a packet emits a real frame.
	#[test]
	fn frame_through_opus_emits_a_frame() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("audio", 0);
		let caps = gst::Caps::builder("audio/x-opus")
			.field("channels", 2i32)
			.field("rate", 48_000i32)
			.build();
		set.caps("audio", 0, &caps).unwrap();
		assert_eq!(
			set.catalog.as_ref().unwrap().snapshot().audio.renditions.len(),
			1,
			"Opus publishes its rendition from channels/rate at construction"
		);

		set.segment("audio", 0, time_segment());
		// The Opus importer carries the packet verbatim; any non-empty payload is one frame.
		set.buffer(
			"audio",
			0,
			Bytes::from_static(&[0xfc, 0xff, 0xfe]),
			Some(gst::ClockTime::ZERO),
		)
		.unwrap();

		let track = set
			.catalog
			.as_ref()
			.unwrap()
			.snapshot()
			.audio
			.renditions
			.keys()
			.next()
			.expect("an audio rendition")
			.clone();
		assert!(
			emitted_a_frame(&set, &track),
			"the Opus packet emitted a frame to the track"
		);
	}

	// Creation only (decision c): these importers build from an empty init and create the track lazily,
	// so assert the producer exists, not a rendition.
	#[test]
	fn creation_succeeds_for_video_codecs() {
		gst::init().unwrap();
		let mut set = pad_set();
		for (i, media) in ["video/x-h265", "video/x-av1", "video/x-vp8", "video/x-vp9"]
			.into_iter()
			.enumerate()
		{
			let name = format!("v{i}");
			set.add_pad(&name, 0);
			// H.265 shares H.264's Annex-B requirement; AV1/VP8/VP9 carry no such specifics.
			let mut builder = gst::Caps::builder(media);
			if media == "video/x-h265" {
				builder = builder.field("stream-format", "byte-stream").field("alignment", "au");
			}
			set.caps(&name, 0, &builder.build()).unwrap();
			assert!(set.pads[&name].framed.is_some(), "{media} producer built");
		}
	}

	#[test]
	fn creation_succeeds_for_aac_with_codec_data() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("audio", 0);
		// AudioSpecificConfig: AAC-LC, 44100 Hz, stereo.
		let codec_data = gst::Buffer::from_slice([0x12u8, 0x10]);
		let caps = gst::Caps::builder("audio/mpeg")
			.field("mpegversion", 4i32)
			.field("stream-format", "raw")
			.field("codec_data", &codec_data)
			.build();
		set.caps("audio", 0, &caps).unwrap();
		assert!(set.pads["audio"].framed.is_some());
	}

	// Missing codec_data is a per-pad error (the factory cannot build AAC), not a session failure.
	#[test]
	fn aac_without_codec_data_fails_the_pad() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("audio", 0);
		let caps = gst::Caps::builder("audio/mpeg")
			.field("mpegversion", 4i32)
			.field("stream-format", "raw")
			.build();
		assert!(
			set.caps("audio", 0, &caps).is_ok(),
			"a missing codec_data fails the pad, not the session"
		);
		assert!(set.status.is_failed("audio", 0), "the pad is marked failed");
		assert!(!set.pads.contains_key("audio"), "the failed pad's producer is dropped");
	}

	// Per-codec specifics that caps_supported delegates: the factory fails the pad on a bad detail.
	#[test]
	fn h264_length_prefixed_avc_fails_the_pad() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("video", 0);
		// FramedFormat::Avc3 needs Annex-B, so length-prefixed avc is rejected by the factory.
		let caps = gst::Caps::builder("video/x-h264")
			.field("stream-format", "avc")
			.field("alignment", "au")
			.build();
		set.caps("video", 0, &caps).unwrap();
		assert!(set.status.is_failed("video", 0), "length-prefixed avc fails the pad");
		assert!(!set.pads.contains_key("video"));
	}

	#[test]
	fn h265_without_byte_stream_au_fails_the_pad() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("video", 0);
		set.caps("video", 0, &gst::Caps::builder("video/x-h265").build())
			.unwrap();
		assert!(
			set.status.is_failed("video", 0),
			"H.265 without byte-stream/au fails the pad"
		);
		assert!(!set.pads.contains_key("video"));
	}

	#[test]
	fn aac_wrong_mpegversion_fails_the_pad() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("audio", 0);
		let codec_data = gst::Buffer::from_slice([0x12u8, 0x10]);
		let caps = gst::Caps::builder("audio/mpeg")
			.field("mpegversion", 1i32)
			.field("stream-format", "raw")
			.field("codec_data", &codec_data)
			.build();
		set.caps("audio", 0, &caps).unwrap();
		assert!(set.status.is_failed("audio", 0), "mpegversion!=4 fails the pad");
		assert!(!set.pads.contains_key("audio"));
	}

	// CAPS then EOS with no frame: lazy importers (H.265/AV1/VP8/VP9) never created a track, so
	// finalize must be a clean no-op rather than a "not initialized" session error.
	#[test]
	fn caps_then_eos_before_first_frame_is_clean_for_lazy_codecs() {
		gst::init().unwrap();
		for media in ["video/x-h265", "video/x-av1", "video/x-vp8", "video/x-vp9"] {
			let mut set = pad_set();
			set.add_pad("video", 0);
			let mut builder = gst::Caps::builder(media);
			if media == "video/x-h265" {
				builder = builder.field("stream-format", "byte-stream").field("alignment", "au");
			}
			set.caps("video", 0, &builder.build()).unwrap();
			// EOS before any frame must be clean, not a session error.
			assert!(
				set.eos("video", 0).is_ok(),
				"{media}: CAPS->EOS with no frame must be clean"
			);
			// The pad must still count as ended (it is terminal for aggregation).
			assert!(
				set.eos("video", 0).unwrap(),
				"{media}: the EOS'd pad completes the element"
			);
		}
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
		assert!(set.status.is_failed("data", 0), "the bad pad is marked failed");
		assert!(!set.status.is_failed("video", 0), "the good pad is untouched");
		assert!(set.pads.contains_key("video"), "the good pad keeps its producer");
		assert!(!set.pads.contains_key("data"), "the failed pad's producer is dropped");
	}

	// The buffer/bitstream failure path (decode_frame error), distinct from the caps path: a malformed
	// Annex-B NAL invalidates only that pad, which is then terminal for aggregation.
	#[test]
	fn malformed_bitstream_fails_only_that_pad() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("video", 0);
		set.add_pad("audio", 0);
		set.caps("video", 0, &h264_caps()).unwrap();
		set.caps("audio", 0, &h264_caps()).unwrap();
		set.segment("video", 0, time_segment());

		// Annex-B start code + a NAL header with the forbidden_zero_bit set: a real importer error.
		let bad = Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x80]);
		assert!(
			set.buffer("video", 0, bad, Some(gst::ClockTime::ZERO)).is_ok(),
			"a bitstream error must not kill the session"
		);
		assert!(
			set.status.is_failed("video", 0),
			"the bad-bitstream pad is marked failed"
		);
		assert!(!set.status.is_failed("audio", 0), "the other pad is untouched");
		assert!(!set.pads.contains_key("video"), "the failed pad's producer is dropped");
		// The failed pad is terminal: the audio EOS completes the element.
		assert!(
			set.eos("audio", 0).unwrap(),
			"the failed video pad no longer blocks completion"
		);
	}

	// A failed pad is terminal (its track is finalized), so it stops blocking aggregation: the element
	// completes once the remaining good pads end.
	#[test]
	fn a_failed_pad_is_terminal_for_aggregation() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("video", 0);
		set.add_pad("data", 0);
		set.caps("video", 0, &h264_caps()).unwrap();
		// "data" fails on unsupported caps; the element is not complete yet (video still active).
		assert!(!set.caps("data", 0, &audio_caps()).unwrap());
		// video EOS now finishes the element: data (failed, terminal) + video (EOS) are both ended.
		assert!(
			set.eos("video", 0).unwrap(),
			"the failed pad no longer blocks completion"
		);
	}

	// Failing the last pending pad completes the element directly (caps reports completion).
	#[test]
	fn failing_the_last_pending_pad_completes_the_element() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("video", 0);
		set.add_pad("data", 0);
		set.caps("video", 0, &h264_caps()).unwrap();
		assert!(!set.eos("video", 0).unwrap(), "video ended but data is still pending");
		assert!(
			set.caps("data", 0, &audio_caps()).unwrap(),
			"failing the last pending pad completes the element"
		);
	}

	// A finalized catalog is a session failure: caps returns Err so the worker tears the session down.
	#[test]
	fn caps_after_catalog_finalized_is_a_session_error() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("video", 0);
		set.finalize_all().unwrap();
		assert!(
			set.caps("video", 0, &h264_caps()).is_err(),
			"a gone catalog is session-fatal"
		);
	}

	// A PTS that regresses within an Active timeline still emits: frames arrive in decode order and
	// B-frames carry non-monotonic presentation timestamps (moq-mux container contract). The timeline
	// itself is guarded at the SEGMENT level (the Invalid state), not per frame.
	#[test]
	fn regressing_pts_within_an_active_timeline_still_emits() {
		gst::init().unwrap();
		let mut pad = Pad::new();
		pad.set_segment(time_segment_at(0, 0));
		assert_eq!(
			pad.frame_timestamp(Some(gst::ClockTime::from_mseconds(10_000))),
			FrameDecision::Emit(10_000_000)
		);
		// A later buffer whose PTS sits below the previous one (a B-frame in decode order) still emits
		// at its own running time, rather than being dropped as a regression.
		assert_eq!(
			pad.frame_timestamp(Some(gst::ClockTime::from_mseconds(6_000))),
			FrameDecision::Emit(6_000_000)
		);
	}

	// Failures are scoped to BOTH the pad generation and the session generation, so neither a recreated
	// pad nor a recreated session (same pad name + generation across a restart) inherits a stale failure.
	#[test]
	fn failure_is_scoped_to_the_generation() {
		let status = Status::default();
		let session = status.begin_session();
		status.mark_failed(session, "video", 0);
		assert!(status.is_failed("video", 0), "the failed incarnation is failed");
		assert!(!status.is_failed("video", 1), "a newer pad incarnation is not failed");

		// A new session does not inherit the old one's failures, even for the same pad name/generation.
		let _next = status.begin_session();
		assert!(
			!status.is_failed("video", 0),
			"the failure does not carry across a session restart"
		);
		// And a now-stale session's late mark is dropped, not surfaced for the live session.
		status.mark_failed(session, "video", 0);
		assert!(!status.is_failed("video", 0), "a stale session's mark is a no-op");
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

	// The tokio property SessionHandle::stop relies on: dropping the receiver (which the worker does
	// when run_session returns) wakes a sender parked on the full channel with Err, so a stop never
	// deadlocks a chain thread applying backpressure. The element-level path needs a connected session.
	#[test]
	fn dropped_receiver_wakes_blocked_send() {
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

	// FLUSH must wake a send blocked on a full channel AND leave the receiver alive. The second half is
	// the discriminator: dropped_receiver_wakes_blocked_send proves the shutdown wake (receiver gone), so
	// a flush wake that also tore the receiver down would be indistinguishable from it.
	#[test]
	fn flush_wakes_a_blocked_send_and_keeps_the_receiver() {
		let (data_tx, mut data_rx) = mpsc::channel::<DataMsg>(DATA_CHANNEL_BOUND);
		for _ in 0..DATA_CHANNEL_BOUND {
			data_tx
				.try_send(DataMsg::AddPad {
					pad: "x".into(),
					generation: 0,
				})
				.unwrap();
		}
		let (flush_tx, flush_rx) = watch::channel(false);

		// Rendezvous so the flush is sent only after the worker is running and about to call
		// send_or_flush (else the send could see flush=true at its initial check and never block). The
		// sleep then covers parking inside reserve(), which has no test seam to observe directly.
		let started = Arc::new(std::sync::Barrier::new(2));
		let sender = data_tx.clone();
		let mut rx = flush_rx;
		let started_worker = started.clone();
		let blocked = std::thread::spawn(move || {
			started_worker.wait();
			send_or_flush(
				&sender,
				DataMsg::AddPad {
					pad: "x".into(),
					generation: 1,
				},
				&mut rx,
			)
		});
		started.wait();
		std::thread::sleep(Duration::from_millis(50)); // let the send park inside block_on

		flush_tx.send(true).unwrap();
		assert!(
			matches!(blocked.join().unwrap(), SendOutcome::Flushed),
			"flush woke the blocked send"
		);

		// The receiver is still alive (not dropped): it delivers the queued data, and a fresh non-flushing
		// send then goes through. A teardown would fail both.
		assert!(data_rx.blocking_recv().is_some(), "receiver still delivers");
		let (_gate_tx, mut rx) = watch::channel(false);
		assert!(
			matches!(
				send_or_flush(
					&data_tx,
					DataMsg::AddPad {
						pad: "x".into(),
						generation: 2
					},
					&mut rx
				),
				SendOutcome::Sent
			),
			"the receiver survived the flush and accepts new sends"
		);
	}

	// A per-pad watch means flushing pad A never cancels pad B's blocked send.
	#[test]
	fn flush_only_wakes_the_flushed_pad() {
		let (data_tx, mut data_rx) = mpsc::channel::<DataMsg>(DATA_CHANNEL_BOUND);
		for _ in 0..DATA_CHANNEL_BOUND {
			data_tx
				.try_send(DataMsg::AddPad {
					pad: "fill".into(),
					generation: 0,
				})
				.unwrap();
		}
		let (flush_a_tx, flush_a_rx) = watch::channel(false);
		let (_flush_b_tx, flush_b_rx) = watch::channel(false);

		// Both workers must be running before A is flushed, so B is genuinely inside send_or_flush (not
		// merely unspawned). The sleep then covers parking inside reserve().
		let started = Arc::new(std::sync::Barrier::new(3));
		let sender_a = data_tx.clone();
		let mut rx_a = flush_a_rx;
		let started_a = started.clone();
		let blocked_a = std::thread::spawn(move || {
			started_a.wait();
			send_or_flush(
				&sender_a,
				DataMsg::AddPad {
					pad: "a".into(),
					generation: 1,
				},
				&mut rx_a,
			)
		});
		let sender_b = data_tx.clone();
		let mut rx_b = flush_b_rx;
		let started_b = started.clone();
		let blocked_b = std::thread::spawn(move || {
			started_b.wait();
			send_or_flush(
				&sender_b,
				DataMsg::AddPad {
					pad: "b".into(),
					generation: 1,
				},
				&mut rx_b,
			)
		});
		started.wait();
		std::thread::sleep(Duration::from_millis(50));

		flush_a_tx.send(true).unwrap();
		assert!(
			matches!(blocked_a.join().unwrap(), SendOutcome::Flushed),
			"pad A's flush woke A"
		);

		// B was untouched: freeing a slot lets it send normally (Sent, not Flushed).
		assert!(data_rx.blocking_recv().is_some());
		assert!(
			matches!(blocked_b.join().unwrap(), SendOutcome::Sent),
			"pad B's send was not cancelled by A's flush"
		);
	}

	// Already-flushing contract: a pad whose watch is set drops the buffer (returns Flushed, enqueues
	// nothing) even with capacity. This exercises the initial check in send_or_flush, NOT the select's
	// biased arm; the capacity+FLUSH sub-poll tie (pitfall 14) stays structural, not covered here.
	#[test]
	fn flush_drops_the_buffer_even_with_capacity() {
		let (data_tx, mut data_rx) = mpsc::channel::<DataMsg>(DATA_CHANNEL_BOUND); // empty: capacity free
		let (flush_tx, flush_rx) = watch::channel(false);
		flush_tx.send(true).unwrap();
		let mut rx = flush_rx;
		assert!(
			matches!(
				send_or_flush(
					&data_tx,
					DataMsg::AddPad {
						pad: "x".into(),
						generation: 0
					},
					&mut rx
				),
				SendOutcome::Flushed
			),
			"a flushing send returns Flushed"
		);
		assert!(data_rx.try_recv().is_err(), "and enqueues nothing");
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

	// FLUSH re-anchor: flush drops the timeline to NoSegment, so a rewinding post-flush segment anchors
	// fresh and is accepted (Active) rather than rejected as a discontinuity. Without the flush the
	// rewind would go Invalid (see invalid_segment_drops_then_a_valid_one_recovers).
	#[test]
	fn flush_reanchors_so_a_rewinding_segment_recovers() {
		gst::init().unwrap();
		let mut pad = Pad::new();
		pad.set_segment(time_segment_at(0, 5_000));
		assert_eq!(pad.state, PadState::Active);
		assert!(matches!(
			pad.frame_timestamp(Some(gst::ClockTime::from_mseconds(10_000))),
			FrameDecision::Emit(_)
		));

		pad.flush();
		assert_eq!(pad.state, PadState::NoSegment, "flush re-anchors to NoSegment");

		// A base that rewinds below the old one is now accepted fresh, not rejected.
		pad.set_segment(time_segment_at(0, 0));
		assert_eq!(pad.state, PadState::Active, "post-flush rewinding segment is accepted");
		// And the post-flush timeline emits from the new anchor.
		assert_eq!(pad.frame_timestamp(Some(gst::ClockTime::ZERO)), FrameDecision::Emit(0));
	}

	#[test]
	fn flush_for_stale_generation_is_ignored() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("video", 1);
		set.caps("video", 1, &h264_caps()).unwrap();
		set.segment("video", 1, time_segment());
		assert_eq!(set.pads["video"].state, PadState::Active);
		// A flush for a previous incarnation must not touch the live pad.
		set.flush("video", 0);
		assert_eq!(
			set.pads["video"].state,
			PadState::Active,
			"stale-generation flush is ignored"
		);
	}

	#[test]
	fn flush_before_caps_is_a_noop() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("video", 0);
		// No CAPS yet, so no producer entry; flush must not panic or fabricate one.
		set.flush("video", 0);
		assert!(!set.pads.contains_key("video"), "flush before caps creates no producer");
	}

	// FLUSH is not EOS: the producer and its track (same name) survive a flush; only the timeline
	// re-anchors. A keyframe AU publishes the rendition first so the track name is observable.
	#[test]
	fn flush_keeps_the_producer() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("video", 0);
		set.caps("video", 0, &h264_caps()).unwrap();
		set.segment("video", 0, time_segment());
		set.buffer("video", 0, h264_keyframe_au(), Some(gst::ClockTime::ZERO))
			.unwrap();
		let before = set
			.catalog
			.as_ref()
			.unwrap()
			.snapshot()
			.video
			.renditions
			.keys()
			.next()
			.expect("a video rendition")
			.clone();

		set.flush("video", 0);

		assert!(set.pads["video"].framed.is_some(), "flush keeps the producer");
		let after = set
			.catalog
			.as_ref()
			.unwrap()
			.snapshot()
			.video
			.renditions
			.keys()
			.next()
			.cloned();
		assert_eq!(
			after.as_deref(),
			Some(before.as_str()),
			"the track name is unchanged across the flush (no catalog churn)"
		);
		assert_eq!(set.pads["video"].state, PadState::NoSegment, "the timeline re-anchored");
	}

	// The worker's re-anchor path (PadSet::flush then PadSet::segment): a rewinding post-flush segment is
	// accepted (Active), where without the flush the rewind would be rejected to Invalid.
	#[test]
	fn worker_flush_then_rewinding_segment_reanchors() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.add_pad("video", 0);
		set.caps("video", 0, &h264_caps()).unwrap();
		set.segment("video", 0, time_segment_at(0, 5_000));
		assert_eq!(set.pads["video"].state, PadState::Active);
		set.flush("video", 0);
		assert_eq!(set.pads["video"].state, PadState::NoSegment);
		// Rewinding base: accepted fresh after the flush (would go Invalid without it).
		set.segment("video", 0, time_segment_at(0, 0));
		assert_eq!(
			set.pads["video"].state,
			PadState::Active,
			"the worker accepts the post-flush rewinding segment fresh"
		);
	}

	// Decode-order frames, including B-frames, must all emit. The moq-mux container contract
	// (container/mod.rs:46-48) is "frames in DECODE order; B-frames may have non-monotonic presentation
	// timestamps", so a PTS regression is normal reordering, not an error: frame_timestamp must not drop
	// on PTS monotonicity (which would silently lose every B-frame).
	#[test]
	fn bframes_in_decode_order_all_emit() {
		gst::init().unwrap();
		let mut pad = Pad::new();
		// start=0, base=0 so running time == pts. Display order I B B B P @25fps -> DECODE order
		// I P B B B, so the PTS the sink sees is 0, 160, 40, 80, 120 ms.
		pad.set_segment(time_segment());
		let decode_order_pts_ms = [0u64, 160, 40, 80, 120];
		let emitted = decode_order_pts_ms
			.into_iter()
			.filter(|&ms| {
				matches!(
					pad.frame_timestamp(Some(gst::ClockTime::from_mseconds(ms))),
					FrameDecision::Emit(_)
				)
			})
			.count();
		assert_eq!(
			emitted, 5,
			"all five decode-order frames must emit; B-frames are not regressions (got {emitted})"
		);
	}

	// Status is a shared Arc written by every session, and stop() does not await the old task, so a stale
	// session's exit-reset can run after a new session connected. The generation token makes a stale
	// reset_on_exit a no-op, so it cannot clobber the live status (the restart-on-failure race).
	#[test]
	fn stale_session_reset_must_not_clobber_live_status() {
		let status = Arc::new(Status::default());
		let stale = status.begin_session(); // an old session's generation
		let live = status.begin_session(); // the new session's generation, now the live one
									 // The live session connects and writes the shared status.
		status.set_connected(live, true);
		status.set_version(live, Some("moq-lite-04".to_string()));
		// The old session's exit-reset runs late, but it is stale: a no-op that leaves the live status.
		assert!(
			!status.reset_on_exit(stale),
			"a stale generation must not reset the live status"
		);
		assert!(status.connected(), "the live session is still connected");
		// The live session's own exit does reset.
		assert!(status.reset_on_exit(live), "the current generation resets on exit");
		assert!(
			!status.connected(),
			"after the live session exits, the status is disconnected"
		);
	}

	// Not just the exit-reset: a stale session's connected/version/bitrate writes must also be no-ops, or
	// an old task that lingers (or wins its connect late) would clobber the live session's status.
	#[test]
	fn stale_session_writes_are_dropped() {
		let status = Arc::new(Status::default());
		let stale = status.begin_session();
		let live = status.begin_session(); // now the live generation
		status.set_connected(live, true);
		status.set_send_bitrate(live, 1000);
		assert!(status.connected());

		// Stale writes from the old generation are dropped, leaving the live status intact.
		status.set_connected(stale, false);
		status.set_version(stale, Some("ghost".to_string()));
		status.set_send_bitrate(stale, 999);
		assert!(status.connected(), "stale set_connected ignored");
		assert_ne!(status.version().as_deref(), Some("ghost"), "stale set_version ignored");
		assert_eq!(status.send_bitrate(), 1000, "stale set_send_bitrate ignored");
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
