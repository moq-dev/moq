//! One rendition's view of the broadcast's segments, as a `Producer`/[`Consumer`] pair.
//!
//! Segments are derived from one reference rendition's timeline: each of its records is a
//! segment, numbered by the record's sequence. The catalog watcher reads that timeline and fans
//! each record out to every rendition as a row: the segment's number and timing, plus the
//! reference record's frames. Each rendition resolves a row against its own timeline. Two things
//! read the window:
//!
//! * the HTTP serve path, synchronously, to render a media playlist and resolve a segment's
//!   frames (nothing here touches media bytes on that path); and
//! * a [`Consumer`] cursor, for a recorder that wants every segment *with its media*, in
//!   order, exactly once. `next()` waits for the next resolved row, FETCHes and transmuxes its
//!   frames (via [`Rendition`]), and yields the CMAF bytes.
//!
//! A segment is addressed by its reference's tag and its number (the `seg/{tag}.{segment}.m4s`
//! URI); the number alone is its `EXT-X-MEDIA-SEQUENCE` and the recorder cursor's position. The
//! same URI names the same span of content time on every rendition, on every edge, and after
//! every reload. A new reference lists its records under its own tag, continuing past the last
//! advertised number rather than reusing the old one's URIs.

use moq_clock::Wall;
use std::collections::VecDeque;
use std::sync::Arc;
use std::task::{Poll, ready};
use std::time::Duration;

use bytes::Bytes;
use sha2::{Digest, Sha256};

use super::{Kind, Rendition};
use crate::Result;

/// A segment as its URL names it: `seg/[{generation}.]{reference}.{sequence}.m4s`, or
/// `t{pts}` in place of the sequence for the DASH `$Time$` form. Parsed by
/// [`Route::parse`](super::Route::parse) and answered by [`Rendition::segment`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Id {
	/// The publisher run the URL names, if the broadcaster carries one.
	pub generation: Option<String>,
	/// The tag of the reference rendition whose records number the segment.
	pub reference: String,
	/// Where the segment sits on that reference.
	pub at: At,
}

/// Where a segment sits on its reference rendition: two names for the same bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
	/// The segment's aligned number, as HLS playlists address it.
	Sequence(u64),
	/// The segment's timeline pts, as the DASH manifest addresses it.
	Pts(u64),
}

impl Id {
	/// Parse a segment file name without its `.m4s` extension.
	pub(crate) fn parse(stem: &str) -> Option<Self> {
		let (generation, reference, at) = match stem.split('.').collect::<Vec<_>>().as_slice() {
			[reference, at] => (None, reference.to_string(), *at),
			[generation, reference, at] => (Some(generation.to_string()), reference.to_string(), *at),
			_ => return None,
		};
		let at = match at.strip_prefix('t') {
			Some(pts) => At::Pts(pts.parse().ok()?),
			None => At::Sequence(at.parse().ok()?),
		};
		Some(Self {
			generation,
			reference,
			at,
		})
	}
}

/// The producing side of a rendition's timeline window.
///
/// The catalog watcher appends rows via [`push`](Self::push) and marks the stream
/// [`end`](Self::end)ed. Cheap to share behind an `Arc`; the window state lives in a
/// [`kio::Producer`] so a [`Consumer`] can await changes without a separate signal.
pub(crate) struct Producer {
	state: kio::Producer<State>,
}

struct State {
	/// Rows within the window, oldest first. Every row is a complete segment.
	rows: VecDeque<Row>,
	/// The first listed segment, or the segment that would follow an empty trimmed window.
	sequence: u64,
	/// The newest discontinuity seen, retained when the playlist window becomes empty.
	discontinuity: u64,
	/// The timeline track ended: the broadcast is over (`EXT-X-ENDLIST`).
	ended: bool,
	/// The reference timeline failed, so every listing refuses with this error.
	failed: Option<crate::Error>,
	/// Bumped whenever the window is cleared, so a latch on what it listed can tell it is stale.
	resets: u64,
	/// Total rows appended, so a cursor can detect eviction even before its first read.
	produced: u64,
}

/// Numbers the broadcast's content timeline: the sequence bumps wherever the timeline breaks.
///
/// Held once by the timeline fanout rather than per rendition, so every rendition stamps a
/// record with the same sequence even when one isn't listing rows (its sibling ended) or
/// cleared its own window.
#[derive(Default)]
pub(crate) struct Discontinuities {
	sequence: u64,
	/// The end of the last stamped record.
	last_end: Option<Duration>,
	/// Records were skipped, so the next one can't continue the timeline.
	broken: bool,
}

impl Discontinuities {
	/// The sequence of a record spanning `pts..end`, bumped if it doesn't continue the last one.
	pub fn stamp(&mut self, pts: Duration, end: Duration) -> u64 {
		// Tolerate sub-millisecond drift from timescale rounding.
		let jumped = self.last_end.is_some_and(|last| !continues(last, pts));
		if self.broken || jumped {
			self.sequence += 1;
		}
		self.broken = false;
		self.last_end = Some(end);
		self.sequence
	}

	/// Records were skipped (or a new run started): the next one starts a new discontinuity.
	pub fn interrupt(&mut self) {
		self.broken = true;
	}
}

/// Whether two content boundaries coincide within the timeline's rounding tolerance.
pub(crate) fn continues(end: Duration, pts: Duration) -> bool {
	pts.saturating_sub(end).max(end.saturating_sub(pts)) <= Duration::from_millis(1)
}

/// The most segments a playlist window lists, however short they are.
///
/// A fresh subscriber to a `moq-mux` timeline is only promised this many recent records (the
/// checkpoint each timeline group restates), so an edge that joined long ago must not list more
/// source records than one joining now. The dense-timeline
/// test fails if the two drift apart.
pub(crate) const MAX_SEGMENTS: usize = 256;

/// Whether a window of `len` rows, whose rows after the oldest span `rest`, should drop its
/// oldest: it lists more than [`MAX_SEGMENTS`], or the rest still covers `window` and still
/// holds a start (`keeps_start`, see [`starts`]). A GOP longer than the window keeps its sync
/// row, so the window stretches to that one GOP, and stays startable.
pub(crate) fn evicts(window: Duration, len: usize, rest: Duration, keeps_start: bool) -> bool {
	len > MAX_SEGMENTS || (len >= 2 && rest >= window && keeps_start)
}

/// Whether a player can start at a `reference` record: audio starts anywhere, video only at a
/// group start that is a sync point.
pub(crate) fn starts(reference: &ReferenceKey, keyframe: bool, start: &hang::timeline::Position) -> bool {
	reference.0 == Kind::Audio || (keyframe && start.frame == 0)
}

/// A reference's kind, rendition ID, and the track its timeline indexes. The track is part of it
/// because it can change under one ID, and the new track's records are another numbering.
pub(crate) type ReferenceKey = (Kind, String, String);

/// The rendition a broadcast's segment boundaries come from.
pub(crate) type Reference = Arc<ReferenceKey>;

/// The URI tag naming `reference`'s segment numbering: a hash of its kind, ID and track, so every edge
/// derives the same one and it never contains a URI separator.
pub(crate) fn tag(reference: &ReferenceKey) -> String {
	let (kind, id, track) = reference;
	// A track named by its ID hashes as before, so edges on either side of an upgrade agree.
	// An override uses a separator the ID form cannot produce after the kind, and length-prefixes
	// the ID, so no pair of names can hash the bytes of another reference.
	let digest = match id == track {
		true => Sha256::digest(format!("{}/{id}", kind.as_str())),
		false => Sha256::digest(format!("{}\0{}\0{id}{track}", kind.as_str(), id.len())),
	};
	// 32 bits: the tag only has to tell apart the references one broadcast ever switches between.
	digest[..4].iter().map(|b| format!("{b:02x}")).collect()
}

/// One playlist segment: its number, timing, and the reference record's frames.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Row {
	/// The reference timeline's record index, used to mirror exact window trims.
	pub index: u64,
	/// The reference record's sequence: its `EXT-X-MEDIA-SEQUENCE` and, with the reference's tag,
	/// its URI (`seg/{tag}.{segment}.m4s`), shared across renditions.
	pub segment: u64,
	/// The DASH period start, the reference timestamp where its handoff landed; retained
	/// even after the first row of the period is evicted.
	pub period: moq_net::Timestamp,
	/// The rendition the boundaries come from.
	pub reference: Reference,
	/// The reference record's frames, which the reference rendition serves as is.
	pub frames: std::ops::Range<hang::timeline::Position>,
	/// Whether the reference record's first frame is a sync point, where a player can start
	/// decoding (the record's `keyframe` flag).
	pub keyframe: bool,
	/// Presentation duration.
	pub duration: Duration,
	/// The segment's starting presentation timestamp.
	pub pts: moq_net::Timestamp,
	/// The segment's ending presentation timestamp (`pts + duration`), for window eviction
	/// and discontinuity detection.
	pub end: Duration,
	/// The broadcast's [`Discontinuities`] sequence for this record, shared by every rendition,
	/// so renditions mark the same breaks however many segments each one skipped.
	pub discontinuity: u64,
}

impl Row {
	/// Whether the segment starts at a group start that is a sync point, so a player can begin
	/// decoding with it.
	pub fn starts_sync(&self) -> bool {
		self.keyframe && self.frames.start.frame == 0
	}

	/// Whether a player can start at this segment (see [`starts`]).
	pub fn starts(&self) -> bool {
		starts(&self.reference, self.keyframe, &self.frames.start)
	}
}

/// A consistent read of the window, for rendering one playlist (the serve path only).
#[cfg_attr(
	not(feature = "server"),
	allow(dead_code, reason = "only the `server` feature uses it")
)]
pub(crate) struct Window {
	/// The `EXT-X-MEDIA-SEQUENCE` of the first listed segment: its source segment number, so
	/// sequence numbers line up across renditions and edges.
	pub sequence: u64,
	/// The first listed discontinuity, or the newest seen when the window is empty.
	pub discontinuity: u64,
	/// Listed segments, oldest first.
	pub segments: Vec<Row>,
	/// Whether the timeline (and so the playlist) has ended.
	pub ended: bool,
	/// Why the reference timeline failed, if it did.
	pub failed: Option<crate::Error>,
}

/// The next segment a [`Consumer`] should emit, resolved from the window.
enum Next {
	/// A segment is ready to fetch.
	Ready(Row),
	/// No further segment will ever appear (the timeline ended or failed).
	Done,
	/// Nothing new yet; wait for the next window change.
	Pending,
}

impl State {
	/// Snapshot the current window. Every row is a complete segment, so all are listed.
	#[cfg_attr(
		not(feature = "server"),
		allow(dead_code, reason = "only the `server` feature uses it")
	)]
	fn window(&self) -> Window {
		Window {
			sequence: self.sequence,
			discontinuity: self.rows.front().map_or(self.discontinuity, |row| row.discontinuity),
			segments: self.rows.iter().cloned().collect(),
			ended: self.ended,
			failed: self.failed.clone(),
		}
	}

	/// The row at a cursor's append position, refusing any lost prefix or reset.
	fn next_at(&self, position: (u64, u64)) -> Result<Next> {
		let first = self.produced - self.rows.len() as u64;
		if position.0 != self.resets || position.1 < first {
			return Err(crate::Error::RecordingLagged);
		}
		Ok(match self.rows.get((position.1 - first) as usize) {
			Some(row) => Next::Ready(row.clone()),
			None if self.ended || self.failed.is_some() => Next::Done,
			None => Next::Pending,
		})
	}
}

impl Producer {
	/// An empty window.
	pub fn new() -> Self {
		Self {
			state: kio::Producer::new(State {
				rows: VecDeque::new(),
				sequence: 0,
				discontinuity: 0,
				ended: false,
				failed: None,
				resets: 0,
				produced: 0,
			}),
		}
	}

	/// Append a row, evicting the front of the window past `window` (see [`evicts`]). Returns the window's
	/// [`resets`](Self::resets) after the push.
	pub fn push(&self, row: Row, window: Duration) -> u64 {
		let Ok(mut state) = self.state.write() else {
			return u64::MAX;
		};

		if let Some(back) = state.rows.back() {
			// A backward jump in pts or segment number means the publisher restarted its timeline;
			// the old window can't be stitched onto the new one, so start over.
			if Duration::from(row.pts) < Duration::from(back.pts) || row.segment <= back.segment {
				tracing::warn!("timeline jumped backwards; resetting the playlist window");
				state.rows.clear();
				state.resets += 1;
			}
		}
		if state.rows.is_empty() {
			state.sequence = row.segment;
		}

		state.discontinuity = row.discontinuity;
		state.rows.push_back(row);
		state.produced += 1;

		loop {
			let rest = match state.rows.get(1) {
				Some(second) => state.rows.back().unwrap().end.saturating_sub(second.pts.into()),
				None => Duration::ZERO,
			};
			let keeps_start = !state.rows[0].starts() || state.rows.iter().skip(1).any(Row::starts);
			if !evicts(window, state.rows.len(), rest, keeps_start) {
				break;
			}
			state.rows.pop_front();
		}
		state.sequence = state.rows.front().unwrap().segment;
		state.resets
	}

	/// Retire the prefix through segment `through`, so the playlist stays consecutive.
	pub fn retire(&self, through: u64) {
		if let Ok(mut state) = self.state.write() {
			state.rows.retain(|row| row.segment > through);
			state.sequence = state
				.rows
				.front()
				.map_or_else(|| state.sequence.max(through.saturating_add(1)), |row| row.segment);
		}
	}

	/// Clear every retained row after an unrecoverable gap in the source timeline.
	pub fn clear(&self) {
		if let Ok(mut state) = self.state.write() {
			state.rows.clear();
			state.resets += 1;
		}
	}

	/// How many times the window was cleared: by [`clear`](Self::clear), or a timeline that
	/// jumped backwards.
	pub fn resets(&self) -> u64 {
		self.state.read().resets
	}

	/// Mark the timeline finished: the playlist gets `EXT-X-ENDLIST` and cursors end once
	/// drained.
	pub fn end(&self) {
		if let Ok(mut state) = self.state.write() {
			state.ended = true;
		}
	}

	/// Mark the reference timeline failed with `err`: every listing refuses with it instead of
	/// ending, and cursors return it once drained. No row is pushed after.
	pub fn fail(&self, err: crate::Error) {
		if let Ok(mut state) = self.state.write() {
			state.failed.get_or_insert(err);
		}
	}

	/// Whether the timeline finished.
	pub fn is_ended(&self) -> bool {
		self.state.read().ended
	}

	/// Why the reference timeline failed, if it did.
	pub fn failure(&self) -> Option<crate::Error> {
		self.state.read().failed.clone()
	}

	/// Close the channel: no more rows will arrive. A [`Consumer`] drains the segments it
	/// can still see and then errors unless [`end`](Self::end) or [`fail`](Self::fail) confirmed
	/// how it ended. The serve path keeps reading the frozen window.
	pub fn close(&self) {
		let _ = self.state.close();
	}

	/// The start of the oldest listed segment, without copying the window.
	pub fn oldest(&self) -> Option<Duration> {
		self.state.read().rows.front().map(|row| row.pts.into())
	}

	/// Snapshot the current window (serve path).
	#[cfg_attr(
		not(feature = "server"),
		allow(dead_code, reason = "only the `server` feature uses it")
	)]
	pub fn window(&self) -> Window {
		self.state.read().window()
	}

	/// The row of segment `segment` numbered by the reference tagged `tag`, or `None` if it isn't
	/// in the window.
	pub fn row(&self, tag: &str, segment: u64) -> Option<Row> {
		let state = self.state.read();
		state
			.rows
			.iter()
			.find(|r| r.segment == segment && self::tag(&r.reference) == tag)
			.cloned()
	}

	/// The number of the segment whose `pts` is exactly `time` in the timeline's timescale
	/// (DASH `$Time$` addressing), or `None` when no listed row starts there. Exact match is
	/// safe because the rendered `S@t` and this lookup convert the same [`Row::pts`] the same
	/// way.
	pub fn segment_number_at(&self, tag: &str, time: u64, timescale: moq_net::Timescale) -> Option<u64> {
		let state = self.state.read();
		state
			.rows
			.iter()
			.find(|row| row.pts.as_scale(timescale) == time as u128 && self::tag(&row.reference) == tag)
			.map(|row| row.segment)
	}

	/// The newest group of `reference` a row starts on a keyframe, used to bootstrap an init
	/// segment for inline-parameter-set codecs.
	pub fn latest_keyframe_group(&self, reference: &ReferenceKey) -> Option<u64> {
		let state = self.state.read();
		state
			.rows
			.iter()
			.rev()
			.filter(|row| *row.reference == *reference)
			.find(|row| row.starts_sync())
			.map(|row| row.frames.start.group)
	}

	/// Whether the playlist has anything to serve yet (at least one segment, or the broadcast
	/// already ended).
	#[cfg_attr(
		not(feature = "server"),
		allow(dead_code, reason = "only the `server` feature uses it")
	)]
	pub fn is_playable(&self) -> bool {
		let state = self.state.read();
		state.ended || !state.rows.is_empty()
	}

	/// Poll until [`is_playable`](Self::is_playable) or the reference timeline failed, for the
	/// serve path's long-poll.
	#[cfg_attr(
		not(feature = "server"),
		allow(dead_code, reason = "only the `server` feature uses it")
	)]
	pub fn poll_playable(&self, waiter: &kio::Waiter) -> Poll<()> {
		let poll = self.state.poll_ref(waiter, |state| {
			if state.ended || state.failed.is_some() || !state.rows.is_empty() {
				Poll::Ready(())
			} else {
				Poll::Pending
			}
		});
		match poll {
			// Ready, or the channel closed (no more rows will arrive): stop waiting either way.
			Poll::Ready(_) => Poll::Ready(()),
			Poll::Pending => Poll::Pending,
		}
	}

	/// Poll `f` over the listed rows, waking on the next window change while it is pending, and
	/// return the [`resets`](Self::resets) those rows belong to. A closed window stays pending: no
	/// new row can make `f` ready.
	pub fn poll_rows(&self, waiter: &kio::Waiter, mut f: impl FnMut(&VecDeque<Row>) -> Poll<()>) -> Poll<u64> {
		match self
			.state
			.poll_ref(waiter, |state| f(&state.rows).map(|()| state.resets))
		{
			Poll::Ready(Ok(resets)) => Poll::Ready(resets),
			_ => Poll::Pending,
		}
	}

	/// A cursor over segments, starting from the oldest still in the window.
	pub fn subscribe(&self, rendition: Arc<Rendition>) -> Consumer {
		let consumer = self.state.consume();
		let state = self.state.read();
		Consumer {
			state: consumer,
			rendition,
			position: (state.resets, state.produced - state.rows.len() as u64),
			feed: None,
		}
	}
}

/// A segment with its transmuxed media, yielded by a [`Consumer`].
pub struct Segment {
	/// The source segment number (its URI is `seg/{reference}.{segment}.m4s`), shared across renditions.
	pub segment: u64,
	/// The transmuxed CMAF fragment (`moof`+`mdat`), fetched on demand by [`Consumer::next`].
	pub media: Bytes,
	/// Presentation duration.
	pub duration: Duration,
	/// Wall-clock start time, when the timeline advertises an anchor.
	pub program_date_time: Option<Wall>,
	/// The absolute timeline discontinuity sequence within this broadcaster.
	///
	/// Use the first retained segment's value as `EXT-X-DISCONTINUITY-SEQUENCE`, then write
	/// `current - previous` `EXT-X-DISCONTINUITY` tags before each later segment; a skipped
	/// epoch makes that more than one. Cursors share this sequence regardless of when
	/// they start or which segments they skip. Recreating the broadcaster starts a new
	/// namespace: start a new recording or map it into a recording-wide sequence.
	pub discontinuity: u64,
}

/// A cursor over one rendition's segments, in timeline order.
///
/// Obtained from [`Rendition::segments`](super::Rendition::segments). Each
/// [`next`](Self::next) awaits the next segment, then FETCHes and transmuxes it through the
/// path the HTTP serve path uses. The cursor also holds a live subscription to the media
/// track, so each group reaches the cache as it is published and that FETCH is a hit. A
/// cursor reads every group anyway, so this costs no extra traffic, and a source without
/// FETCH (moq-lite before 05) can serve a group no other way.
pub struct Consumer {
	state: kio::Consumer<State>,
	rendition: Arc<Rendition>,
	/// The reset epoch and next append position; errors leave it unchanged.
	position: (u64, u64),
	feed: Option<Feed>,
}

/// The live subscription a [`Consumer`] holds on its rendition's media track.
struct Feed {
	/// What it subscribed through: a re-issued sibling bind gets a fresh feed.
	binding: Arc<moq_mux::Binding>,
	state: FeedState,
}

enum FeedState {
	/// Waiting for the binding to resolve the broadcast.
	Binding,
	Subscribing(moq_net::track::Subscribing),
	Live(Box<moq_net::track::Subscriber>),
	/// The track can't be subscribed; fetches fail on their own.
	Failed,
}

impl Consumer {
	/// The rendition's CMAF init segment, built once and cached; `None` until it can be built
	/// (an inline-parameter-set codec needs the first segment first).
	pub async fn init(&self) -> Result<Option<Bytes>> {
		self.rendition.latest_init().await
	}

	/// The next segment, with its media; `None` only once the rendition ends cleanly, so a
	/// recording that reads to `None` is complete.
	///
	/// Once a timeline fails (the reference's, or this rendition's own), the cursor drains the
	/// segments it still covers, then fails every call with
	/// [`TimelineFailed`](crate::Error::TimelineFailed), as [`Rendition::playlist`] does; a
	/// failed catalog likewise with [`CatalogFailed`](crate::Error::CatalogFailed). Either is
	/// final, whichever lands first. A denied referenced broadcast fails every call with its
	/// authorization error.
	///
	/// Waits for the next segment to resolve on this rendition, then FETCHes and transmuxes its
	/// frames. A gap for this rendition is skipped. Missing window rows or unavailable media
	/// fail the recording instead of silently omitting segments. Fetch/transmux errors leave
	/// the cursor in place so the caller can retry.
	/// A bound instance's retained media drains after End or Restart; a replacement's media
	/// is never read under the old rows.
	pub async fn next(&mut self) -> Result<Option<Segment>> {
		loop {
			let Some(row) = kio::wait(|waiter| self.poll_next(waiter)).await? else {
				return Ok(None);
			};
			kio::wait(|waiter| {
				// The refusal settles the binding, not this rendition's timeline, so watch both.
				self.rendition.poll_media(waiter);
				self.rendition.authorize()?;
				self.rendition.poll_resolved(waiter, &row).map(Ok::<_, crate::Error>)
			})
			.await?;
			// This rendition's timeline failed before covering the row, so no later row resolves.
			if let Some(err) = self.rendition.own_failure()
				&& self.rendition.is_failed(&row)
			{
				return Err(err);
			}
			if self.rendition.is_gap(&row) {
				self.position.1 += 1;
				continue;
			}
			let media = self
				.rendition
				.fetch(&row)
				.await?
				.ok_or(crate::Error::SegmentUnavailable { segment: row.segment })?;
			self.position.1 += 1;
			return Ok(Some(Segment {
				segment: row.segment,
				media,
				duration: row.duration,
				program_date_time: self.rendition.wall_clock(row.pts),
				discontinuity: row.discontinuity,
			}));
		}
	}

	fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Row>>> {
		self.poll_feed(waiter);
		// A refused binding never held media, so there is nothing to drain. `poll_feed` registered
		// the wake for a refusal that arrives while the window waits for rows.
		if let Err(err) = self.rendition.authorize() {
			return Poll::Ready(Err(err));
		}
		let found = match ready!(self.state.poll(waiter, |state| -> Poll<Result<Option<Row>>> {
			match state.next_at(self.position)? {
				Next::Ready(row) => Poll::Ready(Ok(Some(row))),
				Next::Done => Poll::Ready(Ok(None)),
				Next::Pending => Poll::Pending,
			}
		})) {
			Ok(found) => found?,
			Err(_) => return Poll::Ready(Err(crate::Error::IncompleteRecording)),
		};
		if found.is_some() {
			return Poll::Ready(Ok(found));
		}
		Poll::Ready(self.rendition.check().map(|()| None))
	}

	/// Hold a live subscription on the media track the rendition is bound to now.
	fn poll_feed(&mut self, waiter: &kio::Waiter) {
		// Also ends the window on an ended sibling, which nothing else may wake this cursor for.
		let binding = self.rendition.poll_media(waiter);
		let feed = match &mut self.feed {
			Some(feed) if Arc::ptr_eq(&feed.binding, &binding) => feed,
			feed => feed.insert(Feed {
				binding,
				state: FeedState::Binding,
			}),
		};
		loop {
			feed.state = match &mut feed.state {
				FeedState::Binding => match self.rendition.poll_subscribe(&feed.binding, waiter) {
					Poll::Ready(Some(subscribing)) => FeedState::Subscribing(subscribing),
					Poll::Ready(None) => FeedState::Failed,
					Poll::Pending => return,
				},
				FeedState::Subscribing(subscribing) => match subscribing.poll_ok(waiter) {
					Poll::Ready(Ok(subscriber)) => FeedState::Live(Box::new(subscriber)),
					Poll::Ready(Err(_)) => FeedState::Failed,
					Poll::Pending => return,
				},
				FeedState::Live(subscriber) => {
					// Drop each group as it arrives: the cache keeps it for the fetch, and a
					// subscription that resumed across routes only pulls while it is read.
					while let Poll::Ready(Ok(Some(_))) = subscriber.poll_recv_group(waiter) {}
					return;
				}
				FeedState::Failed => return,
			};
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn row(segment: u64, group: u64, pts_ms: u64, duration_ms: u64) -> Row {
		let pts = moq_net::Timestamp::from_millis(pts_ms).unwrap();
		Row {
			index: segment,
			segment,
			period: moq_net::Timestamp::ZERO,
			reference: Arc::new((Kind::Video, "video".to_string(), "video".to_string())),
			frames: hang::timeline::Position::group(group)..hang::timeline::Position::group(group + 1),
			keyframe: true,
			duration: Duration::from_millis(duration_ms),
			pts,
			end: Duration::from(pts) + Duration::from_millis(duration_ms),
			discontinuity: 0,
		}
	}

	#[test]
	fn every_row_is_listed() {
		let live = Producer::new();
		live.push(row(0, 0, 0, 2_000), Duration::from_secs(30));
		live.push(row(1, 1, 2_000, 2_000), Duration::from_secs(30));

		let window = live.window();
		assert_eq!(window.sequence, 0);
		assert!(!window.ended);
		assert_eq!(window.segments.len(), 2, "rows are complete segments; all are listed");
		assert_eq!(window.segments[0].segment, 0);
		assert_eq!(window.segments[0].duration, Duration::from_secs(2));

		live.end();
		assert!(live.window().ended);
	}

	#[test]
	fn window_evicts_and_advances_sequence() {
		let live = Producer::new();
		let window = Duration::from_secs(4);
		for i in 0..6u64 {
			live.push(row(i, i, i * 2_000, 2_000), window);
		}

		let snapshot = live.window();
		// Segments still cover >= 4s after eviction, and the sequence is the first listed
		// segment's aligned number.
		assert!(snapshot.sequence > 0);
		let span: Duration = snapshot.segments.iter().map(|s| s.duration).sum();
		assert!(span >= Duration::from_secs(4));
		assert_eq!(snapshot.segments.first().unwrap().segment, snapshot.sequence);
	}

	#[test]
	fn retiring_a_prefix_advances_the_playlist_sequence() {
		let live = Producer::new();
		let window = Duration::from_secs(30);
		for i in 0..4u64 {
			live.push(row(i, i, i * 2_000, 2_000), window);
		}

		live.retire(1);
		let snapshot = live.window();
		assert_eq!(snapshot.sequence, 2);
		assert_eq!(
			snapshot.segments.iter().map(|row| row.segment).collect::<Vec<_>>(),
			vec![2, 3]
		);

		live.retire(3);
		let snapshot = live.window();
		assert_eq!(snapshot.sequence, 4);
		assert!(snapshot.segments.is_empty());
	}

	#[test]
	fn a_skipped_source_range_clears_rows_before_the_next_segment() {
		let live = Producer::new();
		live.push(row(4, 4, 8_000, 2_000), Duration::from_secs(10));
		live.clear();
		live.push(row(10, 10, 20_000, 2_000), Duration::from_secs(10));

		let snapshot = live.window();
		assert_eq!(snapshot.sequence, 10);
		assert_eq!(
			snapshot.segments.iter().map(|row| row.segment).collect::<Vec<_>>(),
			vec![10]
		);
	}

	fn secs(s: u64) -> Duration {
		Duration::from_secs(s)
	}

	#[test]
	fn a_continuous_timeline_keeps_one_discontinuity() {
		// A cursor that skips a declared gap for its rendition must not mark the
		// next one: a sibling that fetched it wouldn't, and players require renditions to agree.
		let mut timeline = Discontinuities::default();
		assert_eq!(timeline.stamp(secs(0), secs(2)), 0);
		assert_eq!(timeline.stamp(secs(2), secs(4)), 0);
		assert_eq!(timeline.stamp(secs(4), secs(6)), 0);
	}

	#[test]
	fn a_content_time_jump_starts_a_new_discontinuity() {
		let mut timeline = Discontinuities::default();
		assert_eq!(timeline.stamp(secs(0), secs(2)), 0);
		assert_eq!(timeline.stamp(secs(10), secs(12)), 1);
		assert_eq!(timeline.stamp(secs(12), secs(14)), 1);
	}

	#[test]
	fn skipped_records_start_a_new_discontinuity() {
		let mut timeline = Discontinuities::default();
		assert_eq!(timeline.stamp(secs(0), secs(2)), 0);
		timeline.interrupt();
		assert_eq!(
			timeline.stamp(secs(2), secs(4)),
			1,
			"skipped records break the timeline even when the next one lines up"
		);
	}

	#[test]
	fn rows_and_the_keyframe_group() {
		let live = Producer::new();
		let window = Duration::from_secs(30);
		live.push(row(0, 0, 0, 1_000), window);
		live.push(
			Row {
				keyframe: false,
				..row(1, 1, 1_000, 1_000)
			},
			window,
		);

		let video = (Kind::Video, "video".to_string(), "video".to_string());
		let tag = tag(&video);
		assert_eq!(
			live.row(&tag, 0).unwrap().frames,
			hang::timeline::Position::group(0)..hang::timeline::Position::group(1)
		);
		assert_eq!(live.row(&tag, 7), None, "unknown segments miss");
		assert_eq!(live.row("00000000", 0), None, "another reference's numbering misses");
		assert_eq!(
			live.latest_keyframe_group(&video),
			Some(0),
			"row 1 does not start on a keyframe"
		);
		assert_eq!(
			live.latest_keyframe_group(&(Kind::Audio, "video".to_string(), "video".to_string())),
			None
		);
	}

	#[test]
	fn backwards_jump_resets_the_window() {
		let live = Producer::new();
		let window = Duration::from_secs(30);
		assert_eq!(live.push(row(0, 0, 10_000, 2_000), window), 0);
		assert_eq!(live.push(row(1, 1, 12_000, 2_000), window), 0);
		assert_eq!(live.push(row(2, 2, 1_000, 2_000), window), 1); // restart: pts rewound

		let snapshot = live.window();
		assert_eq!(snapshot.segments.len(), 1, "the window restarted at the new row");
		assert_eq!(snapshot.segments[0].segment, 2);

		// A segment number that rewinds (a restarted publisher) resets the same way.
		assert_eq!(live.push(row(0, 0, 2_000, 2_000), window), 2);
		assert_eq!(live.window().segments.len(), 1);
	}

	#[test]
	fn cursor_walks_segments() {
		let live = Producer::new();
		let window = Duration::from_secs(30);
		live.push(row(0, 0, 0, 2_000), window);
		live.push(row(1, 1, 2_000, 2_000), window);

		let Next::Ready(first) = live.state.read().next_at((0, 0)).unwrap() else {
			panic!("expected a segment");
		};
		assert_eq!(first.segment, 0);
		let next = |segment| (0, segment);
		let Next::Ready(second) = live.state.read().next_at(next(1)).unwrap() else {
			panic!("expected a segment");
		};
		assert_eq!(second.segment, 1);
		assert!(
			matches!(live.state.read().next_at(next(2)).unwrap(), Next::Pending),
			"nothing further while live"
		);

		live.end();
		assert!(matches!(live.state.read().next_at(next(2)).unwrap(), Next::Done));
	}

	#[test]
	fn cursors_refuse_lost_rows_and_resets() {
		let live = Producer::new();
		let window = Duration::from_secs(30);
		for segment in 5..8 {
			live.push(row(segment, segment, segment * 2_000, 2_000), window);
		}
		// One cursor consumed the first row, another has not read anything yet.
		live.retire(5);
		assert!(matches!(
			live.state.read().next_at((0, 0)),
			Err(crate::Error::RecordingLagged)
		));
		assert!(matches!(live.state.read().next_at((0, 1)), Ok(Next::Ready(_))));
		live.retire(7);
		assert!(matches!(
			live.state.read().next_at((0, 1)),
			Err(crate::Error::RecordingLagged)
		));
		assert!(matches!(live.state.read().next_at((0, 3)), Ok(Next::Pending)));
		live.clear();
		live.push(row(0, 0, 0, 2_000), window);
		assert!(matches!(
			live.state.read().next_at((0, 3)),
			Err(crate::Error::RecordingLagged)
		));
		assert!(matches!(live.state.read().next_at((1, 3)), Ok(Next::Ready(_))));
	}

	#[test]
	fn a_handoff_continues_the_source_numbering_under_another_tag() {
		let live = Producer::new();
		let window = Duration::from_secs(30);
		let old = row(5, 5, 10_000, 2_000);
		let old_reference = old.reference.clone();
		live.push(old, window);
		let switched = Row {
			reference: Arc::new((Kind::Video, "other".into(), "other".into())),
			period: moq_net::Timestamp::from_secs(12).unwrap(),
			..row(6, 6, 12_000, 2_000)
		};
		live.push(switched, window);
		assert_eq!(live.window().sequence, 5);
		let Next::Ready(next) = live.state.read().next_at((0, 1)).unwrap() else {
			panic!("the replacement's row follows");
		};
		assert_eq!(next.segment, 6);
		assert_ne!(tag(&next.reference), tag(&old_reference));
		assert!(matches!(live.state.read().next_at((0, 2)).unwrap(), Next::Pending));
		live.retire(5);
		assert_eq!(live.window().sequence, 6);
		assert!(live.row(&tag(&old_reference), 5).is_none());
		live.retire(6);
		assert!(live.window().segments.is_empty());
		assert_eq!(live.window().sequence, 7);
	}

	#[test]
	fn tag_encodings_never_collide() {
		let key = |id: &str, track: &str| (Kind::Video, id.to_string(), track.to_string());
		// The ID form and an override cannot share bytes, whatever the names contain.
		assert_ne!(tag(&key("a\0b", "a\0b")), tag(&key("a", "b")));
		// Nor can two overrides that split the same bytes differently.
		assert_ne!(tag(&key("ab", "c")), tag(&key("a", "bc")));
		// A track named by its ID keeps the tag it had before overrides existed.
		assert_eq!(tag(&key("video", "video")), {
			let digest = Sha256::digest("video/video");
			digest[..4].iter().map(|b| format!("{b:02x}")).collect::<String>()
		});
	}
}
