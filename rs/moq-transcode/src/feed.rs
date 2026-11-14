//! The shared live decode: one source subscription and one decoder per
//! broadcast, fanned out to every rung with live demand.
//!
//! Rungs used to subscribe and decode independently, so N active rungs decoded
//! the same source N times. NVDEC throughput (and upstream bandwidth) then
//! scaled with ladder depth rather than source count. A [`Feed`] decodes each
//! group once and broadcasts the frames; each rung resizes and encodes its own
//! copy (cheap: GPU frames are refcounted).
//!
//! The decode loop runs only while at least one [`Listener`] exists: the first
//! listener subscribes to the source and opens the decoder, dropping the last
//! one tears both down. An idle broadcast costs nothing, exactly like the
//! per-rung subscriptions this replaces. That includes the source track handle
//! itself: holding one is interest in the track all the way to its publisher.

use std::sync::{Arc, Mutex};

use hang::catalog::VideoConfig;
use moq_mux::container::Container as _;
use tokio::sync::broadcast;

use crate::Error;

/// Frames buffered per listener. A rung that falls further behind than this
/// lags out (skipping to the next group) instead of stalling the other rungs;
/// the decode task itself never blocks on a slow listener.
const CAPACITY: usize = 16;

/// One item of the shared live feed, in stream order.
#[derive(Clone)]
pub(crate) enum Item {
	/// A new source group started; the frames that follow belong to it. Carries the start of the
	/// output group mirroring it (see [`crate::rung::output_start`]).
	Group(u64),
	/// A decoded frame of the current group. Cloning is cheap (`Arc`), and a GPU
	/// frame stays on the GPU for every receiving rung.
	Frame(Arc<moq_video::Frame>),
	/// The current group ended cleanly.
	End,
	/// The source track ended cleanly; no more items follow.
	Finished,
	/// The current group will never end: its source read failed, or this
	/// listener fell behind and the queue rolled over. Abandon it and pick up at
	/// the next [`Item::Group`].
	Lost,
}

/// Handle to the shared live decode of one source track. Cloning shares the
/// same underlying feed.
#[derive(Clone)]
pub(crate) struct Feed {
	inner: Arc<Inner>,
}

struct Inner {
	/// The source broadcast, whose media track is looked up only while listeners
	/// exist.
	broadcast: moq_net::broadcast::Consumer,
	/// The source media track's name.
	name: String,
	/// The source rendition's catalog entry (codec + container).
	config: VideoConfig,
	/// The decoder: which implementation, and where its frames live.
	decoder: moq_video::decode::Config,
	state: Mutex<State>,
}

#[derive(Default)]
struct State {
	listeners: usize,
	/// The running decode session's channel; `None` while idle.
	sender: Option<broadcast::Sender<Item>>,
	task: Option<tokio::task::JoinHandle<()>>,
}

impl Feed {
	pub(crate) fn new(
		broadcast: moq_net::broadcast::Consumer,
		name: String,
		config: VideoConfig,
		decoder: moq_video::decode::Config,
	) -> Self {
		Self {
			inner: Arc::new(Inner {
				broadcast,
				name,
				config,
				decoder,
				state: Mutex::new(State::default()),
			}),
		}
	}

	/// Attach to the live feed, starting the shared decode session if idle.
	/// A listener attached mid-group sees frames without their [`Item::Group`];
	/// wait for the next boundary before producing.
	pub(crate) fn listen(&self) -> Listener {
		let mut state = self.inner.state.lock().unwrap();
		state.listeners += 1;
		// A finished session may not have cleared itself yet (its final lock
		// races this one); subscribing to it would only ever yield Closed, so
		// treat it as idle and start fresh.
		if state.task.as_ref().is_some_and(|task| task.is_finished()) {
			state.sender = None;
			state.task = None;
		}
		if state.sender.is_none() {
			let (sender, _) = broadcast::channel(CAPACITY);
			state.task = Some(tokio::spawn(run(self.inner.clone(), sender.clone())));
			state.sender = Some(sender);
		}
		let receiver = state.sender.as_ref().expect("sender ensured above").subscribe();
		Listener {
			feed: self.clone(),
			receiver,
		}
	}
}

/// One rung's attachment to the shared feed. Dropping it detaches; dropping
/// the last one stops the shared subscription and decoder.
pub(crate) struct Listener {
	feed: Feed,
	receiver: broadcast::Receiver<Item>,
}

impl Listener {
	/// The next feed item, or `None` when the feed died mid-stream (a source or
	/// decode error; a clean source end arrives as [`Item::Finished`] first).
	pub(crate) async fn recv(&mut self) -> Option<Item> {
		match self.receiver.recv().await {
			Ok(item) => Some(item),
			Err(broadcast::error::RecvError::Lagged(_)) => Some(Item::Lost),
			Err(broadcast::error::RecvError::Closed) => None,
		}
	}
}

impl Drop for Listener {
	fn drop(&mut self) {
		let mut state = self.feed.inner.state.lock().unwrap();
		state.listeners -= 1;
		if state.listeners == 0 {
			// Last listener out: stop decoding and release the subscription.
			state.sender = None;
			if let Some(task) = state.task.take() {
				task.abort();
			}
		}
	}
}

/// One decode session: runs until the source ends or errors, then clears
/// itself from the feed state so the next listener starts a fresh session.
async fn run(inner: Arc<Inner>, sender: broadcast::Sender<Item>) {
	match decode(&inner, &sender).await {
		// Dropping the sender after Finished closes every listener cleanly.
		Ok(()) => {
			let _ = sender.send(Item::Finished);
		}
		// Dropping the sender without Finished reads as an error downstream.
		Err(err) => tracing::warn!(%err, "shared decode session failed"),
	}

	let mut state = inner.state.lock().unwrap();
	// Only clear if this session is still the current one: a full teardown and
	// restart may have raced this exit.
	if state.sender.as_ref().is_some_and(|s| s.same_channel(&sender)) {
		state.sender = None;
		state.task = None;
	}
}

/// Subscribe to the source and decode group for group into the channel.
/// Decodes at the stream's native size: the rungs share these frames, so
/// per-rung sizing happens on their side (`Frame::resize`).
async fn decode(inner: &Inner, sender: &broadcast::Sender<Item>) -> Result<(), Error> {
	let container = moq_mux::catalog::hang::Container::try_from(&inner.config)?;

	// A `Sink` rather than a bare `Decoder`: this loop is a spawned task holding
	// the codec across every `.await` below, so on a multi-thread runtime it would
	// migrate workers and unbalance the per-thread COM apartment the Windows
	// backend opens.
	let mut decoder = moq_video::decode::Sink::open(&inner.config, &inner.decoder).await?;

	// Starts at the live edge, but keeps a superseded group for as long as its
	// publisher does: every rung mirrors each source group as a whole, so a tail
	// dropped because the next group began costs viewers the entire output group.
	// Ascending, since this loop finishes one group before it reads the next.
	// Reading stays in arrival order: every group decodes independently from its
	// own keyframe, so there is nothing for a sequence cursor to buy.
	let subscription = moq_net::track::Subscription::default()
		.with_ranges([moq_net::track::Range::live()])
		.with_order(moq_net::track::Order::Ascending)
		.with_delay(moq_net::track::Delay::default().with_max(None));
	let mut subscriber = inner.broadcast.track(&inner.name)?.subscribe(subscription).await?;

	while let Some(mut group) = subscriber.recv_group().await? {
		let Some(start) = crate::rung::output_start(group.start, group.timescale()) else {
			tracing::debug!(
				group = group.start,
				"skipping a source group past the output's timescale"
			);
			continue;
		};
		// Sends only fail with zero receivers, which is fine: teardown aborts
		// this task at the next await anyway.
		let _ = sender.send(Item::Group(start));

		let mut first = true;
		let complete = loop {
			let wire = match group.read_frame().await {
				Ok(Some(wire)) => wire,
				Ok(None) => break true,
				// A failed group read loses that group alone, such as one its
				// publisher gave up; a failed track ends `recv_group` above instead.
				Err(err) => {
					tracing::debug!(%err, group = group.start, "lost a source group");
					break false;
				}
			};
			for frame in container.decode(wire)? {
				let timestamp = frame.timestamp;
				// The first frame of a group is a keyframe by construction. This drives
				// the decoder's gating and parameter-set injection; each rung asks its
				// own encoder for an output IDR when it opens a group.
				let keyframe = frame.keyframe || first;
				first = false;

				decoder.decode(frame.payload, timestamp, keyframe).await?;
				while let Some(decoded) = decoder.receive()? {
					let _ = sender.send(Item::Frame(Arc::new(decoded)));
				}
			}
		};
		// A buffered decoder still owns this group's tail. Drain it before the
		// boundary so those frames stay in this group rather than appearing after
		// the next group's keyframe.
		decoder.flush().await?;
		// A lost group is never finished short, and every rung hears of it now
		// rather than at a next group that may never come.
		if complete {
			while let Some(decoded) = decoder.receive()? {
				let _ = sender.send(Item::Frame(Arc::new(decoded)));
			}
			let _ = sender.send(Item::End);
		} else {
			while decoder.receive()?.is_some() {}
			let _ = sender.send(Item::Lost);
		}
	}
	Ok(())
}
