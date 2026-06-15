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

pub static CAT: LazyLock<gst::DebugCategory> = LazyLock::new(|| {
	gst::DebugCategory::new(
		"moq-sink-spike",
		gst::DebugColorFlags::empty(),
		Some("MoQ Sink spike"),
	)
});

/// Handoff, not a buffer: a full channel must block the streaming thread, not grow.
const DATA_CHANNEL_BOUND: usize = 8;

/// Read by the element's getters without touching the task; reset on every exit.
#[derive(Default)]
pub struct Status {
	connected: AtomicBool,
	version: Mutex<Option<String>>,
	send_bitrate: AtomicU64,
}

impl Status {
	fn set_connected(&self, value: bool) {
		self.connected.store(value, Ordering::Relaxed);
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
pub enum DataMsg {
	Caps { pad: String, caps: gst::Caps },
	Segment { pad: String, segment: gst::Segment },
	Buffer { pad: String, data: Bytes, pts: Option<gst::ClockTime> },
	Eos { pad: String },
	DropPad { pad: String },
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
	pub fn start(settings: ResolvedSettings, status: Arc<Status>, element: glib::WeakRef<Element>) -> Self {
		let (data_tx, data_rx) = mpsc::channel(DATA_CHANNEL_BOUND);
		let (shutdown_tx, shutdown_rx) = watch::channel(false);

		let join = RUNTIME.spawn(async move {
			// Only a remote close reaches the bus as an error; a local shutdown returns Ok and stays quiet.
			if let Err(err) = run_session(settings, status, data_rx, shutdown_rx, element.clone()).await {
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

	let mut pad_set = PadSet::new(broadcast, catalog);
	let result = run_loop(session, &mut data, &mut shutdown, &mut pad_set, &element, &status).await;

	// Finalize every live producer once on the way out, catalog last; runs on every exit path.
	let finalized = pad_set.finalize_all();
	gst::debug!(CAT, "finalized on exit: {finalized:?}");
	// Reset the whole observable surface on exit, not just connected.
	status.set_connected(false);
	status.set_version(None);
	status.set_send_bitrate(0);
	notify_connected(&element);
	result
}

/// Only on the connect/disconnect edges, never per sample.
fn notify_connected(element: &glib::WeakRef<Element>) {
	if let Some(obj) = element.upgrade() {
		obj.notify("connected");
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
				Some(DataMsg::Caps { pad, caps }) => pad_set.caps(&pad, &caps)?,
				Some(DataMsg::Segment { pad, segment }) => pad_set.segment(&pad, segment),
				Some(DataMsg::Buffer { pad, data, pts }) => pad_set.buffer(&pad, data, pts)?,
				Some(DataMsg::Eos { pad }) => {
					if pad_set.eos(&pad)? {
						gst::info!(CAT, "all pads ended, posting EOS");
						if let Some(obj) = element.upgrade() {
							let _ = obj.post_message(gst::message::Eos::builder().src(&obj).build());
						}
						return Ok(());
					}
				}
				Some(DataMsg::DropPad { pad }) => pad_set.drop_pad(&pad),
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
	pads: HashMap<String, Pad>,
	eos: HashSet<String>,
}

impl PadSet {
	fn new(broadcast: moq_net::BroadcastProducer, catalog: moq_mux::catalog::Producer) -> Self {
		Self {
			broadcast,
			catalog: Some(catalog),
			pads: HashMap::new(),
			eos: HashSet::new(),
		}
	}

	fn caps(&mut self, pad: &str, caps: &gst::Caps) -> Result<()> {
		let broadcast = self.broadcast.clone();
		let catalog = self.catalog.clone().context("catalog already finalized")?;
		self.pads
			.entry(pad.to_string())
			.or_insert_with(Pad::new)
			.set_caps(broadcast, catalog, caps)
	}

	fn segment(&mut self, pad: &str, segment: gst::Segment) {
		// SEGMENT may arrive before CAPS (independent sticky events); this only records timing.
		self.pads.entry(pad.to_string()).or_insert_with(Pad::new).set_segment(segment);
	}

	fn buffer(&mut self, pad: &str, data: Bytes, pts: Option<gst::ClockTime>) -> Result<()> {
		match self.pads.get_mut(pad) {
			Some(pad) => pad.push_buffer(data, pts),
			None => {
				gst::warning!(CAT, "buffer for unknown pad {pad}");
				Ok(())
			}
		}
	}

	/// Returns whether every pad has now ended, so the caller posts the element EOS once.
	fn eos(&mut self, pad: &str) -> Result<bool> {
		if let Some(p) = self.pads.get_mut(pad) {
			p.finalize()?;
		}
		self.eos.insert(pad.to_string());
		Ok(!self.pads.is_empty() && self.eos.len() == self.pads.len())
	}

	fn drop_pad(&mut self, pad: &str) {
		if let Some(mut p) = self.pads.remove(pad) {
			if let Err(err) = p.finalize() {
				gst::warning!(CAT, "finalize on drop {pad}: {err:?}");
			}
		}
		self.eos.remove(pad);
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

struct Pad {
	framed: Option<moq_mux::import::Framed>,
	caps: Option<gst::Caps>,
	segment_info: Option<SegmentInfo>,
	// Kept only to map a buffer PTS to a running time.
	segment: Option<gst::FormattedSegment<gst::ClockTime>>,
}

impl Pad {
	fn new() -> Self {
		Self {
			framed: None,
			caps: None,
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
			structure.name() == "video/x-h264",
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
		match classify_segment(self.segment_info.as_ref(), &info) {
			SegmentDecision::Accept => {
				self.segment_info = Some(info);
				self.segment = segment.downcast::<gst::ClockTime>().ok();
			}
			SegmentDecision::Reject(reason) => gst::warning!(CAT, "ignoring segment: {reason}"),
		}
	}

	/// Pure of the importer, so it can be tested with real segments and no codec.
	fn frame_timestamp(&self, pts: Option<gst::ClockTime>) -> FrameDecision {
		let running_time = self
			.segment
			.as_ref()
			.zip(pts)
			.and_then(|(segment, pts)| segment.to_running_time(pts))
			.map(|time| time.nseconds());
		frame_micros(self.segment_info.is_some(), running_time)
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

fn segment_info(segment: &gst::Segment) -> SegmentInfo {
	match segment.downcast_ref::<gst::ClockTime>() {
		Some(time) => SegmentInfo {
			time_format: true,
			rate: time.rate(),
			start_nanos: time.start().map(|c| c.nseconds()).unwrap_or(0),
			base_nanos: time.base().map(|c| c.nseconds()).unwrap_or(0),
		},
		None => SegmentInfo {
			time_format: false,
			rate: segment.rate(),
			start_nanos: 0,
			base_nanos: 0,
		},
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;

	fn pad_set() -> PadSet {
		let mut broadcast = moq_net::Broadcast::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();
		PadSet::new(broadcast, catalog)
	}

	fn h264_caps() -> gst::Caps {
		gst::Caps::builder("video/x-h264")
			.field("stream-format", "byte-stream")
			.field("alignment", "au")
			.build()
	}

	// EOS/new-caps/drop/shutdown converge on exactly one finalize per producer; catalog last.
	#[test]
	fn finalize_all_finishes_pads_then_catalog_once() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.caps("video", &h264_caps()).unwrap();
		set.caps("audio", &h264_caps()).unwrap();

		let order = set.finalize_all();
		assert_eq!(order.last().map(String::as_str), Some("catalog"), "catalog must finalize last");
		assert!(order.contains(&"video".to_string()) && order.contains(&"audio".to_string()));

		// A second pass finalizes nothing again.
		assert!(set.finalize_all().is_empty());
	}

	#[test]
	fn eos_then_shutdown_does_not_double_finalize() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.caps("video", &h264_caps()).unwrap();

		assert!(set.eos("video").unwrap());
		// Only the catalog is left; the pad is not finalized twice.
		assert_eq!(set.finalize_all(), vec!["catalog".to_string()]);
	}

	#[test]
	fn identical_caps_keep_one_live_producer() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.caps("video", &h264_caps()).unwrap();
		// Re-sent identical caps are a no-op; the pad still holds exactly one live producer.
		set.caps("video", &h264_caps()).unwrap();
		assert_eq!(set.pads.len(), 1);
		assert!(set.pads["video"].framed.is_some());
	}

	#[test]
	fn buffer_for_unknown_pad_is_dropped_without_error() {
		let mut set = pad_set();
		assert!(set.buffer("ghost", Bytes::from_static(b"x"), Some(gst::ClockTime::ZERO)).is_ok());
	}

	// SEGMENT before CAPS: the pad is created and the segment retained when CAPS arrives.
	#[test]
	fn segment_before_caps_is_retained() {
		gst::init().unwrap();
		let mut set = pad_set();
		set.segment("video", time_segment());
		set.caps("video", &h264_caps()).unwrap();

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

	// Two pads, real PTS via to_running_time: the A/V offset survives because running time is shared.
	#[test]
	fn two_pads_keep_av_aligned_through_real_segments() {
		gst::init().unwrap();
		let mut video = Pad::new();
		let mut audio = Pad::new();
		video.set_segment(time_segment());
		audio.set_segment(time_segment());

		assert_eq!(video.frame_timestamp(Some(gst::ClockTime::from_mseconds(7))), FrameDecision::Emit(7_000));
		assert_eq!(audio.frame_timestamp(Some(gst::ClockTime::from_mseconds(5))), FrameDecision::Emit(5_000));
	}

	fn time_segment() -> gst::Segment {
		let mut segment = gst::FormattedSegment::<gst::ClockTime>::new();
		segment.set_start(gst::ClockTime::ZERO);
		segment.upcast()
	}
}
