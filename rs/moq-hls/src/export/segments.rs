//! One rendition's segments, as a `Producer`/[`Consumer`] pair.
//!
//! Each rendition numbers its media groups into segments on the broadcast's grid (see the `grid`
//! module): segment `n` holds the groups placed in slot `n`. The rendition's media watcher appends
//! a row once a later segment starts. Two things read the window:
//!
//! * the HTTP serve path, synchronously, to render a media playlist and resolve a segment's
//!   groups (nothing here touches media bytes on that path); and
//! * a [`Consumer`] cursor, for a recorder that wants every segment *with its media*, in
//!   order, exactly once. `next()` waits for the next row, FETCHes and transmuxes its groups
//!   (via [`Rendition`]), and yields the CMAF bytes.
//!
//! A segment is addressed by its track's tag and its number (the `seg/{tag}.{segment}.m4s`
//! URI); the number alone is its `EXT-X-MEDIA-SEQUENCE` and the recorder cursor's position. Each
//! rendition numbers its own segments, so a number means the same on every edge and reload of
//! that rendition, but nothing across renditions: players match renditions by timestamp.

use kio::clock::Wall;
use std::collections::VecDeque;
use std::sync::Arc;
use std::task::{Poll, ready};
use std::time::Duration;

use bytes::Bytes;

use super::Rendition;
use super::rendition::{Fetch, LoadInit};
use crate::Result;

/// A segment as its URL names it: `seg/[{generation}.]{tag}.{sequence}.m4s`, or `t{pts}` in
/// place of the sequence for the DASH `$Time$` form. Parsed by
/// [`Route::parse`](super::Route::parse) and answered by [`Rendition::segment`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Id {
	/// The publisher run the URL names, if the broadcaster carries one.
	pub generation: Option<String>,
	/// A short hash of the track numbering the segment, its interval, and its config.
	pub tag: String,
	/// Where the segment sits in that numbering.
	pub at: At,
}

/// Where a segment sits in its track's numbering: two names for the same bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
	/// The segment's number, as HLS playlists address it.
	Sequence(u64),
	/// The segment's start in milliseconds, as the DASH manifest addresses it.
	Pts(u64),
}

impl Id {
	/// Parse a segment file name without its `.m4s` extension.
	pub(crate) fn parse(stem: &str) -> Option<Self> {
		let (generation, tag, at) = match stem.split('.').collect::<Vec<_>>().as_slice() {
			[tag, at] => (None, tag.to_string(), *at),
			[generation, tag, at] => (Some(generation.to_string()), tag.to_string(), *at),
			_ => return None,
		};
		let at = match at.strip_prefix('t') {
			Some(pts) => At::Pts(pts.parse().ok()?),
			None => At::Sequence(at.parse().ok()?),
		};
		Some(Self { generation, tag, at })
	}
}

/// The producing side of a rendition's segment window.
///
/// The media watcher appends rows via [`push`](Self::push) and marks the stream
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
	/// Whether rows still arrive, and if not, why.
	phase: Phase,
	/// Bumped whenever the window is cleared, so a cursor can tell its position is stale.
	resets: u64,
	/// Total rows appended, so a cursor can detect eviction even before its first read.
	produced: u64,
}

/// Whether a window still lists new rows, and if not, why. Each later phase is final.
enum Phase {
	/// Rows arrive as the media watcher reads them.
	Live,
	/// The media track ended, or the bound sibling did: the rendition is over (`EXT-X-ENDLIST`).
	Ended,
	/// The run was replaced: the listed rows are final, so nothing appends, trims, or fails them.
	Cut,
	/// The media track or the catalog failed, so every listing refuses with this error.
	Failed(crate::Error),
}

/// The most segments a playlist window lists, however short they are.
///
/// Bounds the window however short its segments are, so an edge that joined long ago lists no
/// more than one joining now, and so a long pause lists at most this many gaps.
pub(crate) const MAX_SEGMENTS: usize = 256;

/// Whether a window of `len` rows, whose rows after the oldest span `rest`, should drop its
/// oldest: it lists more than [`MAX_SEGMENTS`], or the rest still covers `window`.
pub(crate) fn evicts(window: Duration, len: usize, rest: Duration) -> bool {
	len > MAX_SEGMENTS || (len >= 2 && rest >= window)
}

/// The URI tag naming the segments one config muxes from a track: a hash of the track name
/// and the normalized `config`, so every edge derives the same one and it never contains a URI
/// separator. A reconfigured rendition keeps its record track but muxes it differently, so its
/// segments get new URLs rather than new bytes under the old ones.
pub(crate) fn tag(track: &str, config: &[u8]) -> String {
	let mut hash = rind::digest::Context::new(&rind::digest::SHA256);
	hash.update(track.as_bytes());
	hash.update(&[0]);
	hash.update(config);
	let hash = hash.finish();
	// 32 bits: the tag only has to tell apart the configs one rendition name ever carries.
	hash.as_ref()[..4].iter().map(|b| format!("{b:02x}")).collect()
}

/// One playlist segment: its number, timing, and the groups it holds.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Row {
	/// The row's index, its segment number.
	pub index: u64,
	/// The segment's number on the grid: its `EXT-X-MEDIA-SEQUENCE` and, with the tag, its URI
	/// (`seg/{tag}.{segment}.m4s`).
	pub segment: u64,
	/// The [tag](tag) of the track numbering the segment.
	pub tag: Arc<str>,
	/// The groups the segment holds, by start.
	pub groups: super::grid::Groups,
	/// Presentation duration.
	pub duration: Duration,
	/// The segment's starting presentation timestamp.
	pub pts: moq_net::Timestamp,
	/// The segment's ending presentation timestamp (`pts + duration`), for window eviction.
	pub end: Duration,
	/// The rendition's discontinuity sequence for this segment.
	pub discontinuity: u64,
	/// Nothing to fetch: no group starts a segment in this grid slot, as after an overrun or
	/// across a pause. Listed as a gap so the numbering stays consecutive.
	pub stop: bool,
	/// Whether a later segment started after this one, so its groups are all complete. A gap has
	/// no groups.
	pub complete: bool,
	/// Where its last group's content ends (the next group's pts), when known: a fallback for timing
	/// its last sample when neither its codec nor a duration marker does.
	pub content: Option<Duration>,
}

/// A consistent read of the window, for rendering one playlist (the serve path only).
#[cfg_attr(
	not(feature = "server"),
	allow(dead_code, reason = "only the `server` feature uses it")
)]
pub(crate) struct Window {
	/// The `EXT-X-MEDIA-SEQUENCE` of the first listed segment.
	pub sequence: u64,
	/// The first listed discontinuity, or the newest seen when the window is empty.
	pub discontinuity: u64,
	/// Listed segments, oldest first.
	pub segments: Vec<Row>,
	/// Whether the media track (and so the playlist) has ended.
	pub ended: bool,
	/// Why the window failed, if it did.
	pub failed: Option<crate::Error>,
	/// Whether the window is closed, so these are all the rows it will ever list (see
	/// [`Producer::closed`]).
	pub closed: bool,
}

/// The next segment a [`Consumer`] should emit, resolved from the window.
enum Next {
	/// A segment is ready to fetch.
	Ready(Row),
	/// No further segment will ever appear (the media track ended or failed).
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
			ended: matches!(self.phase, Phase::Ended | Phase::Cut),
			failed: self.failure(),
			closed: false,
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
			None if self.live() => Next::Pending,
			None => Next::Done,
		})
	}

	fn live(&self) -> bool {
		matches!(self.phase, Phase::Live)
	}

	fn failure(&self) -> Option<crate::Error> {
		match &self.phase {
			Phase::Failed(err) => Some(err.clone()),
			_ => None,
		}
	}
}

impl Producer {
	/// An empty window whose first segment is numbered `start` or later: those before it belong
	/// to another window.
	pub fn new(start: u64) -> Self {
		Self {
			state: kio::Producer::new(State {
				rows: VecDeque::new(),
				sequence: start,
				discontinuity: 0,
				phase: Phase::Live,
				resets: 0,
				produced: 0,
			}),
		}
	}

	/// Append a row, evicting the front of the window past `window` (see [`evicts`]). A window
	/// that ended, failed, or was cut lists nothing more.
	pub fn push(&self, row: Row, window: Duration) {
		let Ok(mut state) = self.state.write() else {
			return;
		};
		if !state.live() {
			return;
		}

		if let Some(back) = state.rows.back() {
			// A backward jump in pts or segment number means the publisher restarted its timeline;
			// the old window can't be stitched onto the new one, so start over.
			if super::span(row.pts) < super::span(back.pts) || row.segment <= back.segment {
				tracing::warn!("HLS segments jumped backwards; resetting the playlist window");
				state.rows.clear();
				state.resets += 1;
			}
		}

		state.discontinuity = row.discontinuity;
		state.rows.push_back(row);
		state.produced += 1;

		loop {
			let rest = match state.rows.get(1) {
				Some(second) => state.rows.back().unwrap().end.saturating_sub(super::span(second.pts)),
				None => Duration::ZERO,
			};
			if !evicts(window, state.rows.len(), rest) {
				break;
			}
			state.rows.pop_front();
		}
		state.sequence = state.rows.front().unwrap().segment;
	}

	/// Clear every retained row, after the media timestamps went back or a new publisher run
	/// replaced them. An ended or failed window stays so; a cut window keeps what it listed.
	pub fn clear(&self) {
		if let Ok(mut state) = self.state.write()
			&& !matches!(state.phase, Phase::Cut)
		{
			state.rows.clear();
			state.resets += 1;
		}
	}

	/// Mark the media finished: the playlist gets `EXT-X-ENDLIST` and cursors end once drained.
	pub fn end(&self) {
		if let Ok(mut state) = self.state.write()
			&& state.live()
		{
			state.phase = Phase::Ended;
		}
	}

	/// Mark the window failed with `err`: every listing refuses with it instead of ending, and
	/// cursors return it once drained. No row is pushed after. The first failure stays, and a cut
	/// window keeps the rows it listed.
	pub fn fail(&self, err: crate::Error) {
		if let Ok(mut state) = self.state.write()
			&& matches!(state.phase, Phase::Live | Phase::Ended)
		{
			state.phase = Phase::Failed(err);
		}
	}

	/// End the window at the rows listed now, because another run replaced this one: the
	/// playlist gets `EXT-X-ENDLIST`, cursors drain those rows and then end, and whatever the
	/// replaced run sends later is refused. A window that already failed keeps its failure.
	pub fn cut(&self) {
		if let Ok(mut state) = self.state.write()
			&& matches!(state.phase, Phase::Live | Phase::Ended)
		{
			state.phase = Phase::Cut;
		}
		self.close();
	}

	/// Whether no further segment will ever be listed: the media ended, the window failed,
	/// or it was closed.
	#[cfg(feature = "server")]
	pub fn finished(&self) -> bool {
		let state = self.state.read();
		!state.live() || state.is_closed()
	}

	/// Whether the window still lists new rows: it has not ended, failed, or been cut.
	pub(crate) fn lists(&self) -> bool {
		self.state.read().live()
	}

	/// Why the window failed, if it did.
	pub fn failure(&self) -> Option<crate::Error> {
		self.state.read().failure()
	}

	/// Close the channel: no more rows will arrive. A [`Consumer`] drains the segments it
	/// can still see and then errors unless [`end`](Self::end) or [`fail`](Self::fail) confirmed
	/// how it ended. The serve path keeps reading the frozen window.
	pub fn close(&self) {
		let _ = self.state.close();
	}

	/// Snapshot the current window (serve path).
	#[cfg_attr(
		not(feature = "server"),
		allow(dead_code, reason = "only the `server` feature uses it")
	)]
	pub fn window(&self) -> Window {
		let state = self.state.read();
		Window {
			closed: state.is_closed(),
			..state.window()
		}
	}

	/// The row of segment `segment` numbered by the track tagged `tag`, or `None` if it
	/// isn't in the window.
	pub fn row(&self, tag: &str, segment: u64) -> Option<Row> {
		let state = self.state.read();
		state
			.rows
			.iter()
			.find(|row| row.segment == segment && &*row.tag == tag)
			.cloned()
	}

	/// The number of the segment whose `pts` is exactly `time` milliseconds (DASH `$Time$`
	/// addressing), or `None` when no listed row starts there. Exact match is safe because the
	/// rendered `S@t` and this lookup convert the same [`Row::pts`] the same way. A stop is never
	/// rendered, so it never answers, even where content resumes at its pts.
	pub fn segment_number_at(&self, tag: &str, time: u64) -> Option<u64> {
		let state = self.state.read();
		state
			.rows
			.iter()
			.find(|row| !row.stop && row.pts.as_millis() == Ok(time as u128) && &*row.tag == tag)
			.map(|row| row.segment)
	}

	/// The first groups of the newest segments holding media, to bootstrap an inline-parameter-set
	/// codec's init from (a segment starts at a group, and a group at a keyframe), in the order to
	/// try them: the newest whose first group is complete, read from cache at once, then the
	/// newest when its group may still be open.
	pub fn init_groups(&self) -> Vec<u64> {
		let state = self.state.read();
		let complete = |row: &Row| row.complete || row.groups.len() > 1;
		let mut media = state.rows.iter().rev().filter(|row| !row.stop);
		let Some(newest) = media.next().and_then(|row| Some((row, row.groups.first()?))) else {
			return Vec::new();
		};
		if complete(newest.0) {
			return vec![newest.1];
		}
		let older = media.find(|row| complete(row)).and_then(|row| row.groups.first());
		older.into_iter().chain([newest.1]).collect()
	}

	/// Poll until the window lists a row, ends, fails, or closes.
	pub fn poll_listed(&self, waiter: &kio::Waiter) -> Poll<()> {
		match self
			.state
			.poll_ref(waiter, |state| match !state.rows.is_empty() || !state.live() {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}) {
			Poll::Ready(_) => Poll::Ready(()),
			Poll::Pending => Poll::Pending,
		}
	}

	/// Poll until the window lists a row, even after it closed: a predecessor whose rows its
	/// replacement still lists (see [`Rendition::after`]).
	pub fn poll_rows(&self, waiter: &kio::Waiter) -> Poll<()> {
		match self.state.poll_ref(waiter, |state| match state.rows.is_empty() {
			false => Poll::Ready(()),
			true => Poll::Pending,
		}) {
			Poll::Ready(Ok(())) => Poll::Ready(()),
			Poll::Ready(Err(state)) if !state.rows.is_empty() => Poll::Ready(()),
			_ => Poll::Pending,
		}
	}

	/// Ready once the window is closed: its media watcher lists no more rows.
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.state.poll_closed(waiter)
	}

	/// Whether the window lists segment `at` of the record track tagged `tag`.
	pub fn holds(&self, tag: &str, at: At) -> bool {
		match at {
			At::Sequence(segment) => self.row(tag, segment).is_some(),
			At::Pts(time) => self.segment_number_at(tag, time).is_some(),
		}
	}

	/// Poll until the window fails.
	#[cfg_attr(
		not(feature = "server"),
		allow(dead_code, reason = "only the `server` feature uses it")
	)]
	pub fn poll_failed(&self, waiter: &kio::Waiter) -> Poll<()> {
		match self.state.poll_ref(waiter, |state| match state.phase {
			Phase::Failed(_) => Poll::Ready(()),
			_ => Poll::Pending,
		}) {
			Poll::Ready(Ok(())) => Poll::Ready(()),
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
			fetch: None,
		}
	}
}

/// A segment without its media, reported by [`Consumer::peek`].
pub struct Info {
	/// The segment number, as [`Segment::segment`].
	pub segment: u64,
	/// The bytes its groups carry on the wire; 0 for a gap. Its fragment is a little larger,
	/// adding ~12-16 bytes per frame plus a small fixed header, so treat this as a lower bound.
	pub bytes: usize,
	/// Presentation duration.
	pub duration: Duration,
	/// Wall-clock start time, as [`Segment::program_date_time`].
	pub program_date_time: Option<Wall>,
	/// The absolute discontinuity sequence, as [`Segment::discontinuity`].
	pub discontinuity: u64,
}

/// A segment with its transmuxed media, yielded by a [`Consumer`].
pub struct Segment {
	/// The segment number (its URI is `seg/{tag}.{segment}.m4s`).
	pub segment: u64,
	/// The transmuxed CMAF fragment (`moof`+`mdat`), fetched on demand by [`Consumer::next`],
	/// or `None` for a gap: the rendition has no media for this span (`EXT-X-GAP`), but the
	/// segment keeps its number.
	pub media: Option<Bytes>,
	/// Presentation duration.
	pub duration: Duration,
	/// Wall-clock start time, when the catalog advertises its broadcast clock.
	pub program_date_time: Option<Wall>,
	/// The absolute discontinuity sequence within this broadcaster.
	///
	/// Use the first retained segment's value as `EXT-X-DISCONTINUITY-SEQUENCE`, then write
	/// `current - previous` `EXT-X-DISCONTINUITY` tags before each later segment; a skipped
	/// epoch makes that more than one. Cursors share this sequence regardless of when
	/// they start or which segments they skip. Recreating the broadcaster starts a new
	/// namespace: start a new recording or map it into a recording-wide sequence.
	pub discontinuity: u64,
}

/// A cursor over one rendition's segments, in order.
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
	/// The next row's media being fetched or sized, kept across polls.
	fetch: Option<Fetch>,
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
		let mut load = LoadInit::default();
		kio::wait(|waiter| self.rendition.poll_latest_init(waiter, &mut load)).await
	}

	/// The next segment's [`Info`], so a caller can reserve memory and time before
	/// [`next`](Self::next) builds its media.
	///
	/// Counts each frame of the segment's groups as it is read, without decoding or keeping any,
	/// so nothing is held while the caller waits, and `next` reads them again. `None` once the
	/// rendition ends cleanly. Fails as `next` would on a missing segment, and never advances the
	/// cursor.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_peek(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Info>>> {
		if self.fetch.as_ref().is_some_and(|fetch| !fetch.sizes()) {
			self.fetch = None;
		}
		let (row, bytes) = loop {
			let Some(fetch) = &mut self.fetch else {
				let Some(row) = ready!(self.poll_row(waiter))? else {
					return Poll::Ready(Ok(None));
				};
				if row.stop {
					break (row, 0);
				}
				self.fetch = Some(Fetch::sizing(row));
				continue;
			};
			let bytes = ready!(fetch.poll_size(&self.rendition, waiter));
			let row = self.fetch.take().expect("fetching above").row().clone();
			let bytes = bytes?.ok_or(crate::Error::SegmentUnavailable { segment: row.segment })?;
			break (row, bytes);
		};
		Poll::Ready(Ok(Some(Info {
			segment: row.segment,
			bytes,
			duration: row.duration,
			program_date_time: self.rendition.wall_clock(row.pts),
			discontinuity: row.discontinuity,
		})))
	}

	/// The next segment, with its media; `None` only once the rendition ends cleanly, so a
	/// recording that reads to `None` is complete.
	///
	/// Once the rendition's records fail, the cursor drains the segments it still covers, then
	/// fails every call with [`MediaFailed`](crate::Error::MediaFailed), as
	/// [`Rendition::playlist`] does; a failed catalog likewise with
	/// [`CatalogFailed`](crate::Error::CatalogFailed). A denied referenced broadcast fails every
	/// call with its authorization error.
	///
	/// FETCHes and transmuxes the next segment's groups. A stop, or a record its successor can't
	/// bound, is yielded without media, so every segment number is accounted for, except across a
	/// pause longer than the playlist window: only its newest
	/// `MAX_SEGMENTS` gaps are listed, since older ones would slide out of every window before
	/// the next segment, so the numbers before them are skipped. Missing window rows or unavailable media fail the recording instead of
	/// silently omitting segments. Fetch/transmux errors leave the cursor in place so the caller
	/// can retry. A bound instance's retained media drains after End or Restart; a replacement's
	/// media is never read under the old rows.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Segment>>> {
		if self.fetch.as_ref().is_some_and(Fetch::sizes) {
			self.fetch = None;
		}
		let (row, media) = loop {
			let Some(fetch) = &mut self.fetch else {
				let Some(row) = ready!(self.poll_row(waiter))? else {
					return Poll::Ready(Ok(None));
				};
				if row.stop {
					break (row, None);
				}
				self.fetch = Some(Fetch::new(row));
				continue;
			};
			let media = ready!(fetch.poll(&self.rendition, waiter));
			// A failure leaves the position for a retry to fetch the row afresh.
			let row = self.fetch.take().expect("fetching above").row().clone();
			let media = media?.ok_or(crate::Error::SegmentUnavailable { segment: row.segment })?;
			break (row, Some(media));
		};
		self.position.1 += 1;
		Poll::Ready(Ok(Some(Segment {
			segment: row.segment,
			media,
			duration: row.duration,
			program_date_time: self.rendition.wall_clock(row.pts),
			discontinuity: row.discontinuity,
		})))
	}

	fn poll_row(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Row>>> {
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
			tag: Arc::from(tag("video.hls", b"")),
			groups: super::super::grid::Groups::one(group),
			duration: Duration::from_millis(duration_ms),
			pts,
			end: Duration::try_from(pts).unwrap() + Duration::from_millis(duration_ms),
			discontinuity: 0,
			stop: false,
			complete: true,
			content: None,
		}
	}

	// Content that resumes at the pts it stopped shares a `$Time$` with the stop, which the MPD
	// omits, so the time names the resumed segment.
	#[test]
	fn a_time_names_the_resumed_segment_not_the_stop() {
		let live = Producer::new(0);
		let window = Duration::from_secs(30);
		live.push(row(0, 0, 0, 4_000), window);
		let mut stop = row(1, 1, 4_000, 0);
		stop.groups = Default::default();
		stop.stop = true;
		live.push(stop, window);
		live.push(row(2, 1, 4_000, 4_000), window);
		let tag = tag("video.hls", b"");
		assert_eq!(live.segment_number_at(&tag, 4_000), Some(2));
		assert_eq!(live.segment_number_at(&tag, 0), Some(0));
	}

	#[test]
	fn every_row_is_listed() {
		let live = Producer::new(0);
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

	// An ended window lists nothing more: the playlist already went out with `EXT-X-ENDLIST`.
	#[test]
	fn an_ended_window_refuses_rows() {
		let live = Producer::new(0);
		let window = Duration::from_secs(30);
		live.push(row(0, 0, 0, 2_000), window);
		live.end();
		live.push(row(1, 1, 2_000, 2_000), window);
		let snapshot = live.window();
		assert!(snapshot.ended);
		let segments: Vec<_> = snapshot.segments.iter().map(|row| row.segment).collect();
		assert_eq!(segments, [0]);
	}

	// A restart still drops an ended window's rows, so the new run's generation never lists the
	// previous run's segments. The window stays ended.
	#[test]
	fn an_ended_window_still_clears() {
		let live = Producer::new(0);
		let window = Duration::from_secs(30);
		live.push(row(0, 0, 0, 2_000), window);
		live.end();
		live.clear();
		let snapshot = live.window();
		assert!(snapshot.ended);
		assert!(snapshot.segments.is_empty());
	}

	// A cut freezes the listed rows: later records, trims, resets, and failures are refused, and
	// the window reads as ended. One that failed first keeps its failure.
	#[test]
	fn a_cut_window_keeps_what_it_listed() {
		let live = Producer::new(0);
		let window = Duration::from_secs(30);
		live.push(row(0, 0, 0, 2_000), window);
		live.push(row(1, 1, 2_000, 2_000), window);
		live.cut();
		live.push(row(2, 2, 4_000, 2_000), window);
		live.clear();
		live.fail(crate::Error::IncompleteRecording);
		let snapshot = live.window();
		assert!(snapshot.ended);
		assert!(snapshot.failed.is_none());
		let segments: Vec<_> = snapshot.segments.iter().map(|row| row.segment).collect();
		assert_eq!(segments, [0, 1]);

		let failed = Producer::new(0);
		failed.fail(crate::Error::IncompleteRecording);
		failed.cut();
		assert!(failed.failure().is_some());
	}

	#[test]
	fn window_evicts_and_advances_sequence() {
		let live = Producer::new(0);
		let window = Duration::from_secs(4);
		for i in 0..6u64 {
			live.push(row(i, i, i * 2_000, 2_000), window);
		}

		let snapshot = live.window();
		// Segments still cover >= 4s after eviction, and the sequence is the first listed
		// segment's number.
		assert!(snapshot.sequence > 0);
		let span: Duration = snapshot.segments.iter().map(|s| s.duration).sum();
		assert!(span >= Duration::from_secs(4));
		assert_eq!(snapshot.segments.first().unwrap().segment, snapshot.sequence);
	}

	#[test]
	fn a_reset_clears_rows_before_the_next_segment() {
		let live = Producer::new(0);
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

	#[test]
	fn rows_and_the_init_groups() {
		let live = Producer::new(0);
		let window = Duration::from_secs(30);
		assert!(live.init_groups().is_empty());
		live.push(row(0, 0, 0, 1_000), window);
		assert_eq!(live.init_groups(), [0], "a listed row's successor already started");
		live.push(
			Row {
				stop: true,
				..row(1, 1, 1_000, 0)
			},
			window,
		);

		let tag = tag("video.hls", b"");
		assert_eq!(live.row(&tag, 0).unwrap().groups, super::super::grid::Groups::one(0));
		assert_eq!(live.row(&tag, 7), None, "unknown segments miss");
		assert_eq!(live.row("00000000", 0), None, "another record track's numbering misses");
		assert_eq!(live.init_groups(), [0], "a stop holds no media");
		live.push(row(2, 1, 1_000, 1_000), window);
		// Only the stop after it bounds group 2, which the row says before the stop is listed.
		live.push(
			Row {
				complete: false,
				..row(3, 2, 2_000, 1_000)
			},
			window,
		);
		assert_eq!(
			live.init_groups(),
			[1, 2],
			"a group only a stop bounds may be open, so a complete one goes first"
		);
		live.push(
			Row {
				stop: true,
				..row(4, 3, 3_000, 0)
			},
			window,
		);
		assert_eq!(live.init_groups(), [1, 2]);
	}

	#[test]
	fn backwards_jump_resets_the_window() {
		let live = Producer::new(0);
		let window = Duration::from_secs(30);
		live.push(row(0, 0, 10_000, 2_000), window);
		live.push(row(1, 1, 12_000, 2_000), window);
		live.push(row(2, 2, 1_000, 2_000), window); // restart: pts rewound

		let snapshot = live.window();
		assert_eq!(snapshot.segments.len(), 1, "the window restarted at the new row");
		assert_eq!(snapshot.segments[0].segment, 2);

		// A segment number that rewinds (a restarted publisher) resets the same way.
		live.push(row(0, 0, 2_000, 2_000), window);
		assert_eq!(live.window().segments.len(), 1);
		assert_eq!(live.state.read().resets, 2);
	}

	#[test]
	fn cursor_walks_segments() {
		let live = Producer::new(0);
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
		let live = Producer::new(0);
		// Two rows cover the window, so a third evicts the first.
		let window = Duration::from_secs(4);
		for segment in 5..8 {
			live.push(row(segment, segment, segment * 2_000, 2_000), window);
		}
		// One cursor consumed the evicted row, another has not read anything yet.
		assert!(matches!(
			live.state.read().next_at((0, 0)),
			Err(crate::Error::RecordingLagged)
		));
		assert!(matches!(live.state.read().next_at((0, 1)), Ok(Next::Ready(_))));
		live.push(row(8, 8, 16_000, 2_000), window);
		assert!(matches!(
			live.state.read().next_at((0, 1)),
			Err(crate::Error::RecordingLagged)
		));
		assert!(matches!(live.state.read().next_at((0, 3)), Ok(Next::Ready(_))));
		assert!(matches!(live.state.read().next_at((0, 4)), Ok(Next::Pending)));
		live.clear();
		live.push(row(0, 0, 0, 2_000), window);
		assert!(matches!(
			live.state.read().next_at((0, 4)),
			Err(crate::Error::RecordingLagged)
		));
		assert!(matches!(live.state.read().next_at((1, 4)), Ok(Next::Ready(_))));
	}

	#[test]
	fn tags_tell_tracks_apart() {
		assert_ne!(tag("video.hls", b""), tag("video2.hls", b""));
		assert_ne!(tag("video.hls", b"a"), tag("video.hls", b"b"));
		// The separator keeps a name's tail from passing for a config's head.
		assert_ne!(tag("video.hls", b"x"), tag("video.hlsx", b""));
		assert_eq!(tag("video.hls", b"").len(), 8);
		// Edges on other releases derive the same URLs: the first 4 bytes of SHA-256("video.hls\0a").
		assert_eq!(tag("video.hls", b"a"), "93317179");
		assert!(tag("a.b/c", b"").chars().all(|c| c.is_ascii_hexdigit()));
	}
}
