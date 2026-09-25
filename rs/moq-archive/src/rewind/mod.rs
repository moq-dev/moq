//! Seek a track back into its recording, then follow it live again.
//!
//! A [`Rewind`] pairs a live broadcast with the broadcast replaying its recording, which a
//! [`Reader`](crate::Reader) serves (the catalog `archive` entry's `replay`). [`Rewind::seek`]
//! reads the recording's timeline, starts at the segment containing a timestamp, and FETCHes every
//! group the timeline advertises in order. Once the next group is newer than anything advertised,
//! the track subscribes to the live broadcast from that group on. The recording keeps the source's
//! group sequences, so the splice neither repeats nor rewinds a group.
//!
//! The timeline decides what is requested. A group it never advertised, one that expired before
//! its turn, or one whose FETCH is not found is skipped like any other gap.
//!
//! ```no_run
//! # async fn example(live: moq_net::broadcast::Consumer, replay: moq_net::broadcast::Consumer) -> moq_archive::Result<()> {
//! use moq_archive::Rewind;
//!
//! let timeline = hang::catalog::Archive::new(hang::timeline::DEFAULT_NAME);
//! let rewind = Rewind::new(live, replay, timeline);
//! let mut video = rewind.seek("video", moq_net::Timestamp::from_secs(30)?).await?;
//! while let Some(group) = video.next_group().await? {
//!     // Decode `group`; `video.is_live()` turns true once playback reaches the live broadcast.
//!     # drop(group);
//! }
//! # Ok(())
//! # }
//! ```

use std::task::{Poll, ready};

use hang::catalog;
use hang::timeline::Record;
use moq_json::window;
use moq_net::{Timescale, Timestamp, broadcast, group, track};

use crate::index::Index;
use crate::{Error, Result};

/// Pairs a live broadcast with the broadcast replaying its recording.
#[derive(Clone)]
pub struct Rewind {
	live: broadcast::Consumer,
	archive: broadcast::Consumer,
	timeline: catalog::Archive,
}

impl Rewind {
	/// Rewind `live` through `archive`, the broadcast replaying its recording, whose timeline
	/// `timeline` names.
	pub fn new(live: broadcast::Consumer, archive: broadcast::Consumer, timeline: catalog::Archive) -> Self {
		Self {
			live,
			archive,
			timeline,
		}
	}

	/// Read `track` from the recorded segment containing `at`, then from the live broadcast.
	///
	/// Starts at the first group of the newest retained segment starting at or before `at`, or
	/// of the oldest one when `at` predates the window. Waits for the timeline's first record; a
	/// track the timeline never names, or a timeline that ends empty, starts at the live edge.
	pub async fn seek(&self, track: &str, at: Timestamp) -> Result<Track> {
		let scale = self.timeline.timescale as u64;
		let timescale = Timescale::new(scale).map_err(|_| Error::Timescale(scale))?;
		let at = at.convert(timescale).map_err(|_| Error::Overflow)?.value();

		let subscriber = self.archive.track(&self.timeline.track)?.subscribe(None).await?;
		let config = window::ConsumerConfig::default().with_compression(true);
		let mut timeline = window::Consumer::new(subscriber, config);
		let first = timeline.next().await;

		let mut archive = Archive {
			name: track.to_string(),
			track: self.archive.track(track)?,
			timeline: Some(timeline),
			index: Index::default(),
			next: 0,
			fetching: None,
		};
		// The first event opens the window's checkpoint; the rest of it is already here.
		archive.apply(first)?;
		archive.poll_timeline(&kio::Waiter::noop())?;

		let live = self.live.track(track)?;
		let source = match archive.index.seek(track, at) {
			Some(next) => {
				archive.next = next;
				Source::Archive(archive)
			}
			None => Source::Subscribing(subscribe(&live, None)),
		};
		Ok(Track { live, source })
	}
}

/// One track read from the recording, then from the live broadcast.
pub struct Track {
	live: track::Consumer,
	source: Source,
}

// One per track, moved between states in place, so the variants' sizes don't matter.
#[allow(clippy::large_enum_variant)]
enum Source {
	Archive(Archive),
	Subscribing(Subscribing),
	Live(track::Ordered),
}

struct Subscribing {
	pending: track::Subscribing,
	/// The first group the live broadcast may deliver, or `None` for its live edge.
	floor: Option<u64>,
}

impl Track {
	/// Whether groups now come from the live broadcast rather than the recording.
	pub fn is_live(&self) -> bool {
		!matches!(self.source, Source::Archive(_))
	}

	/// Poll for the next group, each with a higher sequence than the last, without blocking.
	///
	/// Returns `None` once the live track finishes. Fails when the recording's timeline or a
	/// track errors; a group that is merely unavailable is skipped instead.
	pub fn poll_next_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		loop {
			match &mut self.source {
				Source::Archive(archive) => {
					if let Some(group) = ready!(archive.poll_next_group(waiter))? {
						return Poll::Ready(Ok(Some(group)));
					}
					self.source = Source::Subscribing(subscribe(&self.live, Some(archive.next)));
				}
				Source::Subscribing(subscribing) => {
					let mut live = ready!(subscribing.pending.poll_ok(waiter))?.ordered();
					if let Some(floor) = subscribing.floor {
						live.set_groups(floor..);
					}
					self.source = Source::Live(live);
				}
				Source::Live(live) => return Poll::Ready(Ok(ready!(live.poll_next_group(waiter))?)),
			}
		}
	}

	/// Return the next group, each with a higher sequence than the last.
	pub async fn next_group(&mut self) -> Result<Option<group::Consumer>> {
		kio::wait(|waiter| self.poll_next_group(waiter)).await
	}
}

/// Subscribe to the live track from `floor`, or from its live edge when `None`.
fn subscribe(live: &track::Consumer, floor: Option<u64>) -> Subscribing {
	let subscription = match floor {
		// Reach back as far as the publisher keeps groups, but never below the floor.
		Some(floor) => track::Subscription::default()
			.with_max_age(crate::writer::REPLAY)
			.with_start(track::Position::group(floor)),
		None => track::Subscription::default(),
	};
	Subscribing {
		pending: live.subscribe(subscription).into_inner(),
		floor,
	}
}

/// The recording side of a [`Track`]: the timeline it follows and the next group to FETCH.
struct Archive {
	name: String,
	/// The track on the replay broadcast.
	track: track::Consumer,
	/// The recording's timeline, until it ends.
	timeline: Option<window::Consumer<Record>>,
	index: Index,
	/// The lowest group not yet returned or skipped.
	next: u64,
	fetching: Option<(u64, track::Fetching)>,
}

impl Archive {
	/// The next advertised group, or `None` once `next` is past everything advertised.
	fn poll_next_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		loop {
			self.poll_timeline(waiter)?;

			let Some((sequence, fetching)) = &self.fetching else {
				let Some(sequence) = self.index.next(&self.name, self.next) else {
					return Poll::Ready(Ok(None));
				};
				let fetching = self.track.fetch_group(sequence, None).into_inner();
				self.fetching = Some((sequence, fetching));
				continue;
			};

			let sequence = *sequence;
			let result = ready!(kio::Pollable::poll(fetching, waiter));
			self.fetching = None;
			// Advertised sequences stay within the recording's id range, so this cannot overflow.
			self.next = sequence + 1;
			match result {
				Ok(group) => return Poll::Ready(Ok(Some(group))),
				// Expired or unreadable since the timeline advertised it: an ordinary gap.
				Err(moq_net::Error::NotFound) => {
					tracing::debug!(track = self.name, sequence, "skipping an unavailable archived group");
				}
				Err(err) => return Poll::Ready(Err(err.into())),
			}
		}
	}

	/// Apply every timeline event that is ready.
	fn poll_timeline(&mut self, waiter: &kio::Waiter) -> Result<()> {
		while let Some(timeline) = &mut self.timeline {
			let Poll::Ready(event) = timeline.poll_next(waiter) else {
				break;
			};
			self.apply(event)?;
		}
		Ok(())
	}

	fn apply(&mut self, event: moq_json::Result<Option<window::Event<Record>>>) -> Result<()> {
		match event.map_err(|err| Error::Timeline(err.to_string()))? {
			Some(window::Event::Push { index, value }) => self.index.push(index, &value),
			Some(window::Event::Pop(range)) => {
				self.index.pop(range);
			}
			Some(_) => {}
			// Following stops; the advertised groups stay servable.
			None => self.timeline = None,
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests;
