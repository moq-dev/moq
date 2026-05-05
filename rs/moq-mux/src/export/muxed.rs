use std::collections::HashMap;
use std::task::Poll;
use std::time::Duration;

use crate::container::{Consumer, Frame, Hang};

/// A frame returned by [`Muxed::read`] tagged with its source track.
#[derive(Clone, Debug)]
pub struct MuxedFrame {
	/// The catalog rendition name this frame came from.
	pub track: String,

	/// The decoded media frame.
	pub frame: Frame,
}

/// Merges every track in a broadcast into one timestamp-ordered stream of decoded frames.
///
/// `Muxed` subscribes to the catalog and (un)subscribes individual tracks as renditions
/// are added or removed. Each track is decoded via [`Consumer<Hang>`], so Legacy and CMAF
/// tracks can be muxed together transparently.
///
/// Frames are returned in ascending timestamp order across all tracks. This is what the
/// fMP4 wire format requires (moof/mdat fragments must appear in presentation order across
/// tracks for a player to interleave them correctly).
///
/// `with_latency` configures the per-track [`Consumer`] latency, which controls how aggressively
/// stalled groups are skipped to keep playback moving.
pub struct Muxed {
	broadcast: moq_lite::BroadcastConsumer,
	catalog: Option<crate::catalog::Consumer>,
	latency: Duration,

	/// Per-track state, keyed by rendition name.
	tracks: HashMap<String, MuxedTrack>,
}

struct MuxedTrack {
	consumer: Consumer<Hang>,

	/// The next frame buffered for this track, used to compare timestamps across tracks.
	pending: Option<Frame>,

	/// Whether the consumer has signalled end-of-track and produced no further frames.
	finished: bool,
}

impl Muxed {
	/// Build a muxer over a broadcast and its catalog.
	///
	/// The catalog drives subscription: as renditions appear in the catalog, their tracks
	/// are subscribed; as they disappear, their consumers are dropped.
	pub fn new(broadcast: moq_lite::BroadcastConsumer, catalog: crate::catalog::Consumer) -> Self {
		Self {
			broadcast,
			catalog: Some(catalog),
			latency: Duration::ZERO,
			tracks: HashMap::new(),
		}
	}

	/// Set the maximum buffering latency for each per-track [`Consumer`].
	///
	/// See [`Consumer::with_latency`] for the per-track skip behavior. Default is zero (skip
	/// aggressively).
	pub fn with_latency(mut self, latency: Duration) -> Self {
		self.latency = latency;
		self
	}

	/// Read the next frame across all tracks in timestamp order.
	///
	/// Returns `None` when the catalog and every track have ended.
	pub async fn read(&mut self) -> Result<Option<MuxedFrame>, crate::Error> {
		conducer::wait(|waiter| self.poll_read(waiter)).await
	}

	/// Poll-based variant of [`Self::read`].
	pub fn poll_read(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Option<MuxedFrame>, crate::Error>> {
		// 1. Drain catalog updates and (un)subscribe tracks accordingly.
		while let Some(catalog) = self.catalog.as_mut() {
			match catalog.poll_next(waiter)? {
				Poll::Ready(Some(snapshot)) => self.update_catalog(&snapshot)?,
				Poll::Ready(None) => {
					self.catalog = None;
					break;
				}
				Poll::Pending => break,
			}
		}

		// 2. Fill any empty pending slots by polling each consumer.
		for track in self.tracks.values_mut() {
			if track.pending.is_some() || track.finished {
				continue;
			}
			match track.consumer.poll_read(waiter) {
				Poll::Ready(Ok(Some(frame))) => track.pending = Some(frame),
				Poll::Ready(Ok(None)) => track.finished = true,
				Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
				Poll::Pending => {}
			}
		}

		// 3. Pick the track whose pending frame has the smallest timestamp.
		let mut chosen: Option<&str> = None;
		let mut chosen_ts = None;
		for (name, track) in &self.tracks {
			if let Some(frame) = &track.pending {
				let ts = frame.timestamp;
				if chosen_ts.is_none_or(|c| ts < c) {
					chosen_ts = Some(ts);
					chosen = Some(name.as_str());
				}
			}
		}

		if let Some(name) = chosen {
			let name = name.to_string();
			let track = self.tracks.get_mut(&name).unwrap();
			let frame = track.pending.take().unwrap();
			return Poll::Ready(Ok(Some(MuxedFrame { track: name, frame })));
		}

		// 4. If catalog is closed and every track is finished, we're done.
		if self.catalog.is_none() && self.tracks.values().all(|t| t.finished) {
			return Poll::Ready(Ok(None));
		}

		// 5. Drop finished tracks so the next catalog update can re-add a track of the same name.
		self.tracks.retain(|_, t| !(t.finished && t.pending.is_none()));

		Poll::Pending
	}

	fn update_catalog(&mut self, catalog: &hang::Catalog) -> Result<(), crate::Error> {
		let mut active: HashMap<String, &hang::catalog::Container> = HashMap::new();

		for (name, config) in &catalog.video.renditions {
			active.insert(name.clone(), &config.container);
		}
		for (name, config) in &catalog.audio.renditions {
			active.insert(name.clone(), &config.container);
		}

		// Add any new tracks.
		for (name, container) in &active {
			if self.tracks.contains_key(name) {
				continue;
			}
			let media: Hang = (*container).try_into()?;
			let track = self.broadcast.subscribe_track(&moq_lite::Track::new(name.clone()))?;
			let consumer = Consumer::new(track, media).with_latency(self.latency);
			self.tracks.insert(
				name.clone(),
				MuxedTrack {
					consumer,
					pending: None,
					finished: false,
				},
			);
		}

		// Remove tracks no longer in the catalog.
		self.tracks.retain(|name, _| active.contains_key(name));

		Ok(())
	}

	/// Borrow the underlying broadcast consumer.
	pub fn broadcast(&self) -> &moq_lite::BroadcastConsumer {
		&self.broadcast
	}
}
