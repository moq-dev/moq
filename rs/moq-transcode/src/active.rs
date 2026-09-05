//! Which renditions are encoding, and how much each has produced.
//!
//! Nothing is encoded until a consumer asks for a rung, so a transcoder that is
//! publishing a catalog and a transcoder that is saturating a GPU look identical
//! from the outside. Broadcast demand ([`moq_net::broadcast::Demand`]) closes
//! half the gap: it says *someone* is watching. This module closes the other
//! half by naming *which* renditions are being produced, and counting what each
//! one produced, which is what a caller pricing or admitting the work needs.
//!
//! [`Consumer`] is a cursor shaped like [`moq_net::announce::Consumer`]: it
//! reports the ladder as it resolves, then one rendition starting or stopping at
//! a time. Each [`Rendition`] it hands over is a lasting handle, so a caller
//! keeps them all and reads the counters whenever it bills.
//!
//! The cursor cannot bill on its own. A rendition whose pipelines start and stop
//! between two calls is never reported as an edge (the same is true of
//! `announce`), and a group fetch is exactly that: one pipeline per group, alive
//! for milliseconds. The counters behind the handle count it anyway, which is
//! why the ladder is delivered up front rather than on the first edge.
//!
//! Frames are the unit rather than wall-clock time, because the two only agree
//! on the live path. A group fetch encodes seconds of media in milliseconds, and
//! a subscriber attached to a stalled source holds a pipeline open for minutes
//! while producing nothing; counting frames is right in both. Media seconds, if
//! that is the bill, are [`Rendition::frames`] over [`Rendition::framerate`].
//!
//! A rendition counts as encoding from its first encoded frame, not from the
//! moment a consumer asked, for the same reason.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Poll, ready};

use crate::catalog::Resolved;

/// A rendition starting or stopping, delivered by [`Consumer`].
///
/// Also delivered once per rendition when the ladder resolves, with `encoding`
/// false, so a caller has every handle before any encoding can be missed.
pub struct Update {
	/// The rendition this is about.
	pub rendition: Rendition,

	/// Whether it is encoding right now.
	pub encoding: bool,
}

/// A handle to one output rendition, holding the counters a caller bills against.
///
/// Cheap to clone, and it outlives the encode: the totals stay readable while
/// the rendition is idle, and keep accumulating if it starts again. Obtained
/// from [`Update::rendition`].
#[derive(Clone)]
pub struct Rendition(Arc<Meter>);

impl Rendition {
	fn new(rung: &Resolved) -> Self {
		Self(Arc::new(Meter {
			rung: rung.clone(),
			counts: kio::Lock::new(Counts::default()),
		}))
	}

	/// The rendition/track name, e.g. `video/360p`.
	pub fn name(&self) -> &str {
		&self.0.rung.name
	}

	/// The output resolution, derived from the source aspect ratio.
	///
	/// Fixed for the life of the rendition: a source that resizes the ladder
	/// under it retires this rung and publishes the replacement under a new name.
	pub fn size(&self) -> moq_video::Size {
		self.0.rung.size
	}

	/// The target bitrate, in bits per second.
	pub fn bitrate(&self) -> u64 {
		self.0.rung.bitrate
	}

	/// The output framerate, inherited from the source.
	pub fn framerate(&self) -> u32 {
		self.0.rung.framerate
	}

	/// How many frames this rendition has encoded, over every pipeline.
	///
	/// This is the meter to bill: monotonic, never reset, and counting what was
	/// produced rather than how long a pipeline stayed alive, so a group fetch
	/// and a live session are charged the same way. Subtracting two reads bills
	/// the span between them, and `frames / framerate` is the media seconds that
	/// reached consumers. Frames that failed to reach the output group are not
	/// counted.
	pub fn frames(&self) -> u64 {
		self.0.counts.lock().frames
	}

	/// How many bytes of encoded bitstream this rendition has produced.
	///
	/// The payloads written to the output track, excluding container framing.
	pub fn bytes(&self) -> u64 {
		self.0.counts.lock().bytes
	}

	/// Bank encoded output. Called on the writing path, off the cursor's lock.
	fn produced(&self, frames: u64, bytes: u64) {
		let mut counts = self.0.counts.lock();
		counts.frames += frames;
		counts.bytes += bytes;
	}
}

impl std::fmt::Debug for Rendition {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Rendition")
			.field("name", &self.name())
			.field("frames", &self.frames())
			.finish()
	}
}

struct Meter {
	rung: Resolved,
	counts: kio::Lock<Counts>,
}

/// Everything a caller bills against, behind one lock the cursors never touch.
#[derive(Default)]
struct Counts {
	/// Frames written to the output track.
	frames: u64,
	/// Bytes of encoded bitstream written to the output track.
	bytes: u64,
}

/// One rendition's entry in the shared ladder.
struct Entry {
	rendition: Rendition,
	/// Pipelines producing it right now. Lives here rather than behind the
	/// handle so bumping it is the same mutation that wakes the cursors.
	refs: usize,
}

#[derive(Default)]
struct State {
	/// The resolved ladder, by track name. Fixed once `run` resolves it, and
	/// never pruned: a caller subtracting two [`Rendition::frames`] reads needs
	/// the counters to survive an idle gap.
	ladder: BTreeMap<String, Entry>,
}

/// The writing half, held by the transcoder and every rung serving off it.
#[derive(Clone, Default)]
pub(crate) struct Producer {
	state: kio::Producer<State>,
}

impl Producer {
	/// A fresh cursor, positioned before the ladder so it reports every
	/// rendition once and everything already encoding.
	pub(crate) fn consume(&self) -> Consumer {
		Consumer {
			state: self.state.consume(),
			seen: BTreeMap::new(),
		}
	}

	/// Publish the resolved ladder, so a cursor holds every handle before any
	/// rung can encode.
	///
	/// Called again whenever the source resizes the ladder. A rung re-resolved
	/// under a new picture is a new name and so a new handle; the retired one
	/// keeps its own, since the bill for what it already encoded outlives it.
	pub(crate) fn declare<'a>(&self, rungs: impl IntoIterator<Item = &'a Resolved>) {
		let Ok(mut state) = self.state.write() else { return };
		for rung in rungs {
			state.entry(rung);
		}
	}

	/// Attach a pipeline to a rendition until the returned guard drops.
	///
	/// Attaching is not encoding: the rendition starts on the guard's first
	/// [`Guard::produced`], so a pipeline that never encodes a frame is never
	/// reported and never billed.
	pub(crate) fn attach(&self, rung: &Resolved) -> Guard {
		// The guard holds a producer clone, so the channel stays open until the
		// last of them is gone.
		let rendition = match self.state.write() {
			Ok(mut state) => state.entry(rung).rendition.clone(),
			// Closed: an orphan meter, so the pipeline still counts its own work.
			Err(_) => Rendition::new(rung),
		};

		Guard {
			state: self.state.clone(),
			rendition,
			producing: AtomicBool::new(false),
		}
	}
}

impl State {
	fn entry(&mut self, rung: &Resolved) -> &mut Entry {
		self.ladder.entry(rung.name.clone()).or_insert_with(|| Entry {
			rendition: Rendition::new(rung),
			refs: 0,
		})
	}

	/// Adjust how many pipelines are producing `name`. Zero to one (and back) is
	/// the edge a cursor reports.
	fn count(&mut self, name: &str, delta: isize) {
		let Some(entry) = self.ladder.get_mut(name) else { return };
		entry.refs = entry.refs.saturating_add_signed(delta);
	}
}

/// Holds a rendition encoding until dropped.
///
/// RAII rather than an explicit release: every encode path is cancelled by being
/// dropped (a rung whose demand goes away, a fetch aborted with its `JoinSet`),
/// so a release call would be skipped exactly when it matters and leave the
/// rendition reported as encoding forever.
pub(crate) struct Guard {
	state: kio::Producer<State>,
	rendition: Rendition,
	/// Whether this pipeline has produced a frame, so it is counted in the
	/// rendition's refs and has to take itself back out on drop. Atomic rather
	/// than a `Cell` only so a `&Guard` can cross an `.await` in a spawned task.
	producing: AtomicBool,
}

impl Guard {
	/// Count frames written to the output track.
	///
	/// The first call is what makes the rendition encoding, waking the cursors.
	/// Later calls only touch this rendition's counters, so a per-frame call
	/// costs one uncontended lock and no wakeups.
	pub(crate) fn produced(&self, frames: u64, bytes: u64) {
		if frames == 0 {
			return;
		}
		self.rendition.produced(frames, bytes);

		if self.producing.swap(true, Ordering::Relaxed) {
			return;
		}
		if let Ok(mut state) = self.state.write() {
			state.count(self.rendition.name(), 1);
		}
	}
}

impl Drop for Guard {
	fn drop(&mut self) {
		if !self.producing.load(Ordering::Relaxed) {
			return;
		}
		if let Ok(mut state) = self.state.write() {
			state.count(self.rendition.name(), -1);
		}
	}
}

/// A cursor over the renditions this transcoder produces.
///
/// Shaped like [`moq_net::announce::Consumer`]: it yields one rendition at a
/// time rather than a snapshot of the whole ladder, and it starts before the
/// ladder, so it reports every rendition once (with [`Update::encoding`] false)
/// and then every start and stop. Obtained from
/// [`Transcoder::active`](crate::Transcoder::active).
///
/// It is a cursor, not a log: a rendition that starts and stops between two
/// calls is reported neither time. Bill from [`Rendition::frames`], which counts
/// it regardless.
///
/// ```no_run
/// # async fn example(active: &mut moq_transcode::active::Consumer) {
/// while let Some(update) = active.next().await {
///     match update.encoding {
///         true => println!("{} started", update.rendition.name()),
///         false => println!("{} idle after {} frames", update.rendition.name(), update.rendition.frames()),
///     }
/// }
/// # }
/// ```
pub struct Consumer {
	state: kio::Consumer<State>,
	/// What this cursor last reported for each rendition, which is its position.
	/// A name absent from it has never been reported at all.
	seen: BTreeMap<String, bool>,
}

impl Consumer {
	/// The next rendition to report, or `None` once the transcoder is gone.
	pub async fn next(&mut self) -> Option<Update> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next rendition to report, without blocking.
	///
	/// Returns `Poll::Ready(Some(_))` for an update, `Poll::Ready(None)` once the
	/// transcoder is gone, or `Poll::Pending` after registering `waiter`.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Option<Update>> {
		let update = {
			let seen = &self.seen;
			match ready!(self.state.poll(waiter, |state| match next_update(state, seen) {
				Some(update) => Poll::Ready(update),
				None => Poll::Pending,
			})) {
				Ok(update) => update,
				// Closed: discard the Ref so its lock guard doesn't escape this call.
				Err(_) => return Poll::Ready(None),
			}
		};
		Poll::Ready(Some(self.advance(update)))
	}

	/// The next rendition to report, or `None` if there is nothing new.
	///
	/// `None` does NOT mean the cursor is closed; see [`is_closed`](Self::is_closed).
	pub fn try_next(&mut self) -> Option<Update> {
		let update = {
			let seen = &self.seen;
			next_update(&self.state.read(), seen)?
		};
		Some(self.advance(update))
	}

	/// True once the transcoder is gone: nothing will start encoding again.
	pub fn is_closed(&self) -> bool {
		self.state.is_closed()
	}

	/// Move the cursor past an update before handing it to the caller.
	fn advance(&mut self, update: Update) -> Update {
		self.seen.insert(update.rendition.name().to_string(), update.encoding);
		update
	}
}

/// The first rendition whose state differs from `seen`, in name order. A name
/// missing from `seen` has never been reported, so the ladder lands first.
fn next_update(state: &State, seen: &BTreeMap<String, bool>) -> Option<Update> {
	state.ladder.iter().find_map(|(name, entry)| {
		let encoding = entry.refs > 0;
		if seen.get(name) == Some(&encoding) {
			return None;
		}
		Some(Update {
			rendition: entry.rendition.clone(),
			encoding,
		})
	})
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;

	fn resolved(name: &str, height: u32) -> Resolved {
		Resolved {
			name: name.to_string(),
			height,
			size: moq_video::Size::new(height * 16 / 9, height),
			bitrate: 100_000,
			framerate: 30,
		}
	}

	#[tokio::test]
	async fn reports_the_ladder_then_each_edge() {
		let active = Producer::default();
		let rung = resolved("video/360p", 360);
		let mut cursor = active.consume();
		assert!(cursor.try_next().is_none());

		// The ladder lands before anything encodes, so a caller holds the handle
		// even if the first pipeline is too short to be an edge.
		active.declare(std::slice::from_ref(&rung));
		let update = cursor.next().await.unwrap();
		assert_eq!(update.rendition.name(), "video/360p");
		assert_eq!(update.rendition.size().height, 360);
		assert!(!update.encoding);
		assert!(cursor.try_next().is_none());

		let guard = active.attach(&rung);
		guard.produced(1, 1_000);
		assert!(cursor.next().await.unwrap().encoding);
		assert!(cursor.try_next().is_none());

		drop(guard);
		assert!(!cursor.next().await.unwrap().encoding);
	}

	/// A pipeline is billable when it produces, not when it attaches: a viewer
	/// subscribing to a rung whose source never sends a frame costs nothing, and
	/// a transcoder that encodes nothing must not look like one saturating a GPU.
	#[tokio::test]
	async fn attaching_without_producing_is_not_encoding() {
		let active = Producer::default();
		let rung = resolved("video/360p", 360);
		active.declare(std::slice::from_ref(&rung));

		let mut cursor = active.consume();
		let rendition = cursor.next().await.unwrap().rendition;

		let guard = active.attach(&rung);
		tokio::time::sleep(Duration::from_millis(20)).await;
		assert!(cursor.try_next().is_none(), "attaching reported an edge");
		assert_eq!(rendition.frames(), 0);

		// The first frame is what makes it encoding.
		guard.produced(1, 1_000);
		assert!(cursor.next().await.unwrap().encoding);
		assert_eq!(rendition.frames(), 1);

		drop(guard);
		assert!(!cursor.next().await.unwrap().encoding);
	}

	/// A fetch overlapping the live session is one rendition, not two: a second
	/// pipeline is not an edge. Its output still counts, because it really did
	/// encode those frames.
	#[tokio::test]
	async fn concurrent_pipelines_are_one_rendition() {
		let active = Producer::default();
		let low = resolved("video/240p", 240);
		let high = resolved("video/360p", 360);
		let mut cursor = active.consume();

		let live = active.attach(&high);
		live.produced(2, 2_000);
		let rendition = cursor.next().await.unwrap().rendition;

		let fetch = active.attach(&high);
		fetch.produced(1, 500);
		let other = active.attach(&low);
		other.produced(1, 400);
		// Only the second NAME is an edge.
		let update = cursor.next().await.unwrap();
		assert_eq!(update.rendition.name(), "video/240p");
		assert!(update.encoding);
		assert!(cursor.try_next().is_none());

		drop(fetch);
		// Still live, so the release is not an edge either.
		assert!(cursor.try_next().is_none());

		drop(live);
		let update = cursor.next().await.unwrap();
		assert_eq!(update.rendition.name(), "video/360p");
		assert!(!update.encoding);
		drop(other);
		assert!(!cursor.next().await.unwrap().encoding);

		// The handle outlives the encode, and both pipelines counted their output.
		assert_eq!(rendition.frames(), 3);
		assert_eq!(rendition.bytes(), 2_500);
	}

	/// A fresh cursor must report what is already encoding, or a caller that only
	/// ever awaits `next` never learns about a rendition that started first.
	#[tokio::test]
	async fn a_fresh_cursor_reports_the_current_set() {
		let active = Producer::default();
		let rung = resolved("video/480p", 480);
		let guard = active.attach(&rung);
		guard.produced(1, 1_000);

		let mut cursor = active.consume();
		let update = cursor.next().await.unwrap();
		assert_eq!(update.rendition.name(), "video/480p");
		assert!(update.encoding);
		// Caught up: it waits for a real change rather than spinning.
		assert!(cursor.try_next().is_none());
		assert!(
			tokio::time::timeout(Duration::from_millis(50), cursor.next())
				.await
				.is_err()
		);
	}

	/// The whole point of splitting the counters from the cursor: a pipeline that
	/// starts and stops between two reads is invisible as an edge, but it still
	/// encoded, so it still bills. This is the group-fetch path, which lives for
	/// milliseconds. The caller can only bill it because the ladder handed it the
	/// handle up front.
	#[tokio::test]
	async fn a_transient_pipeline_is_metered_without_an_edge() {
		let active = Producer::default();
		let rung = resolved("video/360p", 360);
		active.declare(std::slice::from_ref(&rung));

		let mut cursor = active.consume();
		let rendition = cursor.next().await.unwrap().rendition;
		assert_eq!(rendition.frames(), 0);

		let guard = active.attach(&rung);
		guard.produced(30, 30_000);
		drop(guard);

		// The cursor converged without ever reporting the start or the stop.
		assert!(cursor.try_next().is_none());
		// The counters did not miss it.
		assert_eq!(rendition.frames(), 30);
		assert_eq!(rendition.bytes(), 30_000);
	}

	/// A caller bills by subtracting two reads, so the counters have to keep
	/// their totals across an idle gap rather than restarting with the next
	/// pipeline.
	#[tokio::test]
	async fn the_counters_survive_an_idle_gap() {
		let active = Producer::default();
		let rung = resolved("video/360p", 360);
		let mut cursor = active.consume();

		let guard = active.attach(&rung);
		guard.produced(10, 10_000);
		let rendition = cursor.next().await.unwrap().rendition;

		drop(guard);
		cursor.next().await;
		assert_eq!(rendition.frames(), 10, "the totals reset when the rendition went idle");

		// A second session keeps accumulating rather than restarting at zero.
		let guard = active.attach(&rung);
		guard.produced(5, 5_000);
		assert!(cursor.next().await.unwrap().encoding);
		assert_eq!(rendition.frames(), 15);
		assert_eq!(rendition.bytes(), 15_000);
		drop(guard);
	}

	/// A cursor has to be able to tell "nothing is encoding" from "the transcoder
	/// is gone", or a metering loop parks forever on a dead transcode.
	#[tokio::test]
	async fn the_cursor_closes_with_the_producer() {
		let active = Producer::default();
		let mut cursor = active.consume();
		assert!(!cursor.is_closed());

		drop(active);
		assert!(cursor.is_closed());
		assert!(cursor.next().await.is_none());
	}
}
