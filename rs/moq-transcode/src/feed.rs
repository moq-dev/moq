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
//! per-rung subscriptions this replaces.

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
	/// A new source group started; the frames that follow belong to it.
	Group(u64),
	/// A decoded frame of the current group. Cloning is cheap (`Arc`), and a
	/// GPU frame stays on the GPU for every receiving rung.
	Frame(Arc<moq_video::decode::Frame>),
	/// The current group ended cleanly.
	End,
	/// The source track ended cleanly; no more items follow.
	Finished,
	/// Never broadcast: synthesized by [`Listener::recv`] when this listener
	/// fell behind and the queue rolled over. Abandon the current group and
	/// pick up at the next [`Item::Group`].
	Lagged,
}

/// Handle to the shared live decode of one source track. Cloning shares the
/// same underlying feed.
#[derive(Clone)]
pub(crate) struct Feed {
	inner: Arc<Inner>,
}

struct Inner {
	/// The source media track (subscribed only while listeners exist).
	source: moq_net::track::Consumer,
	/// The source rendition's catalog entry (codec + container).
	config: VideoConfig,
	/// Which decoder implementation to use.
	decoder: moq_video::decode::Kind,
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
	pub(crate) fn new(source: moq_net::track::Consumer, config: VideoConfig, decoder: moq_video::decode::Kind) -> Self {
		Self {
			inner: Arc::new(Inner {
				source,
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
			Err(broadcast::error::RecvError::Lagged(_)) => Some(Item::Lagged),
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
	let container = moq_mux::catalog::hang::Container::try_from(&inner.config.container)?;

	let mut config = moq_video::decode::Config::new();
	config.kind = inner.decoder.clone();
	let mut decoder = moq_video::decode::Decoder::new(&inner.config, &config)?;

	// The feed serves whichever rungs are active, so there is no single
	// downstream subscription to mirror; live-edge defaults fit every rung.
	let mut subscriber = inner.source.subscribe(None).await?;

	while let Some(mut group) = subscriber.next_group().await? {
		// Sends only fail with zero receivers, which is fine: teardown aborts
		// this task at the next await anyway.
		let _ = sender.send(Item::Group(group.sequence));

		let mut first = true;
		while let Some(frames) = container.read(&mut group).await? {
			for frame in frames {
				let timestamp: u64 = frame
					.timestamp
					.as_micros()
					.try_into()
					.map_err(|_| moq_net::TimeOverflow)?;
				// The first frame of a group is a keyframe by construction.
				let keyframe = frame.keyframe || first;
				first = false;

				for decoded in decoder.decode(&frame.payload, timestamp, keyframe)? {
					let _ = sender.send(Item::Frame(Arc::new(decoded)));
				}
			}
		}
		let _ = sender.send(Item::End);
	}
	Ok(())
}
