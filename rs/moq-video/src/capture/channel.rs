//! An async, latest-frame channel shared by every capture backend.
//!
//! Backends produce frames from a foreign thread (the macOS delegate dispatch
//! queue, or the V4L2 / Media Foundation pump thread) via the synchronous
//! [`push`](FrameChannel::push); the encode loop consumes them with the async
//! [`recv`](FrameChannel::recv). Because `recv` is a real `.await`, dropping the
//! capture future cancels it promptly, which is what makes capture cancel-safe:
//! the [`Stream`](super::Stream) drops, the device is released, and no
//! blocking thread is left pinned.
//!
//! A screen delivers only when its picture changes, so its channel
//! [repeats](FrameChannel::repeating) the last frame while the screen is still.

use kio::clock::Instant;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;

use crate::Error;
use crate::frame::{Frame, Surface};
use moq_net::Timestamp;

/// The longest a still screen goes without a frame. A player shows a stall after
/// 500 ms without one, and a viewer joining mid-stillness waits for the next
/// frame to open its group. An unchanged picture encodes as a skip frame of a
/// few dozen bytes, so this costs next to nothing.
#[cfg(any(
	all(target_os = "linux", feature = "pipewire"),
	target_os = "macos",
	target_os = "windows",
	test
))]
const REPEAT: Duration = Duration::from_millis(250);

/// The producer/consumer rendezvous for a single capture session.
pub(super) struct FrameChannel {
	state: Mutex<State>,
	notify: Notify,
	epoch: Instant,
	/// How long [`recv`](Self::recv) waits for a new frame before repeating the
	/// last one, or `None` for a source that delivers on its own cadence.
	repeat: Option<Duration>,
}

/// The last frame [`recv`](FrameChannel::recv) returned, kept to repeat.
struct Held {
	/// `None` once [forgotten](FrameChannel::forget).
	surface: Option<Surface>,
	/// The latest timestamp returned, which the next frame must exceed.
	timestamp: Timestamp,
	/// When it was returned, which the next repeat waits from.
	at: Instant,
}

struct State {
	frame: Option<Frame>,
	/// Only a repeating channel holds one.
	held: Option<Held>,
	#[cfg(any(target_os = "linux", target_os = "windows", test))]
	native: Option<Native>,
	closed: bool,
	error: Option<Error>,
	/// Frames replaced before the consumer reached them.
	skipped: u64,
}

impl State {
	/// Hold `frame` as the latest, counting the one it replaces as skipped.
	fn replace(&mut self, frame: Frame) {
		if self.frame.replace(frame).is_some() {
			self.skipped += 1;
		}
	}
}

/// How a device's own timeline maps onto this stream's.
#[cfg(any(target_os = "linux", target_os = "windows", test))]
#[derive(Clone, Copy)]
struct Native {
	/// A device timestamp and the local time it was anchored to.
	source: Timestamp,
	local: Timestamp,
	/// The previous device timestamp, which the next must exceed to keep the anchor.
	last: Timestamp,
	/// The previous mapped timestamp, which a re-anchor must exceed.
	mapped: Timestamp,
}

impl FrameChannel {
	pub(super) fn new() -> Arc<Self> {
		Self::build(None)
	}

	/// A channel for a source that delivers only when its picture changes:
	/// [`recv`](Self::recv) repeats the last frame once 250 ms, or `interval` if
	/// longer, pass without a new one.
	#[cfg(any(
		all(target_os = "linux", feature = "pipewire"),
		target_os = "macos",
		target_os = "windows",
		test
	))]
	pub(super) fn repeating(interval: Duration) -> Arc<Self> {
		Self::build(Some(interval.max(REPEAT)))
	}

	fn build(repeat: Option<Duration>) -> Arc<Self> {
		Arc::new(Self {
			state: Mutex::new(State {
				frame: None,
				held: None,
				#[cfg(any(target_os = "linux", target_os = "windows", test))]
				native: None,
				closed: false,
				error: None,
				skipped: 0,
			}),
			notify: Notify::new(),
			epoch: Instant::now(),
			repeat,
		})
	}

	/// Publish the latest frame, replacing one the consumer has not reached. Safe
	/// to call from the foreign producer thread; a no-op once closed.
	pub(super) fn push(&self, frame: Surface) {
		self.push_at(frame, Instant::now());
	}

	pub(super) fn push_at(&self, surface: Surface, captured: Instant) {
		self.publish(Frame::new(surface, self.at(captured)));
	}

	/// Map a device-local timestamp into this stream's private timeline. The
	/// source epoch never escapes: its first sample is anchored to arrival.
	/// The blocking-device pump and WGC feed native timestamps; the mapping is
	/// also compiled for its host-side tests.
	///
	/// A device timeline that steps back or stalls (a driver restarting its clock
	/// at zero, or one reporting a constant) re-anchors that sample to arrival, or
	/// just past the last delivered timestamp if that is later, so the stream
	/// never rewinds or repeats a timestamp it already delivered.
	#[cfg(any(target_os = "linux", target_os = "windows", test))]
	pub(super) fn push_native(&self, surface: Surface, source: Timestamp) {
		self.push_native_at(surface, source, Instant::now());
	}

	#[cfg(any(target_os = "linux", target_os = "windows", test))]
	fn push_native_at(&self, surface: Surface, source: Timestamp, arrived: Instant) {
		let arrived = self.at(arrived);
		let mut state = self.state.lock().unwrap();
		if state.closed {
			return;
		}
		// A backlog drained faster than real time maps ahead of arrival, so a
		// re-anchor floors strictly above the last delivered timestamp. A device
		// clock that jumped to the end of the timeline leaves no room above it.
		let floor = match state.native {
			None => arrived,
			Some(native) => match native.mapped.checked_add(Timestamp::from_micros(1).unwrap()) {
				Ok(next) if next > arrived => next,
				Ok(_) => arrived,
				Err(err) => {
					drop(state);
					return self.fail(err.into());
				}
			},
		};
		let anchor = match state.native {
			Some(native) if source > native.last => native,
			_ => Native {
				source,
				local: floor,
				last: source,
				mapped: floor,
			},
		};
		let timestamp = source
			.checked_sub(anchor.source)
			.and_then(|elapsed| anchor.local.checked_add(elapsed))
			.unwrap_or(floor);
		state.native = Some(Native {
			last: source,
			mapped: timestamp,
			..anchor
		});
		state.replace(Frame::new(surface, timestamp));
		drop(state);
		self.notify.notify_one();
	}

	fn publish(&self, frame: Frame) {
		let mut state = self.state.lock().unwrap();
		if state.closed {
			return;
		}
		state.replace(frame);
		drop(state);
		self.notify.notify_one();
	}

	/// How many frames a newer one replaced before [`recv`](Self::recv) reached them.
	pub(super) fn skipped(&self) -> u64 {
		self.state.lock().unwrap().skipped
	}

	/// Stop repeating until the next frame, because the last picture no longer
	/// stands: the source resized, minimized, or blanked, or freed the buffer it
	/// lives in. A frame the consumer has not reached yet is the same stale
	/// picture, so it goes too.
	#[cfg(any(
		all(target_os = "linux", feature = "pipewire"),
		target_os = "macos",
		target_os = "windows",
		test
	))]
	pub(super) fn forget(&self) {
		let mut state = self.state.lock().unwrap();
		state.frame = None;
		if let Some(held) = &mut state.held {
			held.surface = None;
		}
	}

	/// Mark the source ended, so a parked [`recv`](Self::recv) returns `None`.
	pub(super) fn close(&self) {
		let mut state = self.state.lock().unwrap();
		state.closed = true;
		drop(state);
		self.wake();
	}

	/// End the source with an error. Any pending frame is discarded so source
	/// removal or revoked permission reaches the consumer immediately.
	pub(super) fn fail(&self, error: Error) {
		let mut state = self.state.lock().unwrap();
		if state.closed {
			return;
		}
		state.frame = None;
		state.error = Some(error);
		state.closed = true;
		drop(state);
		self.wake();
	}

	/// Wake the single consumer, retaining a permit if it has not parked yet.
	///
	/// `notify_waiters` would drop the wakeup on the floor here: `recv` builds
	/// its `Notified` before taking the lock but only registers it when the
	/// future is first polled, so a terminal state set from the producer thread
	/// in that window would leave the consumer asleep forever.
	fn wake(&self) {
		self.notify.notify_one();
	}

	/// Await the latest frame, the terminal backend error, or `None` once closed.
	/// A repeating channel returns its last frame again, stamped now, once its
	/// interval passes without a new one.
	pub(super) async fn recv(&self) -> Result<Option<Frame>, Error> {
		loop {
			// Register for a wakeup before checking, so a `push` that races the
			// check still wakes this future (tokio's documented Notify pattern).
			let notified = self.notify.notified();
			let due = {
				let mut state = self.state.lock().unwrap();
				if let Some(error) = state.error.take() {
					return Err(error);
				}
				if let Some(frame) = state.frame.take() {
					return Ok(Some(self.hold(&mut state, frame)?));
				}
				if state.closed {
					return Ok(None);
				}
				match (self.repeat, &mut state.held) {
					(
						Some(repeat),
						Some(Held {
							surface: Some(surface),
							timestamp,
							at,
						}),
					) => {
						let due = at.saturating_add(repeat);
						let now = Instant::now();
						if now >= due {
							// Stamped now rather than its capture time plus the interval, so
							// a repeat a slow consumer reached late is not delivered late.
							*timestamp = past(self.at(now), *timestamp)?;
							*at = now;
							return Ok(Some(Frame::new(
								surface.retain().expect("only a retained surface is held"),
								*timestamp,
							)));
						}
						Some(due)
					}
					_ => None,
				}
			};
			match due {
				Some(due) => drop(kio::clock::timeout_at(due, notified).await),
				None => notified.await,
			}
		}
	}

	/// Keep a copy of `frame` to repeat, if this channel repeats.
	fn hold(&self, state: &mut State, mut frame: Frame) -> Result<Frame, Error> {
		if self.repeat.is_none() {
			return Ok(frame);
		}
		// A frame captured before a repeat was stamped, but pushed after it, would
		// rewind the stream, so it goes just past the repeat instead.
		if let Some(held) = &state.held {
			frame.timestamp = past(frame.timestamp, held.timestamp)?;
		}
		state.held = Some(Held {
			surface: frame.surface.retain(),
			timestamp: frame.timestamp,
			at: Instant::now(),
		});
		Ok(frame)
	}

	pub(super) fn now(&self) -> Timestamp {
		self.at(Instant::now())
	}

	/// An instant on this stream's timeline, clamped to its epoch.
	fn at(&self, instant: Instant) -> Timestamp {
		let micros = instant.saturating_duration_since(self.epoch).as_micros();
		let micros = u64::try_from(micros).unwrap_or(u64::MAX);
		Timestamp::from_micros(micros).expect("capture timestamp fits")
	}
}

/// `timestamp`, or just past `floor` if it is not already later.
fn past(timestamp: Timestamp, floor: Timestamp) -> Result<Timestamp, Error> {
	if timestamp > floor {
		return Ok(timestamp);
	}
	Ok(floor.checked_add(Timestamp::from_micros(1).unwrap())?)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::frame::I420;

	/// A throwaway frame tagged via its width, so a test can identify which frame
	/// `recv` returned without building real pixel data.
	fn frame(id: u32) -> Surface {
		Surface::I420(I420 {
			width: id,
			height: 2,
			data: Vec::new(),
			color: None,
		})
	}

	#[tokio::test]
	async fn recv_returns_frames_in_order() {
		let chan = FrameChannel::new();
		chan.push(frame(1));
		assert_eq!(chan.recv().await.unwrap().unwrap().surface.width(), 1);
		chan.push(frame(2));
		assert_eq!(chan.recv().await.unwrap().unwrap().surface.width(), 2);
	}

	#[tokio::test]
	async fn slow_consumer_receives_only_the_latest_frame() {
		let chan = FrameChannel::new();
		for id in 1..=6 {
			chan.push(frame(id));
		}
		assert_eq!(chan.recv().await.unwrap().unwrap().surface.width(), 6);
		assert_eq!(chan.skipped(), 5);
	}

	#[tokio::test]
	async fn close_returns_none_after_the_pending_frame() {
		let chan = FrameChannel::new();
		chan.push(frame(1));
		chan.close();
		assert_eq!(chan.recv().await.unwrap().unwrap().surface.width(), 1);
		assert!(chan.recv().await.unwrap().is_none());
	}

	#[tokio::test]
	async fn failure_discards_a_pending_frame_and_surfaces_the_cause() {
		let chan = FrameChannel::new();
		chan.push(frame(1));
		chan.fail(Error::SourceUnavailable("window closed".to_string()));

		assert!(matches!(
			chan.recv().await,
			Err(Error::SourceUnavailable(reason)) if reason == "window closed"
		));
		assert!(chan.recv().await.unwrap().is_none());
	}

	/// A terminal state set while the consumer is between building its `Notified`
	/// and registering it must still wake that consumer, so the wakeup has to
	/// leave a permit behind rather than only signalling registered waiters.
	#[tokio::test]
	async fn closing_retains_a_wakeup_for_a_consumer_that_has_not_parked() {
		let chan = FrameChannel::new();
		// A permit consumed by nobody stands in for the unregistered consumer.
		chan.close();
		chan.notify.notified().await;

		let chan = FrameChannel::new();
		chan.fail(Error::SourceUnavailable("stream stopped".to_string()));
		chan.notify.notified().await;
	}

	/// Cancelling a parked `recv` (as the encode loop's `select!` does each time a
	/// frame loses the race) must not drop a wakeup: a later `recv` still sees the
	/// next frame. Frames live in the queue, not the notification, so this holds.
	#[tokio::test]
	async fn recv_is_cancel_safe() {
		let chan = FrameChannel::new();
		// Poll `recv` to Pending (registering its waker), then cancel it.
		tokio::select! {
			_ = chan.recv() => panic!("no frame pushed yet"),
			_ = std::future::ready(()) => {}
		}
		chan.push(frame(7));
		assert_eq!(chan.recv().await.unwrap().unwrap().surface.width(), 7);
	}

	#[tokio::test]
	async fn timestamp_is_captured_before_queued_delay() {
		let chan = FrameChannel::new();
		let captured = chan.epoch.saturating_add(std::time::Duration::from_millis(12));
		chan.push_at(frame(1), captured);
		assert_eq!(
			chan.recv().await.unwrap().unwrap().timestamp.as_micros().unwrap(),
			12_000
		);
	}

	#[tokio::test]
	async fn native_timestamps_keep_deltas_without_exposing_the_device_epoch() {
		let chan = FrameChannel::new();
		let first_source = Timestamp::from_micros(9_000_000).unwrap();
		chan.push_native(frame(1), first_source);
		let first = chan.recv().await.unwrap().unwrap().timestamp;
		chan.push_native(frame(2), Timestamp::from_micros(9_033_367).unwrap());
		let second = chan.recv().await.unwrap().unwrap().timestamp;
		assert_eq!(second.as_micros().unwrap() - first.as_micros().unwrap(), 33_367);
		assert!(first.as_micros().unwrap() < 9_000_000);
	}

	/// A device clock that restarts at zero mid-stream must not rewind the stream's
	/// timeline: the sample re-anchors to arrival and the device's spacing resumes from there.
	#[tokio::test]
	async fn native_timestamps_reanchor_when_the_device_clock_restarts() {
		let chan = FrameChannel::new();
		let us = |micros| Timestamp::from_micros(micros).unwrap();
		let at = |millis| chan.epoch.saturating_add(std::time::Duration::from_millis(millis));

		chan.push_native_at(frame(1), us(0), at(0));
		// Real time passes with the device clock, so the mapping stays at arrival.
		chan.push_native_at(frame(2), us(20_000), at(20));
		assert_eq!(
			chan.recv().await.unwrap().unwrap().timestamp.as_micros().unwrap(),
			20_000
		);

		chan.push_native_at(frame(3), us(0), at(25));
		let restarted = chan.recv().await.unwrap().unwrap().timestamp;
		assert_eq!(restarted.as_micros().unwrap(), 25_000, "re-anchored to arrival");

		chan.push_native_at(frame(4), us(33_000), at(58));
		let next = chan.recv().await.unwrap().unwrap().timestamp;
		assert_eq!(next.as_micros().unwrap() - restarted.as_micros().unwrap(), 33_000);
	}

	/// A backlog drained faster than real time maps ahead of arrival, so a device
	/// clock restart during the drain must re-anchor above the last delivered
	/// timestamp rather than at arrival.
	#[tokio::test]
	async fn native_timestamps_reanchor_above_a_fast_drain() {
		let chan = FrameChannel::new();
		let us = |micros| Timestamp::from_micros(micros).unwrap();
		let at = |millis| chan.epoch.saturating_add(std::time::Duration::from_millis(millis));

		chan.push_native_at(frame(1), us(0), at(0));
		chan.push_native_at(frame(2), us(40_000), at(1));
		let drained = chan.recv().await.unwrap().unwrap().timestamp;
		assert_eq!(drained.as_micros().unwrap(), 40_000);

		chan.push_native_at(frame(3), us(0), at(2));
		let restarted = chan.recv().await.unwrap().unwrap().timestamp;
		assert!(restarted > drained, "{restarted:?} rewound behind {drained:?}");

		chan.push_native_at(frame(4), us(33_000), at(3));
		let next = chan.recv().await.unwrap().unwrap().timestamp;
		assert_eq!(
			next.as_micros().unwrap() - restarted.as_micros().unwrap(),
			33_000,
			"the device's spacing resumes"
		);
	}

	/// A device clock that jumps to the last representable timestamp leaves no room
	/// for a later re-anchor, which must end the stream with an error rather than
	/// panic the pump thread and leave the consumer parked.
	#[tokio::test]
	async fn a_device_clock_at_the_end_of_the_timeline_fails_the_stream() {
		let chan = FrameChannel::new();
		let us = |micros| Timestamp::from_micros(micros).unwrap();
		let at = |millis| chan.epoch.saturating_add(std::time::Duration::from_millis(millis));

		chan.push_native_at(frame(1), us(0), at(0));
		chan.push_native_at(frame(2), us((1 << 61) - 1), at(1));
		chan.push_native_at(frame(3), us(0), at(2));

		assert!(matches!(chan.recv().await, Err(Error::TimeOverflow(_))));
		assert!(chan.recv().await.unwrap().is_none());
	}

	/// A driver that reports one constant timestamp must not stamp every frame
	/// identically: each sample falls back to its arrival.
	#[tokio::test]
	async fn a_stalled_device_clock_falls_back_to_arrival() {
		let chan = FrameChannel::new();
		let constant = Timestamp::from_micros(0).unwrap();
		let at = |millis| chan.epoch.saturating_add(std::time::Duration::from_millis(millis));

		chan.push_native_at(frame(1), constant, at(0));
		let first = chan.recv().await.unwrap().unwrap().timestamp;
		chan.push_native_at(frame(2), constant, at(5));
		let second = chan.recv().await.unwrap().unwrap().timestamp;
		assert!(second > first, "a stalled device clock repeated {first:?}");
		assert_eq!(second.as_micros().unwrap(), 5_000);
	}

	/// The frame `recv` returns, as (id, timestamp in milliseconds).
	async fn next(chan: &FrameChannel) -> (u32, u128) {
		let frame = chan.recv().await.unwrap().unwrap();
		(frame.surface.width(), frame.timestamp.as_micros().unwrap() / 1_000)
	}

	/// Whether `recv` returns anything within `wait`.
	async fn quiet(chan: &FrameChannel, wait: std::time::Duration) -> bool {
		kio::clock::timeout(wait, chan.recv()).await.is_err()
	}

	#[tokio::test(start_paused = true)]
	async fn a_still_source_repeats_at_the_floor_not_its_frame_rate() {
		let chan = FrameChannel::repeating(std::time::Duration::from_millis(16));
		chan.push(frame(1));
		assert_eq!(next(&chan).await, (1, 0));
		for repeat in 1..=4 {
			assert_eq!(next(&chan).await, (1, repeat * 250));
		}
	}

	#[tokio::test(start_paused = true)]
	async fn changes_keep_their_cadence_and_postpone_the_repeat() {
		let chan = FrameChannel::repeating(std::time::Duration::from_millis(33));
		for step in 0..10 {
			chan.push(frame(step));
			assert_eq!(next(&chan).await, (step, u128::from(step) * 33));
			tokio::time::advance(std::time::Duration::from_millis(33)).await;
		}
		// The last change was at 297 ms, so its repeat waits until 547 ms.
		assert_eq!(next(&chan).await, (9, 547));
	}

	#[tokio::test(start_paused = true)]
	async fn a_source_slower_than_the_floor_repeats_at_its_own_interval() {
		let chan = FrameChannel::repeating(std::time::Duration::from_secs(1));
		chan.push(frame(1));
		next(&chan).await;
		assert_eq!(next(&chan).await, (1, 1_000));
	}

	/// The encode loop cancels `recv` whenever demand or the bitrate target moves,
	/// which must not push the repeat back.
	#[tokio::test(start_paused = true)]
	async fn a_cancelled_recv_keeps_the_repeat_due() {
		let chan = FrameChannel::repeating(std::time::Duration::from_millis(33));
		chan.push(frame(1));
		next(&chan).await;
		for _ in 0..4 {
			assert!(quiet(&chan, std::time::Duration::from_millis(60)).await);
		}
		assert_eq!(next(&chan).await, (1, 250));
	}

	#[tokio::test(start_paused = true)]
	async fn a_forgotten_picture_waits_for_the_next_change() {
		let chan = FrameChannel::repeating(std::time::Duration::from_millis(33));
		chan.push(frame(1));
		next(&chan).await;
		assert_eq!(next(&chan).await, (1, 250));
		// Pushed but not yet read when the source invalidated it.
		chan.push(frame(9));
		chan.forget();
		assert!(quiet(&chan, std::time::Duration::from_secs(5)).await);
		chan.push(frame(2));
		assert_eq!(next(&chan).await, (2, 5_250));
		assert_eq!(next(&chan).await, (2, 5_500));
	}

	/// A frame captured just before a repeat was stamped, but pushed after it.
	#[tokio::test(start_paused = true)]
	async fn a_frame_captured_before_a_repeat_goes_after_it() {
		let chan = FrameChannel::repeating(std::time::Duration::from_millis(33));
		chan.push(frame(1));
		next(&chan).await;
		assert_eq!(next(&chan).await, (1, 250));
		chan.push_at(
			frame(2),
			chan.epoch.saturating_add(std::time::Duration::from_millis(240)),
		);
		let late = chan.recv().await.unwrap().unwrap();
		assert_eq!(late.timestamp.as_micros().unwrap(), 250_001);
		// A forgotten picture still keeps the stream from rewinding.
		chan.forget();
		chan.push_at(
			frame(3),
			chan.epoch.saturating_add(std::time::Duration::from_millis(245)),
		);
		assert_eq!(
			chan.recv().await.unwrap().unwrap().timestamp.as_micros().unwrap(),
			250_002
		);
	}

	#[tokio::test(start_paused = true)]
	async fn a_repeating_channel_still_ends() {
		let chan = FrameChannel::repeating(std::time::Duration::from_millis(33));
		chan.push(frame(1));
		next(&chan).await;
		chan.close();
		assert!(chan.recv().await.unwrap().is_none());

		let chan = FrameChannel::repeating(std::time::Duration::from_millis(33));
		chan.push(frame(1));
		next(&chan).await;
		chan.fail(Error::SourceUnavailable("window closed".to_string()));
		assert!(chan.recv().await.is_err());
	}

	#[tokio::test(start_paused = true)]
	async fn a_camera_channel_never_repeats() {
		let chan = FrameChannel::new();
		chan.push(frame(1));
		next(&chan).await;
		assert!(quiet(&chan, std::time::Duration::from_secs(5)).await);
	}

	/// A repeat is the same picture again, which the encoder must still emit as a
	/// frame for the player to see the screen as live. Keyframes still come on the
	/// join interval, which is measured in time rather than frames.
	#[tokio::test(start_paused = true)]
	#[cfg(feature = "openh264")]
	async fn repeats_reach_an_active_subscriber() {
		use crate::encode::{Config, Encoder, Kind, Producer};

		let chan = FrameChannel::repeating(std::time::Duration::from_millis(20));
		chan.push(Surface::I420(I420 {
			width: 320,
			height: 240,
			data: vec![0x80; 320 * 240 * 3 / 2],
			color: None,
		}));
		let mut config = Config::new(320, 240, crate::FrameRate::integer(50));
		config.kind = Kind::Software;
		let mut encoder = Encoder::new(&config).unwrap();
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let track = broadcast
			.create_track("video", catalog.track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = track.subscribe(None);
		let rendition = config.probe().await.unwrap();
		let container = moq_mux::catalog::hang::Container::try_from(&rendition).unwrap();
		let mut subscriber = moq_mux::container::Consumer::new(subscriber, container);
		let reserved = catalog.reserve().with_interval(config.effective_join());
		let mut producer = Producer::new(track, reserved, rendition).unwrap();
		assert!(producer.demand().wanted().is_some());

		for step in 0..10u128 {
			let frame = chan.recv().await.unwrap().unwrap();
			for encoded in encoder.encode_all(&frame).unwrap() {
				producer.publish(&encoded).unwrap();
			}
			let received = loop {
				if let moq_mux::container::Read::Frame { frame, .. } = subscriber.read().await.unwrap().unwrap() {
					break frame;
				}
			};
			assert_eq!(received.timestamp.as_micros().unwrap(), step * 250_000);
			// The default join is 2 s, eight repeats.
			assert_eq!(received.keyframe, step % 8 == 0, "frame {step}");
		}
	}
}
