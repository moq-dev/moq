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

/// How long a superseded source group stays worth decoding: long enough for its tail to
/// arrive behind its successor's head, and no longer, since the feed is a live reader.
/// An unbounded budget asks for everything the publisher keeps, which a moq-lite-06 hop
/// delivers from its oldest cached group, so a cold start would decode the whole cache
/// before reaching the live edge.
const SUPERSEDED: std::time::Duration = std::time::Duration::from_secs(1);

/// Frames buffered per listener. A rung that falls further behind than this
/// lags out (skipping to the next group) instead of stalling the other rungs;
/// the decode task itself never blocks on a slow listener.
///
/// Sized for a whole realistic GOP (two seconds at 60 fps, four at 30): a cold
/// start decodes the cached current group as fast as the decoder runs, while the
/// rung is still opening its encoder and then encodes no faster than it decodes,
/// so anything shorter than the burst loses the first group to lag. A frame is
/// dropped from the queue once every listener has read it, so only a rung that
/// is actually behind holds this many.
const CAPACITY: usize = 128;

/// One item of the shared live feed, in stream order.
#[derive(Clone)]
pub(crate) enum Item {
	/// Source starts its publisher declared it never produces, sent ahead of the group above
	/// them. A rung mirroring the source declares them too, so a lite-07 reader of its output
	/// never waits on a pause between groups.
	Skip(std::ops::Range<u64>),
	/// A new source group started; the frames that follow belong to it. Carries its key, which
	/// the output group mirroring it shares.
	Group(u64),
	/// A decoded frame of the current group. Cloning is cheap (`Arc`), and a GPU
	/// frame stays on the GPU for every receiving rung.
	Frame(Arc<moq_video::Frame>),
	/// The current group ended cleanly, with its exclusive end if known.
	End(Option<u64>),
	/// The source track ended cleanly; no more items follow.
	Finished,
	/// The current group will never end: its source read failed, it failed to
	/// decode, or this listener fell behind and the queue rolled over. Abandon it and pick up at
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
	/// The next feed item, or `None` when the feed died mid-stream (a source
	/// error, or a decoder that would not open; a clean source end arrives as
	/// [`Item::Finished`] first).
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
	// Opened before subscribing, so a codec this host cannot decode fails here. `None` after a
	// group failed to decode, until the next group opens a fresh one.
	let mut decoder = Some(moq_video::decode::Sink::open(&inner.config, &inner.decoder).await?);

	// Starts at the live edge, and keeps a superseded group for [`SUPERSEDED`]:
	// every rung mirrors each source group as a whole, so a tail dropped because the
	// next group began costs viewers the entire output group.
	// Ascending, since this loop finishes one group before it reads the next.
	// Reading stays in arrival order: every group decodes independently from its
	// own keyframe, so there is nothing for a sequence cursor to buy.
	let subscription = moq_net::track::Subscription::default()
		.with_order(moq_net::track::Order::Ascending)
		.with_ranges([moq_net::track::Range::from(moq_net::track::Start::Live {
			min: Some(std::time::Duration::ZERO),
			max: Some(SUPERSEDED),
		})]);
	let mut subscriber = inner.broadcast.track(&inner.name)?.subscribe(subscription).await?;

	// One past the newest group read, and where the next pause may start: that group's End once
	// it ended whole, since no group starts inside a whole group's span.
	let mut after: Option<(u64, u64)> = None;
	while let Some(mut group) = subscriber.recv_group().await? {
		let sink = match &mut decoder {
			Some(sink) => sink,
			None => decoder.insert(moq_video::decode::Sink::open(&inner.config, &inner.decoder).await?),
		};

		// Sends only fail with zero receivers, which is fine: teardown aborts
		// this task at the next await anyway.
		//
		// Asked once, as the group arrives: a skip still in flight reads as unknown, which only
		// leaves the run to a fill, never declares a group that exists.
		let (from, pause) = after.unwrap_or((0, 0));
		if pause < group.start && subscriber.is_skipped(pause..group.start) {
			let _ = sender.send(Item::Skip(from..group.start));
		}
		let _ = sender.send(Item::Group(group.start));

		// A lost group is never finished short, and every rung hears of it now
		// rather than at a next group that may never come.
		let item = match decode_group(&container, sink, &mut group, sender).await {
			Ok(true) => Item::End(group.end()),
			Ok(false) => Item::Lost,
			// A corrupt packet costs its own group, not every rung's track: each group decodes
			// from its own keyframe, so the next one starts clean on a fresh decoder. Closed
			// before that one opens, so a reset never needs a second hardware session.
			Err(err) => {
				tracing::warn!(%err, group = group.start, "could not decode a source group");
				decoder = None;
				Item::Lost
			}
		};
		// A group arriving late below a newer one moves nothing back.
		let next = group.start.saturating_add(1);
		let pause = match &item {
			Item::End(Some(end)) => (*end).max(next),
			_ => next,
		};
		if after.is_none_or(|(from, _)| next > from) {
			after = Some((next, pause));
		}
		let _ = sender.send(item);
	}
	Ok(())
}

/// Decode one source group into the channel, draining the decoder at its end. Returns whether
/// the group arrived whole; a read that failed loses it, leaving the decoder empty for the next.
///
/// An error is the decoder's or the container's, after which the decoder may hold anything.
async fn decode_group(
	container: &moq_mux::catalog::hang::Container,
	decoder: &mut moq_video::decode::Sink,
	group: &mut moq_net::group::Consumer,
	sender: &broadcast::Sender<Item>,
) -> Result<bool, Error> {
	let mut first = true;
	let complete = loop {
		let wire = match group.read_frame().await {
			Ok(Some(wire)) => wire,
			Ok(None) => break true,
			// A failed group read loses that group alone, such as one its
			// publisher gave up; a failed track ends `recv_group` instead.
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
	if complete {
		while let Some(decoded) = decoder.receive()? {
			let _ = sender.send(Item::Frame(Arc::new(decoded)));
		}
	} else {
		while decoder.receive()?.is_some() {}
	}
	Ok(complete)
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A feed of a 320x240 avc3 source track named `video`, decoded in software.
	fn feed(source: &moq_net::broadcast::Producer) -> Feed {
		let mut config = VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		config.coded_width = Some(320);
		config.coded_height = Some(240);
		let decoder = crate::Config {
			decoder: moq_video::decode::Kind::Software,
			..Default::default()
		}
		.decoder();
		Feed::new(source.consume(), "video".into(), config, decoder)
	}

	/// Write `count` gray frames at 30 fps from `first` (a frame index) into `group`, opening on
	/// a keyframe.
	fn write(group: &mut moq_net::group::Producer, first: u64, count: u64) {
		let mut encoder = moq_video::encode::Encoder::new(&{
			let mut config = moq_video::encode::Config::new(320, 240, moq_video::FrameRate::new(30, 1).unwrap());
			config.kind = moq_video::encode::Kind::Software;
			config
		})
		.unwrap();
		let gray = vec![0x80u8; 320 * 240 * 4];
		for index in first..first + count {
			let surface = moq_video::Surface::rgba(&gray, moq_video::Size::new(320, 240)).unwrap();
			let timestamp = moq_net::Timestamp::from_micros(index * 33_333).unwrap();
			encoder.encode(&moq_video::Frame::new(surface, timestamp)).unwrap();
			while let Some(encoded) = encoder.receive().unwrap() {
				hang::container::Frame {
					timestamp: encoded.timestamp,
					payload: encoded.payload,
				}
				.write_to(group)
				.unwrap();
			}
		}
		encoder.flush().unwrap();
		while let Some(encoded) = encoder.receive().unwrap() {
			hang::container::Frame {
				timestamp: encoded.timestamp,
				payload: encoded.payload,
			}
			.write_to(group)
			.unwrap();
		}
	}

	/// Read one group off `listener`: its key, how many frames it carried, and whether it
	/// ended whole rather than lost.
	async fn read_group(listener: &mut Listener) -> (u64, usize, bool) {
		let Some(Item::Group(key)) = listener.recv().await else {
			panic!("expected a group to start");
		};
		let mut frames = 0;
		loop {
			match listener.recv().await.expect("the feed died") {
				Item::Frame(_) => frames += 1,
				Item::End(_) => return (key, frames, true),
				Item::Lost => return (key, frames, false),
				Item::Skip(_) | Item::Group(_) | Item::Finished => panic!("group {key} never ended"),
			}
		}
	}

	/// A cold start decodes the cached current group as fast as the decoder runs, while a rung
	/// reads nothing until its encoder opens. A whole GOP waits for it rather than lagging it
	/// out of the very group a viewer joined on.
	#[tokio::test]
	async fn a_cold_start_keeps_a_whole_cached_gop_for_a_slow_rung() {
		let source = moq_net::broadcast::Info::default().produce();
		let track = source
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let feed = feed(&source);
		// Reads nothing until the decode is through the burst, as a rung opening its encoder.
		let mut slow = feed.listen();
		// Reads eagerly, which tells when that is.
		let mut eager = feed.listen();

		// Two seconds at 30 fps, cached whole by the time the feed reads it.
		let mut group = track.create_group(0u64.into()).unwrap();
		write(&mut group, 0, 60);
		group.finish().unwrap();

		assert_eq!(read_group(&mut eager).await, (0, 60, true));
		assert_eq!(read_group(&mut slow).await, (0, 60, true), "the slow rung lost the GOP");
	}

	/// What the feed says around a source's groups, its frames left out.
	#[derive(Debug, PartialEq)]
	enum Seen {
		Skip(std::ops::Range<u64>),
		Group(u64),
		End(Option<u64>),
		Lost,
	}

	/// The next item that isn't a frame.
	async fn next_seen(listener: &mut Listener) -> Seen {
		loop {
			match listener.recv().await.expect("the feed died") {
				Item::Frame(_) => {}
				Item::Skip(groups) => return Seen::Skip(groups),
				Item::Group(key) => return Seen::Group(key),
				Item::End(end) => return Seen::End(end),
				Item::Lost => return Seen::Lost,
				Item::Finished => panic!("the source ended"),
			}
		}
	}

	/// The starts a source skips between its media-timed groups reach every listener ahead of the
	/// group above them, across a pause past a whole group's End and past a lost group too.
	#[tokio::test]
	async fn the_source_skips_reach_the_listeners() {
		const FRAME: u64 = 33_333;
		let source = moq_net::broadcast::Info::default().produce();
		let track = source
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let feed = feed(&source);
		let mut listener = feed.listen();
		let mut demand = track.demand();
		while !demand.wanted_changed().await.unwrap() {}
		let at = |index: u64| moq_net::Timestamp::from_micros(index * FRAME).unwrap();

		// Two frames, then a pause: the group ends whole well before the next one starts.
		let mut first = track.append_group(at(0)).unwrap();
		write(&mut first, 0, 2);
		first.finish_at(2 * FRAME).unwrap();
		assert_eq!(next_seen(&mut listener).await, Seen::Group(0));
		assert_eq!(next_seen(&mut listener).await, Seen::End(Some(2 * FRAME)));

		// Appending says nothing starts since the previous group, which the feed names in full.
		let mut lost = track.append_group(at(60)).unwrap();
		write(&mut lost, 60, 2);
		assert_eq!(next_seen(&mut listener).await, Seen::Skip(1..60 * FRAME));
		assert_eq!(next_seen(&mut listener).await, Seen::Group(60 * FRAME));
		lost.abort(moq_net::Error::Cancel).unwrap();
		assert_eq!(next_seen(&mut listener).await, Seen::Lost);

		let mut last = track.append_group(at(150)).unwrap();
		write(&mut last, 150, 2);
		last.finish().unwrap();
		assert_eq!(next_seen(&mut listener).await, Seen::Skip(60 * FRAME + 1..150 * FRAME));
		assert_eq!(next_seen(&mut listener).await, Seen::Group(150 * FRAME));
		assert_eq!(next_seen(&mut listener).await, Seen::End(None));

		// A group the source created without skipping below it may still have neighbors there.
		let mut unproven = track.create_group((200 * FRAME).into()).unwrap();
		write(&mut unproven, 200, 2);
		unproven.finish().unwrap();
		assert_eq!(next_seen(&mut listener).await, Seen::Group(200 * FRAME));
	}

	/// A source packet that fails to decode costs its own group: every listener hears it lost,
	/// and the next group decodes whole on a fresh decoder rather than the feed dying with it.
	#[tokio::test]
	async fn a_corrupt_packet_costs_only_its_group() {
		let source = moq_net::broadcast::Info::default().produce();
		let track = source
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let feed = feed(&source);
		let mut listener = feed.listen();

		let mut corrupt = track.create_group(0u64.into()).unwrap();
		write(&mut corrupt, 0, 2);
		// A truncated container timestamp: no hang frame at all.
		corrupt
			.write_frame(moq_net::Timestamp::from_micros(66_666).unwrap(), &[0xffu8][..])
			.unwrap();
		write(&mut corrupt, 3, 2);
		corrupt.finish().unwrap();
		let (key, _, whole) = read_group(&mut listener).await;
		assert_eq!((key, whole), (0, false), "the corrupt group ended whole");

		let mut clean = track.create_group(1u64.into()).unwrap();
		write(&mut clean, 5, 2);
		clean.finish().unwrap();
		assert_eq!(read_group(&mut listener).await, (1, 2, true));
	}
}
