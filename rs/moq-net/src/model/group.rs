//! A group is a stream of frames, split into a [Producer] and [Consumer] handle.
//!
//! A [Producer] writes an ordered stream of frames.
//! Frames can be written all at once ([Producer::write_frame]), or in chunks
//! ([Producer::create_frame]).
//!
//! A [Consumer] reads an ordered stream of frames.
//! The reader can be cloned, in which case each reader receives a copy of each frame. (fanout)
//!
//! Frames are numbered from 0 in write order. A group can be short at its front or its
//! back but never in the middle: [Producer::start_at] starts it later, so a handle can
//! carry the tail of a group whose leading frames came from somewhere else, and
//! [Producer::finish] ends it wherever writing stopped. [Consumer::set_frames] bounds a reader to a sub-range the same way [`track::Subscriber`]
//! bounds group sequences.
//!
//! The stream is closed with [Error] when all writers or readers are dropped.
use super::flate;
use crate::cache;
use crate::frame::{self, Frame, FrameBuf};
use crate::{Cap, Timescale, stats, track};
use bytes::Bytes;
use std::collections::VecDeque;
use std::mem::MaybeUninit;
use std::ops::{Bound, RangeBounds};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::task::{Poll, ready};

use crate::{Error, IntoBytes, Result, StreamError, Timestamp};

/// Maximum total size of frames in a group.
///
/// A write that would exceed this aborts the group with [`Error::GroupTooLarge`].
/// Doubles as the per-frame size cap: a larger declared size is [`Error::FrameTooLarge`]
/// before allocating, so one maximum-size frame can fill a group.
pub const MAX_CACHE_BYTES: u64 = 32 * 1024 * 1024; // 32 MB

/// Maximum number of frames in a group.
///
/// 8192 is the largest legal group; the 8193rd write returns [`Error::GroupTooLarge`]
/// and aborts the group.
pub const MAX_GROUP_FRAMES: usize = 8192;

/// Slots `VecDeque` rounds a group's first frame up to.
///
/// A `RawVec` detail rather than a knob, so it is asserted rather than trusted: std
/// handing out more would silently undercharge every cached group.
const FRAME_SLOTS: usize = 4;

/// Heap one cached group costs beyond its frame payloads, excluding the track-side
/// bookkeeping in [`track::CACHE_OVERHEAD`].
///
/// A group is one kio channel (allocated whether or not anything ever parks on it), the
/// `Arc<Alive>` its writer and observers share, and the frame slots the first write rounds up
/// to. Half of [`cache::ENTRY_OVERHEAD`]; see it for why this is derived rather than
/// measured.
pub(super) const CACHE_OVERHEAD: u64 = (kio::Producer::<GroupState>::HEAP
	// `Alive` behind an `Arc`'s two reference counts, which it is pointer-aligned to sit
	// straight after.
	+ 2 * size_of::<usize>()
	+ size_of::<Alive>()
	+ FRAME_SLOTS * size_of::<Frame>()) as u64;

/// A group contains a sequence number because they can arrive out of order.
///
/// You can use [track::Producer::append_group] if you just want to +1 the sequence number.
#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Ord, PartialOrd)]
pub struct Info {
	/// Per-track sequence number used to detect ordering and gaps. Higher numbers
	/// supersede lower ones; consumers may skip late arrivals.
	pub sequence: u64,
}

impl Info {
	/// Create a producer for this group on a millisecond track.
	///
	/// Test-only: real groups are created via [`track::Producer`], which
	/// supplies the parent track's [`track::Info`]. This helper exists for in-crate
	/// tests that don't exercise timestamps.
	#[cfg(test)]
	// private: test
	pub(crate) fn produce(self) -> Producer {
		Producer::new(
			self,
			track::Info::default().with_timescale(Timescale::MILLI),
			Default::default(),
		)
	}

	/// Create a producer for this group on an untimed track. Test-only, like [`Self::produce`].
	#[cfg(test)]
	pub(super) fn produce_untimed(self) -> Producer {
		Producer::new(self, track::Info::default(), Default::default())
	}
}

impl From<usize> for Info {
	fn from(sequence: usize) -> Self {
		Self {
			sequence: sequence as u64,
		}
	}
}

impl From<u64> for Info {
	fn from(sequence: u64) -> Self {
		Self { sequence }
	}
}

impl From<u32> for Info {
	fn from(sequence: u32) -> Self {
		Self {
			sequence: sequence as u64,
		}
	}
}

impl From<u16> for Info {
	fn from(sequence: u16) -> Self {
		Self {
			sequence: sequence as u64,
		}
	}
}

/// The in-flight (tail) frame being written. At most one exists at a time, since a
/// group is a single ordered stream.
pub(super) struct Partial {
	timestamp: Option<Timestamp>,
	fields: Option<Arc<super::object::Fields>>,
	buf: FrameBuf,
	// How much of `buf` has been charged to the cache so far.
	charged: u64,
	// A received DEFLATE slice's inflated size, on a compressed group.
	inflated: Option<u64>,
	// Plain bytes a local producer streams into a compressed group: compressed when the
	// frame commits, so no reader sees them, and uncharged until then.
	scratch: bool,
}

/// Shared group state. Model-visible so [`frame`] handles can observe the abort flag
/// while streaming a partial frame.
#[derive(Default)]
pub(super) struct GroupState {
	// Completed frames, each a contiguous payload. `offset` is the first frame this
	// handle holds, raised by [`Producer::start_at`]. A compressed group stores DEFLATE
	// slices here.
	pub(super) frames: VecDeque<Frame>,
	// IETF relay state, boxed so a group that never carries any pays one pointer.
	relay: Option<Box<Relay>>,
	// Compressed-track state, boxed so a plain group pays one pointer.
	flate: Option<Box<Flate>>,

	// The single in-flight frame, if one is open.
	pub(super) partial: Option<Partial>,

	// Index of the first frame this handle holds: any the group deliberately started
	// past (see [`Producer::start_at`]). Reading below it is [`Error::Lagged`]; the
	// frames are not here.
	pub(super) offset: usize,

	// The index the next frame written will get. Tracked separately from `frames` so it
	// survives the cache being released: a route taking the track over needs to know where
	// production stopped, and an abort is exactly when it asks.
	next_index: usize,

	// Exclusive cap on `next_index`, set when the track ends inside this group: a write
	// past the track's end is refused rather than served as content that does not exist.
	limit: Option<usize>,

	// One past the last frame that was fully written. Trails `next_index` while a chunked
	// frame is in flight, which is the frame a replacement route has to redeliver: only
	// its opener saw the payload, and only partly.
	committed: usize,

	// The total size (in bytes) of all cached frames plus what the in-flight frame has
	// written so far.
	pub(super) cache: u64,

	// Bills payload and frame-slot growth to the track's shared cache pool, so its bytes count
	// against the byte budget tracks evict toward.
	charge: cache::Charge,

	// Where the group sits in presentation time, settled by its first frame. Kept here
	// rather than read off `frames` so an abort doesn't erase it.
	timeline: Timeline,

	// How the track judged the start against its neighbors' when frame 0 landed.
	origin: Origin,

	// Once finalized, the total number of frames the group will ever contain. Recorded
	// at finish so the count outlives an abort that clears the cache.
	pub(super) fin: Option<usize>,

	// The error that caused the group to be aborted, if any. Mirrored into
	// `Alive::aborted`, so [`Producer::abort`] stays the only writer: anything else
	// setting this would leave track scans reading a group as live.
	pub(super) abort: Option<Error>,

	// The readers that could resume this group on another route. A release that is no
	// cache policy moves the frames to them rather than dropping them.
	handoff: Mutex<Weak<Handoff>>,

	// Whether an adopted head (see [`Handle::adopt_head`]) begins at the group's first
	// object, for a wire header that arrives after it.
	adopted: Option<bool>,
}

/// The frames a copy of a group holds, from where it starts, which a successor copy of
/// the same group on another route adopts below where it starts.
pub(super) struct Head {
	start: usize,
	// Whether `start` is the group's first object.
	first_object: bool,
	frames: Vec<Frame>,
	// Relay metadata for `frames`, at the same index; frames past its end carry none.
	objects: Vec<Option<Arc<super::object::Fields>>>,
	// A compressed group's inflated sizes for `frames`, at the same index; `None` for a
	// plain group.
	sizes: Option<Vec<u64>>,
}

/// A copy's [`Head`], shared by every reader that could resume its group elsewhere.
///
/// Read from the copy while it holds its frames. Once it fails, its release moves them
/// here instead, so a plain consumer of the failed copy still reads the abort and pins
/// nothing, and the frames go with the last reader holding this.
pub(super) struct Handoff {
	state: kio::ConsumerWeak<GroupState>,
	salvaged: Mutex<Option<Salvage>>,
}

/// A failed copy's head, billed to its pool while held; see [`cache::Track::hold`].
struct Salvage {
	head: Arc<Head>,
	bill: cache::Bill,
}

impl Handoff {
	/// The copy's head, if it holds any frames, or held them when it failed.
	pub(super) fn head(&self) -> Option<Arc<Head>> {
		// The group's lock first, in `release`'s order: under it the frames are either
		// still in the copy or already moved here, never in neither.
		let state = self.state.read();
		let salvaged = (self.salvaged.lock().unwrap_or_else(PoisonError::into_inner))
			.as_ref()
			.map(|salvage| salvage.head.clone());
		salvaged.or_else(|| state.head().map(Arc::new))
	}

	/// Drop the held head when `reclaim`, or once the pool's idle window passed, returning
	/// whether none is held any more.
	pub(super) fn expire(&self, reclaim: bool) -> bool {
		let mut salvaged = self.salvaged.lock().unwrap_or_else(PoisonError::into_inner);
		if salvaged
			.as_ref()
			.is_some_and(|salvage| reclaim || salvage.bill.expired())
		{
			*salvaged = None;
		}
		salvaged.is_none()
	}
}

impl Head {
	/// Roughly what holding it costs: payloads, fields, and their slots.
	fn bytes(&self) -> u64 {
		let payload: u64 = self.frames.iter().map(|frame| frame.payload.len() as u64).sum();
		let fields: u64 = self.objects.iter().flatten().map(|fields| fields.heap()).sum();
		let slots = self.frames.len() * size_of::<Frame>()
			+ self.objects.len() * size_of::<Option<Arc<super::object::Fields>>>()
			+ self.sizes.as_ref().map_or(0, |sizes| sizes.len() * size_of::<u64>());
		payload + fields + slots as u64
	}
}

/// What an IETF relay keeps beside a group's frames.
#[derive(Default)]
struct Relay {
	// Known before frames arrive.
	wire: Option<Wire>,
	// The terminal object, learned at the stream tail.
	end_object: Option<Arc<super::object::Fields>>,
	// Fields for `frames`, at the same local index. Frames past its end carry none.
	objects: VecDeque<Option<Arc<super::object::Fields>>>,
	// Heap charged for this box, its fields, and the terminal object.
	bytes: u64,
}

/// What a compressed group keeps beside its stored DEFLATE slices.
struct Flate {
	// Each completed frame's inflated size, at the same local index as `frames`.
	sizes: VecDeque<u64>,
	// The inflated size of every frame admitted so far, held to the budget a plain
	// group's bytes are, so a declaration is refused before anything inflates.
	inflated: u64,
	// Compresses a local producer's frames, created by its first write. A received group
	// stores what arrived and never recompresses.
	encoder: Option<flate::Encoder>,
	// The inflated copy every plain reader shares, built on first demand.
	plain: Arc<Plain>,
}

impl Flate {
	fn new(bill: cache::Extra) -> Self {
		Self {
			sizes: VecDeque::new(),
			inflated: 0,
			encoder: None,
			plain: Arc::new(Plain {
				len: AtomicUsize::new(0),
				bill,
				state: Mutex::new(PlainState::default()),
			}),
		}
	}

	/// Record a completed local frame's inflated size.
	fn push(&mut self, size: u64) {
		self.sizes.push_back(size);
		self.inflated += size;
	}

	/// Compress a locally written frame, returning its stored slice.
	fn deflate(&mut self, payload: &[u8]) -> Bytes {
		let plain = &self.plain;
		let encoder = self.encoder.get_or_insert_with(|| {
			plain.bill.add(flate::ENCODER_HEAP);
			flate::Encoder::new()
		});
		encoder.frame(payload)
	}

	/// Drop the codec state once no frame follows at `fin`: the encoder, and the decoder if
	/// its readers already inflated every frame.
	fn finish(&mut self, fin: usize) {
		if self.encoder.take().is_some() {
			self.plain.bill.sub(flate::ENCODER_HEAP);
		}
		let mut plain = self.plain.lock();
		plain.fin = Some(fin);
		if plain.frames.len() == fin {
			self.plain.drop_decoder(&mut plain);
		}
	}

	/// Drop everything but how the group is read, failing a late inflater with `err`.
	fn release(&mut self, err: Error) {
		self.sizes = VecDeque::new();
		self.encoder = None;
		self.plain.release(err);
	}
}

/// A compressed group's inflated copy, shared by its plain readers.
///
/// A reader takes the slices it needs under the group's guard and inflates them under
/// only this lock ([`Inflate`]), so a cold read never holds up the group's writer.
struct Plain {
	// Frames inflated so far, so a reader under the group's guard takes only the slices
	// past it without waiting on an inflater.
	len: AtomicUsize,
	// The inflated bytes and the group's codec state, counted toward its eviction size
	// once they exist. Changed only under `state`, or the group's lock for the encoder.
	bill: cache::Extra,
	state: Mutex<PlainState>,
}

#[derive(Default)]
struct PlainState {
	// Inflates the stored slices in order. Dropped once a finished group is fully inflated.
	decoder: Option<flate::Decoder>,
	// Inflated frames, from the group's first.
	frames: VecDeque<Bytes>,
	// The group's frame count once finished, so the decoder drops after the last frame.
	// Kept here rather than taken with the slices, which may predate the finish.
	fin: Option<usize>,
	// The first failure, sticky: every later frame builds on the one that failed.
	err: Option<Error>,
	// Why the group released its frames, failing a late inflater rather than billing past
	// the release.
	released: Option<Error>,
}

impl Plain {
	fn lock(&self) -> MutexGuard<'_, PlainState> {
		self.state.lock().unwrap_or_else(PoisonError::into_inner)
	}

	/// Drop `state`'s decoder, which no frame needs any more.
	fn drop_decoder(&self, state: &mut PlainState) {
		if state.decoder.take().is_some() {
			self.bill.sub(flate::DECODER_HEAP);
		}
	}

	/// Inflated local frame `local`, or why it is not.
	fn get(&self, local: usize) -> Result<Bytes> {
		let state = self.lock();
		match state.frames.get(local) {
			Some(frame) => Ok(frame.clone()),
			None => Err(state
				.released
				.clone()
				.or_else(|| state.err.clone())
				.unwrap_or(Error::Dropped)),
		}
	}

	/// Whether inflating failed on the group's content, rather than its release.
	fn malformed(&self) -> bool {
		let state = self.lock();
		state.released.is_none() && state.err.is_some()
	}

	fn release(&self, err: Error) {
		let mut state = self.lock();
		state.decoder = None;
		state.frames = VecDeque::new();
		state.released = Some(err);
		self.len.store(0, Ordering::Release);
		self.bill.clear();
	}
}

/// The stored slices a plain read of a compressed group inflates once the group's guard
/// is released.
struct Inflate {
	plain: Arc<Plain>,
	// Local index of the first slice: the copy's inflated length when the read took them.
	from: usize,
	// Each slice with its declared inflated size.
	slices: Vec<(Bytes, u64)>,
}

impl Inflate {
	/// Inflate the slices, skipping any a concurrent reader already did, then read local
	/// frames back with [`Self::get`]. `meter` counts the bytes this read inflated.
	///
	/// Locks per frame, so a release waits on at most one frame's inflation. Call with no
	/// lock held, since settling the debt takes the track's.
	fn run(&self, meter: &stats::Meter) {
		for (index, (slice, size)) in (self.from..).zip(&self.slices) {
			let mut state = self.plain.lock();
			if state.released.is_some() || state.err.is_some() {
				break;
			}
			if state.frames.len() > index {
				continue;
			}
			let frame = usize::try_from(*size)
				.map_err(|_| Error::FrameTooLarge)
				.and_then(|size| {
					let bill = &self.plain.bill;
					let decoder = state.decoder.get_or_insert_with(|| {
						bill.add(flate::DECODER_HEAP);
						flate::Decoder::new()
					});
					decoder.frame(slice, size)
				});
			match frame {
				Ok(frame) => {
					self.plain.bill.add(frame.len() as u64);
					meter.inflated(frame.len() as u64);
					state.frames.push_back(frame);
					self.plain.len.store(state.frames.len(), Ordering::Release);
					if state.fin == Some(state.frames.len()) {
						self.plain.drop_decoder(&mut state);
					}
				}
				Err(err) => {
					state.err = Some(err);
					self.plain.drop_decoder(&mut state);
					break;
				}
			}
		}
		// The inflated bytes are written like a producer's, so they pay the pool's debt the
		// same way: a finished track has no writer left to settle it.
		self.plain.bill.settle();
	}

	/// Inflated local frame `local`, or why it is not.
	fn get(&self, local: usize) -> Result<Bytes> {
		self.plain.get(local)
	}
}

/// A frame resolved under the group's guard, by [`GroupState::poll_frame_source`].
enum Source {
	/// Ready to hand out.
	Ready(frame::Header, frame::Source),
	/// A plain read of a compressed group: local frame `local` once `job` inflates it.
	Inflate {
		job: Inflate,
		local: usize,
		timestamp: Option<Timestamp>,
		fields: Option<Arc<super::object::Fields>>,
	},
}

/// The IETF header state that cannot be inferred from a plain frame list.
#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
#[derive(Clone, Debug)]
// private: passthrough
pub(crate) struct Wire {
	/// Object IDs differ from the public frame ordinals, so ordinal recovery cannot stitch.
	pub sparse: bool,
	pub first_object: bool,
	pub first_id: u64,
	pub extensions: bool,
	pub has_end: bool,
	pub fetchable: bool,
	pub last: Option<u64>,
}

/// Where a group sits in presentation time.
///
/// Its first frame settles which: an empty group has not presented anything yet, which
/// is a different answer from a group on an untimed track, whose frames carry no time.
#[derive(Clone, Copy, Debug, Default)]
enum Timeline {
	/// No frame opened yet.
	#[default]
	Empty,
	/// The track is untimed, so the group has no place in media time.
	Untimed,
	/// `start` is the first frame's timestamp, never revised. `latest` is the newest
	/// frame's: the group's presentation end so far, which a reader that has taken
	/// every frame sits at and a drift budget measures it against.
	Timed { start: Timestamp, latest: Timestamp },
}

/// How a timed group's start was checked against its neighbors' (see
/// [`track::Neighbors`]), which decides whether it bounds theirs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Origin {
	/// Not checked: untimed, started past frame 0, written with no open track, or received
	/// from a session. A start read off a later frame is not where the group began, and a
	/// received one may not be media time, so it bounds nothing.
	#[default]
	Unchecked,
	/// Frame 0 fit between its neighbors, so its start bounds theirs.
	Bound,
	/// Frame 0 landed at or below the track's eviction watermark, where its lower bound
	/// may be gone. Accepted so backfill keeps working, but it neither bounds a neighbor
	/// nor drives media-time expiry.
	Unverified,
}

impl GroupState {
	/// Refuse a write that would advance the group to `next_index`, past the track's end.
	fn admit(&self, next_index: usize) -> Result<()> {
		match self.limit {
			Some(limit) if next_index > limit => Err(Error::Closed),
			_ => Ok(()),
		}
	}

	/// Whether a compressed group's content failed to inflate.
	fn malformed(&self) -> bool {
		self.flate.as_ref().is_some_and(|flate| flate.plain.malformed())
	}

	/// Whether the first frame opened, or the group ended without one.
	fn started(&self) -> bool {
		!matches!(self.timeline, Timeline::Empty) || self.fin.is_some() || self.abort.is_some()
	}

	/// The bytes the in-flight frame declared but has not written yet, which its open
	/// already admitted.
	fn unwritten(&self) -> u64 {
		self.partial
			.as_ref()
			.map_or(0, |partial| partial.buf.size() as u64 - partial.charged)
	}

	/// Content still available to a reader of this group.
	///
	/// Counts the in-flight frame at its declared size, like [`Self::content_range`]: a
	/// reader that skips it misses the whole frame, however much has arrived.
	fn content(&self) -> stats::Content {
		stats::Content {
			bytes: self.cache + self.unwritten(),
			frames: self.next_index.saturating_sub(self.offset) as u64,
			groups: 1,
			datagrams: 0,
		}
	}

	/// Content in the half-open frame range that is still cached here.
	pub(super) fn content_range(&self, start: usize, end: usize) -> stats::Content {
		let start = start.max(self.offset);
		let end = end.min(self.next_index);
		if start >= end {
			return stats::Content::default();
		}

		let local_start = start.saturating_sub(self.offset).min(self.frames.len());
		let local_end = end.saturating_sub(self.offset).min(self.frames.len());
		let mut bytes = self
			.frames
			.range(local_start..local_end)
			.map(|frame| frame.payload.len() as u64)
			.sum();
		if start <= self.committed
			&& self.committed < end
			&& let Some(partial) = &self.partial
		{
			bytes += partial.buf.size() as u64;
		}

		stats::Content {
			bytes,
			frames: (end - start) as u64,
			groups: 0,
			datagrams: 0,
		}
	}

	/// Refuse admitting `extra` more inflated bytes past the group budget, on a compressed
	/// group. The declaration is checked like a plain frame's size, before anything inflates.
	fn inflated_overflow(&self, extra: u64) -> bool {
		self.flate
			.as_ref()
			.is_some_and(|flate| flate.inflated.saturating_add(extra) > MAX_CACHE_BYTES)
	}

	/// The slices a plain read of a compressed group still needs inflated to reach local
	/// frame `last`, to inflate once the guard is released.
	///
	/// [`Error::Lagged`] when the group started past frame 0: its first slice builds on
	/// frames it never held.
	fn inflate(&self, flate: &Flate, last: usize) -> Result<Inflate> {
		if self.offset != 0 {
			return Err(Error::Lagged);
		}
		let from = flate.plain.len.load(Ordering::Acquire).min(last + 1);
		Ok(Inflate {
			plain: flate.plain.clone(),
			from,
			slices: (from..=last)
				.map(|local| (self.frames[local].payload.clone(), flate.sizes[local]))
				.collect(),
		})
	}

	/// Resolve the source for the frame at `index`: a completed frame (whole) or the
	/// in-flight tail (streamed). Used by [`Consumer::poll_next_frame`].
	///
	/// A compressed group hands a plain reader each frame whole once it completes, as the
	/// slices to inflate, and a `raw` one the stored slice, streaming one still arriving.
	fn poll_frame_source(&self, index: usize, raw: bool) -> Poll<Result<Option<Source>>> {
		if index < self.offset {
			return Poll::Ready(Err(Error::Lagged));
		}
		let local = index - self.offset;
		if let Some(f) = self.frames.get(local) {
			// A frame read is a cache access: stamp it so expiry and the eviction
			// walk spare a group a consumer is actively draining.
			self.charge.refresh();
			let inflated = match &self.flate {
				Some(flate) if !raw => {
					return Poll::Ready(Ok(Some(Source::Inflate {
						job: self.inflate(flate, local)?,
						local,
						timestamp: f.timestamp,
						fields: self.fields(local),
					})));
				}
				Some(flate) => Some(flate.sizes[local]),
				None => None,
			};
			let info = frame::Info {
				size: f.payload.len() as u64,
				timestamp: f.timestamp,
			};
			return Poll::Ready(Ok(Some(Source::Ready(
				frame::Header {
					info,
					fields: self.fields(local),
					inflated,
				},
				frame::Source::Complete(f.payload.clone()),
			))));
		}
		if local == self.frames.len()
			&& let Some(p) = &self.partial
			&& !p.scratch
			&& (raw || self.flate.is_none())
		{
			self.charge.refresh();
			let info = frame::Info {
				size: p.buf.size() as u64,
				timestamp: p.timestamp,
			};
			return Poll::Ready(Ok(Some(Source::Ready(
				frame::Header {
					info,
					fields: p.fields.clone(),
					inflated: p.inflated,
				},
				frame::Source::Partial(p.buf.clone()),
			))));
		}
		ready!(self.poll_terminal(index))?;
		Poll::Ready(Ok(None))
	}

	/// Resolve the group's terminal state for a reader positioned at `index`.
	///
	/// A finished group is still aborted once its frames are released to free memory
	/// (aged out of the track's max age window, or evicted by the cache pool). A reader
	/// that already consumed every frame is missing nothing, so it gets the clean end of
	/// group; one that fell short sees the abort rather than a silently truncated stream.
	fn poll_terminal(&self, index: usize) -> Poll<Result<()>> {
		match (self.fin, &self.abort) {
			(Some(total), Some(err)) if index < total => Poll::Ready(Err(err.clone())),
			(Some(_), _) => Poll::Ready(Ok(())),
			(None, Some(err)) => Poll::Ready(Err(err.clone())),
			(None, None) => Poll::Pending,
		}
	}

	/// Resolve whether a reader at `index` can still make progress, answering the same
	/// question as a read without consuming anything.
	fn poll_end(&self, index: usize) -> Poll<Result<()>> {
		if index < self.offset {
			return Poll::Ready(Err(Error::Lagged));
		}
		self.poll_terminal(index)
	}

	/// Check frame 0's `timestamp` against the track's `neighbors`, returning how to record
	/// it. Anything but a timed frame 0 checked under an open local track is unchecked.
	fn check_start(&self, neighbors: Option<&track::Neighbors>, timestamp: Option<Timestamp>) -> Result<Origin> {
		match (neighbors, timestamp) {
			(Some(neighbors), Some(start)) if self.next_index == 0 => neighbors.admit(start),
			_ => Ok(self.origin),
		}
	}

	/// Settle how the group holds its start, returning it if frame 0 just became a bound.
	fn settle(&mut self, origin: Origin) -> Option<Timestamp> {
		let bound = origin == Origin::Bound && self.origin != Origin::Bound;
		self.origin = origin;
		match self.timeline {
			Timeline::Timed { start, .. } if bound => Some(start),
			_ => None,
		}
	}

	/// Record a frame's place in presentation time: the first frame starts the group,
	/// and each later one extends a timed group's end. Every frame matches its track
	/// ([`on_track`]), so a group never mixes the two.
	fn stamp(&mut self, timestamp: Option<Timestamp>) {
		self.timeline = match (self.timeline, timestamp) {
			(Timeline::Timed { start, .. }, Some(latest)) => Timeline::Timed { start, latest },
			(_, Some(start)) => Timeline::Timed { start, latest: start },
			(_, None) => Timeline::Untimed,
		};
	}

	/// Whether adding `extra_frames` totaling `extra_bytes` would exceed the group budget.
	fn would_overflow(&self, extra_frames: usize, extra_bytes: u64) -> bool {
		self.next_index.saturating_sub(self.offset).saturating_add(extra_frames) > MAX_GROUP_FRAMES
			|| self
				.cache
				.saturating_add(self.relay.as_ref().map_or(0, |relay| relay.bytes))
				.saturating_add(extra_bytes)
				> MAX_CACHE_BYTES
	}

	/// The relay state, charging its box the first time a group needs it.
	fn relay(&mut self) -> &mut Relay {
		let charge = &mut self.charge;
		self.relay.get_or_insert_with(|| {
			let bytes = size_of::<Relay>() as u64;
			charge.add(bytes);
			Box::new(Relay {
				bytes,
				..Default::default()
			})
		})
	}

	pub(super) fn wire(&self) -> Option<&Wire> {
		self.relay.as_ref()?.wire.as_ref()
	}

	fn wire_mut(&mut self) -> Option<&mut Wire> {
		self.relay.as_mut()?.wire.as_mut()
	}

	/// Whether Object IDs left the frame ordinals, so ordinal recovery cannot stitch.
	pub(super) fn sparse(&self) -> bool {
		self.wire().is_some_and(|wire| wire.sparse)
	}

	/// Reserve the next completed frame, returning growth not covered by the fixed overhead.
	fn reserve_frame(&mut self) -> u64 {
		let before = self.frames.capacity().saturating_sub(FRAME_SLOTS);
		self.frames.reserve(1);
		let after = self.frames.capacity().saturating_sub(FRAME_SLOTS);
		((after - before) * size_of::<Frame>()) as u64
	}

	/// Reserve a metadata slot for the next completed frame, returning the growth.
	fn reserve_fields(&mut self) -> u64 {
		let len = self.frames.len();
		let objects = &mut self.relay().objects;
		let before = objects.capacity();
		objects.resize(len, None);
		objects.reserve(1);
		((objects.capacity() - before) * size_of::<Option<Arc<super::object::Fields>>>()) as u64
	}

	/// How many completed frames, from the first, the fields table covers.
	fn objects(&self) -> usize {
		self.relay.as_ref().map_or(0, |relay| relay.objects.len())
	}

	/// The relay metadata of the completed frame at `local`, if it carried any.
	fn fields(&self, local: usize) -> Option<Arc<super::object::Fields>> {
		self.relay.as_ref()?.objects.get(local).cloned().flatten()
	}

	/// Charge the in-flight frame's bytes written since its last charge, as a write access.
	///
	/// Returns the coarse tick it stamped, like [`cache::Charge::add`].
	fn charge_partial(&mut self) -> Option<u64> {
		let written = match &mut self.partial {
			// Scratch is charged as the slice it commits as.
			Some(partial) if partial.scratch => 0,
			Some(partial) => {
				let written = partial.buf.written(Ordering::Acquire) as u64;
				written - std::mem::replace(&mut partial.charged, written)
			}
			None => 0,
		};
		self.cache += written;
		let slots = match &self.partial {
			Some(partial) if partial.fields.is_some() => self.reserve_frame() + self.reserve_fields(),
			Some(_) => self.reserve_frame(),
			None => 0,
		};
		self.charge.add(written + slots)
	}

	/// Whether the frames held start at the group's first object.
	fn first_object(&self) -> bool {
		self.wire().map_or(self.offset == 0, |wire| wire.first_object)
	}

	/// The committed frames, for a successor copy to adopt. Sparse Object IDs refuse
	/// ordinal recovery, so they lend nothing.
	fn head(&self) -> Option<Head> {
		if self.frames.is_empty() || self.sparse() {
			return None;
		}
		Some(Head {
			start: self.offset,
			first_object: self.first_object(),
			frames: self.frames.iter().cloned().collect(),
			objects: self
				.relay
				.as_ref()
				.map_or_else(Vec::new, |relay| relay.objects.iter().cloned().collect()),
			sizes: self.flate.as_ref().map(|flate| flate.sizes.iter().copied().collect()),
		})
	}

	/// Drop the cached frames (and any in-flight tail) and release their pool charge.
	///
	/// With `salvage`, the committed frames move to the readers that could resume the
	/// group elsewhere, if any are left, instead of being dropped.
	fn release(&mut self, salvage: bool) {
		let handoff = match salvage && !self.frames.is_empty() && !self.sparse() {
			true => self.handoff.lock().unwrap_or_else(PoisonError::into_inner).upgrade(),
			false => None,
		};
		let head = handoff.as_ref().map(|_| Head {
			start: self.offset,
			first_object: self.first_object(),
			frames: std::mem::take(&mut self.frames).into(),
			objects: self
				.relay
				.as_mut()
				.map_or_else(Vec::new, |relay| std::mem::take(&mut relay.objects).into()),
			sizes: self.flate.as_mut().map(|flate| std::mem::take(&mut flate.sizes).into()),
		});
		self.frames = VecDeque::new();
		self.partial = None;
		self.cache = 0;
		self.charge.clear();
		if let Some(flate) = &mut self.flate {
			flate.release(self.abort.clone().unwrap_or(Error::Dropped));
		}
		// The header state and terminal object outlive the frames.
		if let Some(relay) = &mut self.relay {
			relay.objects = VecDeque::new();
			relay.bytes = size_of::<Relay>() as u64 + relay.end_object.as_ref().map_or(0, |fields| fields.heap());
			self.charge.add(relay.bytes);
		}
		// Billed once the group's own charge is gone, so the head only takes its place.
		if let (Some(handoff), Some(head)) = (handoff, head)
			&& let Some(track) = self.charge.account()
			&& let Some(bill) = track.hold(head.bytes())
		{
			let salvage = Salvage {
				head: Arc::new(head),
				bill,
			};
			*handoff.salvaged.lock().unwrap_or_else(PoisonError::into_inner) = Some(salvage);
			// Only once it is held: a sweep finding the hand-off empty forgets it.
			track.pool().hold_head(&handoff);
		}
	}
}

/// `timestamp` at the track's `timescale`.
///
/// A track is all timed or all untimed, so a frame must match it: a timed frame on an
/// untimed track, or an untimed one on a timed track, is [`Error::TimestampMismatch`],
/// as is a timestamp the track's scale can't hold.
pub(super) fn on_track(timestamp: Option<Timestamp>, timescale: Option<Timescale>) -> Result<Option<Timestamp>> {
	match (timestamp, timescale) {
		(Some(timestamp), Some(timescale)) => Ok(Some(
			timestamp.convert(timescale).map_err(|_| Error::TimestampMismatch)?,
		)),
		(None, None) => Ok(None),
		_ => Err(Error::TimestampMismatch),
	}
}

fn modify(state: &kio::Producer<GroupState>) -> Result<kio::Mut<'_, GroupState>> {
	state.write().map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))
}

/// Writes frames to a group in order.
///
/// Each group is delivered independently over a QUIC stream.
/// Use [Self::write_frame] for simple single-buffer frames,
/// or [Self::create_frame] for multi-chunk streaming writes.
///
/// The writer cannot be cloned; move it to its next owner instead.
///
/// ```compile_fail
/// # fn cannot_clone(producer: moq_net::group::Producer) {
/// let _: moq_net::group::Producer = producer.clone();
/// # }
/// ```
#[must_use = "dropping an unfinished group aborts it with `Dropped`"]
pub struct Producer {
	handle: Handle,
	stats: stats::Meter,
}

/// Shared group state without permission to write frames or finish the group.
#[derive(Clone)]
// private: capability gap
// Sessions need an abortable observer while public callers use Demand.
pub(crate) struct Handle {
	state: kio::Producer<GroupState>,
	info: Info,
	track: track::Info,
	cache: Arc<cache::Track>,
	alive: Arc<Alive>,
}

struct Alive {
	// Monotone mirror of `GroupState::abort` for track scans that already hold the
	// track lock. A stale false only hands out a group that is concurrently aborting;
	// true is stored after the abort exists, so it can never hide a live group.
	//
	// Only ever read on its own. A decision that pairs the abort with something else
	// out of `GroupState` has to read both under one guard, or the two halves can
	// straddle the abort: see `Handle::live_first_frame`.
	aborted: AtomicBool,
	// Set once frame 0's rewind check ran, so later writes skip the track lock it takes. A
	// stale false only takes that lock once more.
	checked: AtomicBool,
	// The abort's error when it was the publisher giving the group up rather than cache
	// policy (`Old`, `Timeout`, `Evicted`). Set before `aborted`, under the same rules, so
	// a track scan learns why without taking the group lock.
	cut: OnceLock<Error>,
	// Monotone mirror of `GroupState::started` for the first frame or the finish, set after
	// the group lock is released and before the track is told: a track scan reading a stale
	// false judges the group frameless until that wake re-judges it.
	started: AtomicBool,
	// The cache stamp `GroupState::charge` maintains, held here as well so the
	// eviction and expiry walks can weigh a candidate without taking the group lock
	// they already hold the track lock over.
	access: Arc<cache::Access>,
}

impl Drop for Producer {
	fn drop(&mut self) {
		if let Ok(state) = self.handle.state.write() {
			if state.fin.is_some() || state.abort.is_some() {
				return;
			}
			tracing::warn!(
				sequence = self.sequence,
				"group::Producer dropped without finish() or abort()"
			);
			self.handle.commit_abort(state, Error::Dropped);
		}
	}
}

impl std::ops::Deref for Handle {
	type Target = Info;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl std::ops::Deref for Producer {
	type Target = Info;

	fn deref(&self) -> &Self::Target {
		&self.handle.info
	}
}

impl Producer {
	#[cfg(feature = "async")]
	// private: passthrough
	pub(crate) fn wire(&self) -> Option<Wire> {
		self.handle.state.read().wire().cloned()
	}

	#[cfg(feature = "async")]
	// private: passthrough
	pub(crate) fn set_wire(&self, mut wire: Wire) -> Result<()> {
		let mut state = modify(&self.handle.state)?;
		// A head adopted before the header arrived holds the objects below the stream's.
		if let Some(first_object) = state.adopted {
			wire.first_object = first_object;
			wire.first_id = state.offset as u64;
			wire.last = wire.last.or((state.next_index as u64).checked_sub(1));
		}
		state.relay().wire = Some(wire);
		Ok(())
	}

	#[cfg(feature = "async")]
	// private: passthrough
	pub(crate) fn mark_gap(&self, id: u64, gap: Option<u64>) -> Result<()> {
		if gap.is_some_and(|gap| gap > id) {
			return Err(Error::MalformedTrack);
		}
		let mut state = modify(&self.handle.state)?;
		let offset = state.offset as u64;
		if let Some(wire) = state.wire_mut() {
			let missing = match wire.last {
				None => id.checked_sub(offset).ok_or(Error::MalformedTrack)?,
				Some(prior) => id
					.checked_sub(prior)
					.and_then(|step| step.checked_sub(1))
					.ok_or(Error::MalformedTrack)?,
			};
			if wire.last.is_some() && gap.is_some_and(|gap| gap > missing) {
				return Err(Error::MalformedTrack);
			}
			// A first object's gap may also cover IDs below where this copy starts.
			wire.fetchable &= missing == 0 || gap.is_some_and(|gap| gap >= missing);
		}
		Ok(())
	}

	#[cfg(feature = "async")]
	// private: passthrough
	pub(crate) fn set_end_object(&self, fields: super::object::Fields) -> Result<()> {
		let mut state = modify(&self.handle.state)?;
		if state.fin.is_some() || state.relay.as_ref().is_some_and(|relay| relay.end_object.is_some()) {
			return Err(Error::MalformedTrack);
		}
		let bytes = fields.heap();
		if state.would_overflow(0, bytes) {
			return Err(Error::GroupTooLarge);
		}
		state.charge.add(bytes);
		let relay = state.relay();
		relay.bytes += bytes;
		if let Some(wire) = &mut relay.wire {
			wire.last = Some(fields.id);
		}
		relay.end_object = Some(Arc::new(fields));
		Ok(())
	}

	/// Create a group producer bound to its parent track's [`track::Info`] and cache
	/// account.
	///
	/// Crate-private: groups are only constructed via [`track::Producer`], which
	/// threads both down so properties like the timescale are inherited rather than
	/// passed in. Every frame added to this group is normalized to the track's
	/// timescale by [`Self::create_frame`].
	///
	/// Charges the group into `cache`, so its cached bytes count against the budget the
	/// track evicts toward under memory pressure.
	pub(super) fn new(info: Info, track: track::Info, cache: Arc<cache::Track>) -> Self {
		let state = kio::Producer::<GroupState>::default();
		let charge = cache.charge();
		let access = charge.access();
		{
			let mut state = state.write().ok().expect("a new group is open");
			if track.compress {
				state.flate = Some(Box::new(Flate::new(charge.extra())));
			}
			state.charge = charge;
		}
		let alive = Arc::new(Alive {
			aborted: AtomicBool::new(false),
			checked: AtomicBool::new(false),
			cut: OnceLock::new(),
			started: AtomicBool::new(false),
			access,
		});
		Self {
			handle: Handle {
				info,
				state,
				track,
				cache,
				alive,
			},
			stats: stats::Meter::default(),
		}
	}

	/// Attach an ingress payload meter, counting this as one delivered group.
	/// Called by a tagged [`track::Producer`] when it creates the group.
	pub(super) fn with_meter(mut self, meter: stats::Meter) -> Self {
		meter.group();
		self.stats = meter;
		self
	}

	/// The group's header, as it was created.
	pub fn info(&self) -> Info {
		self.handle.info
	}

	/// The parent track's timescale, or `None` when it declared no timeline.
	pub fn timescale(&self) -> Option<Timescale> {
		self.handle.track.timescale
	}

	/// Whether the parent track compresses, so a received frame arrives as a stored slice
	/// with its inflated size ([`frame::Header::inflated`]).
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	// private: passthrough
	pub(crate) fn compressed(&self) -> bool {
		self.handle.track.compress
	}

	/// The track to check this group's start against, while frame 0 may still be unwritten.
	///
	/// The caller holds its lock until the frame is recorded, so the check and the stamp are
	/// one step against every other group's (locks are ordered track then group). Only a
	/// timed group not yet checked pays for it, once.
	fn first_frame_track(&self) -> Option<kio::Producer<track::TrackState>> {
		if self.handle.track.timescale.is_none() || self.handle.alive.checked.load(Ordering::Relaxed) {
			return None;
		}
		self.handle.cache.track()
	}

	/// Start the group at frame `index` rather than 0, so the first frame written lands
	/// there.
	///
	/// A group can be short at its front or its back, never in the middle: this trims
	/// the front, and simply stopping (then [`finish`](Self::finish)ing) trims the back.
	/// The frames below `index` are not a gap this handle will ever fill, so a reader
	/// positioned below it gets [`Error::Lagged`]. They belong to whoever produced the
	/// head of the group, typically another route serving the same track (see
	/// [`crate::track::Subscriber`]).
	///
	/// The counterpart of [`Consumer::set_frames`], which positions a *reader* the same
	/// way. Where the group begins is part of its shape, so this must come before the
	/// first frame; afterwards it returns [`Error::Closed`].
	pub fn start_at(&mut self, index: u64) -> Result<()> {
		let index = usize::try_from(index).map_err(|_| Error::BoundsExceeded(crate::coding::BoundsExceeded))?;
		if index == usize::MAX {
			return Err(Error::BoundsExceeded(crate::coding::BoundsExceeded));
		}

		let mut state = modify(&self.handle.state)?;
		// Every write advances `next_index` past `offset`, so this is "nothing written
		// yet".
		if state.fin.is_some() || state.next_index != state.offset {
			return Err(Error::Closed);
		}
		state.offset = index;
		state.next_index = index;
		state.committed = index;
		Ok(())
	}

	/// A helper method to write a frame from a single byte buffer.
	///
	/// If you want to write multiple chunks, use [Self::create_frame] to get a frame producer.
	/// But an upfront size is required.
	///
	/// `timestamp` is converted into the parent track's timescale. Pass `None` on an
	/// untimed track; a frame that doesn't match its track is [`Error::TimestampMismatch`].
	pub fn write_frame<B: IntoBytes>(&mut self, timestamp: impl Into<Option<Timestamp>>, data: B) -> Result<()> {
		let timestamp = on_track(timestamp.into(), self.handle.track.timescale)?;
		let payload = data.into_bytes();
		if payload.len() as u64 > MAX_CACHE_BYTES {
			return Err(Error::FrameTooLarge);
		}

		let producer = self.first_frame_track();
		let track = producer.as_ref().map(|track| track.read());
		let neighbors = track
			.as_ref()
			.and_then(|track| track.neighbors(self.handle.info.sequence));
		let mut state = modify(&self.handle.state)?;
		if state.fin.is_some() {
			return Err(Error::Closed);
		}
		debug_assert!(state.partial.is_none(), "a frame is already open");
		let next_index = state
			.next_index
			.checked_add(1)
			.ok_or(Error::BoundsExceeded(crate::coding::BoundsExceeded))?;
		state.admit(next_index)?;
		let origin = state.check_start(neighbors.as_ref(), timestamp)?;
		debug_assert!(state.partial.is_none(), "a frame is already open");
		let inflated = payload.len() as u64;
		let payload = match &mut state.flate {
			Some(flate) => flate.deflate(&payload),
			None => payload,
		};
		let size = payload.len() as u64;
		if state.would_overflow(1, size) || state.inflated_overflow(inflated) {
			return Err(self.abort_too_large(state));
		}
		state.cache += size;
		let slots = state.reserve_frame();
		let now = state.charge.add(size + slots);
		state.frames.push_back(Frame {
			timestamp,
			payload,
			arrived: super::clock::now(),
		});
		if let Some(flate) = &mut state.flate {
			flate.push(inflated);
		}
		state.next_index = next_index;
		state.committed = state.next_index;
		state.stamp(timestamp);
		let bound = state.settle(origin);
		self.handle.alive.checked.store(true, Ordering::Relaxed);
		self.advance_largest(&state);
		drop(state);
		// The group's lock is released first: indexing reads it back through its slot.
		if let Some(track) = &track {
			track.first_frame(self.handle.info.sequence, bound);
		}
		drop(track);
		self.handle.start(producer);

		// With the group lock released (lock order is track then group), settle
		// eviction debt if enough has been written since the track last paid.
		self.handle.cache.settle(now);
		// An untimed frame presents nothing, so it can expire no read.
		if let Some(timestamp) = timestamp {
			self.handle.cache.wakes().presented(timestamp);
		}

		// Ingress payload: one whole frame written.
		self.stats.frames(1);
		self.stats.bytes(size);
		Ok(())
	}

	/// Write a whole batch of frames at once, draining `frames`.
	///
	/// One lock covers the batch, so an ingest with several frames in hand pays the
	/// group mutex and the track's eviction settle once rather than per frame. Build
	/// the batch with [`frame::Buffer::push`].
	///
	/// The batch is validated before anything is written, so a rejected frame leaves
	/// both the group and the buffer exactly as they were, ready to retry or redirect.
	pub fn write_frames<const N: usize>(&mut self, frames: &mut frame::Buffer<N>) -> Result<()> {
		for frame in frames.filled() {
			on_track(frame.timestamp, self.handle.track.timescale)?;
			if frame.payload.len() as u64 > MAX_CACHE_BYTES {
				return Err(Error::FrameTooLarge);
			}
		}

		let count = frames.len();
		let bytes: u64 = frames.filled().iter().map(|frame| frame.payload.len() as u64).sum();
		let producer = self.first_frame_track();
		let track = producer.as_ref().map(|track| track.read());
		let neighbors = track
			.as_ref()
			.and_then(|track| track.neighbors(self.handle.info.sequence));
		let mut state = modify(&self.handle.state)?;
		if state.fin.is_some() {
			return Err(Error::Closed);
		}
		debug_assert!(state.partial.is_none(), "a frame is already open");
		let next_index = state
			.next_index
			.checked_add(count)
			.ok_or(Error::BoundsExceeded(crate::coding::BoundsExceeded))?;
		state.admit(next_index)?;
		let first = frames.filled().first().map(|frame| frame.timestamp);
		let origin = match first {
			Some(timestamp) => state.check_start(
				neighbors.as_ref(),
				on_track(timestamp, self.handle.track.timescale).expect("timestamp scale checked above"),
			)?,
			None => state.origin,
		};
		// A compressed group stores slices, compressed aside so a refused batch is untouched.
		let mut deflated = arrayvec::ArrayVec::<Bytes, N>::new();
		if let Some(flate) = &mut state.flate {
			deflated.extend(frames.filled().iter().map(|frame| flate.deflate(&frame.payload)));
		}
		let stored = match state.flate.is_some() {
			true => deflated.iter().map(|slice| slice.len() as u64).sum(),
			false => bytes,
		};
		if state.would_overflow(count, stored) || state.inflated_overflow(bytes) {
			return Err(self.abort_too_large(state));
		}

		// The last frame's tick, reused below so settling does not re-read the clock.
		let mut now = None;
		let mut latest = None;
		let mut deflated = deflated.into_iter();
		// The whole batch lands at once.
		let arrived = super::clock::now();
		for mut frame in frames.drain() {
			frame.arrived = arrived;
			frame.timestamp =
				on_track(frame.timestamp, self.handle.track.timescale).expect("timestamp scale checked above");
			if let Some(flate) = &mut state.flate {
				flate.push(frame.payload.len() as u64);
				frame.payload = deflated.next().expect("one slice per frame");
			}
			let size = frame.payload.len() as u64;
			state.cache += size;
			let slots = state.reserve_frame();
			now = state.charge.add(size + slots);
			state.stamp(frame.timestamp);
			latest = frame.timestamp;
			state.frames.push_back(frame);
		}
		state.next_index = next_index;
		state.committed = next_index;
		let bound = state.settle(origin);
		if count > 0 {
			self.handle.alive.checked.store(true, Ordering::Relaxed);
			self.advance_largest(&state);
		}
		drop(state);
		// The group's lock is released first: indexing reads it back through its slot.
		if let Some(track) = &track {
			track.first_frame(self.handle.info.sequence, bound);
		}
		drop(track);
		if count > 0 {
			self.handle.start(producer);
		}

		self.handle.cache.settle(now);
		if let Some(latest) = latest {
			self.handle.cache.wakes().presented(latest);
		}
		self.stats.frames(count as u64);
		self.stats.bytes(stored);
		Ok(())
	}

	/// Create a frame with an upfront size and presentation timestamp, streamed in
	/// chunks. The returned [`frame::Producer`] takes this writer, so only one frame is
	/// open at a time; its [`finish`](frame::Producer::finish) hands the writer back.
	/// Use [`Self::write_frame`] for a frame already in one buffer.
	///
	/// The `timestamp` is converted into the parent track's timescale, so the scale you
	/// build it with doesn't have to match the track. A frame the group cannot take
	/// aborts the group with the error: [`Error::FrameTooLarge`] if the declared size
	/// exceeds the group's byte budget (refused before allocating),
	/// [`Error::TimestampMismatch`] if the timestamp can't be converted, or
	/// [`Error::Closed`] once the group finished.
	///
	/// The declared size is allocated up front while the frames in flight through the
	/// origin handle fit its budget, and grows with the bytes written past that, so a
	/// peer's declaration cannot commit memory it never sends.
	pub fn create_frame(self, frame: frame::Info) -> Result<frame::Producer> {
		self.create_object(frame.into())
	}

	/// [`Self::create_frame`] with the moq-transport object fields a relay forwards.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	// private: passthrough
	pub(crate) fn create_object(mut self, frame: frame::Header) -> Result<frame::Producer> {
		let meter = self.chunk_meter(frame.inflated);
		let size = frame.info.size as usize;
		let reserved = self.handle.cache.frames().reserve(size);
		let buf = match reserved {
			Some(_) => FrameBuf::new(size),
			None => FrameBuf::growing(size),
		};
		let info = match self.open_frame(frame, &buf) {
			Ok(info) => info,
			Err(err) => {
				let _ = self.handle.abort(err.clone());
				return Err(err);
			}
		};
		Ok(frame::Producer::new(self, buf, info, reserved).with_meter(meter))
	}

	/// The meter a streamed frame's chunks bump: none for a plain frame a compressed group
	/// compresses on commit, which counts its stored slice then, as `bytes` counts payload
	/// as stored.
	fn chunk_meter(&self, inflated: Option<u64>) -> stats::Meter {
		match self.handle.track.compress && inflated.is_none() {
			true => stats::Meter::default(),
			false => self.stats.clone(),
		}
	}

	/// Open `buf` as the in-flight frame, returning its header in the track's timescale.
	///
	/// Its bytes are charged to the cache as they are written, not here: the declared
	/// size is only a promise until they arrive.
	fn open_frame(&mut self, header: frame::Header, buf: &FrameBuf) -> Result<frame::Info> {
		let frame = header.info;
		let timestamp = on_track(frame.timestamp, self.handle.track.timescale)?;
		if frame.size > MAX_CACHE_BYTES || header.inflated.is_some_and(|inflated| inflated > MAX_CACHE_BYTES) {
			return Err(Error::FrameTooLarge);
		}
		// Only a compressed group takes a stored slice, and every frame a local producer
		// streams into one is plain until it commits.
		if header.inflated.is_some() && !self.handle.track.compress {
			return Err(Error::ProtocolViolation);
		}
		let scratch = self.handle.track.compress && header.inflated.is_none();
		let inflated = header.inflated.unwrap_or(frame.size);

		let producer = self.first_frame_track();
		let track = producer.as_ref().map(|track| track.read());
		let neighbors = track
			.as_ref()
			.and_then(|track| track.neighbors(self.handle.info.sequence));
		let mut state = modify(&self.handle.state)?;
		if state.fin.is_some() {
			return Err(Error::Closed);
		}
		debug_assert!(state.partial.is_none(), "a frame is already open");
		let next_index = state
			.next_index
			.checked_add(1)
			.ok_or(Error::BoundsExceeded(crate::coding::BoundsExceeded))?;
		state.admit(next_index)?;
		let origin = state.check_start(neighbors.as_ref(), timestamp)?;
		// Only one frame is ever in flight, so the declared size is the most the cache
		// can grow by before the next check.
		let metadata_bytes = header.fields.as_ref().map_or(0, |fields| fields.heap());
		if state.would_overflow(1, frame.size.saturating_add(metadata_bytes)) || state.inflated_overflow(inflated) {
			return Err(self.abort_too_large(state));
		}
		if let (Some(flate), Some(inflated)) = (&mut state.flate, header.inflated) {
			flate.inflated += inflated;
		}
		let now = state.charge.record_write();
		state.partial = Some(Partial {
			timestamp,
			fields: header.fields,
			buf: buf.clone(),
			charged: 0,
			inflated: header.inflated,
			scratch,
		});
		if metadata_bytes > 0 {
			state.relay().bytes += metadata_bytes;
			state.charge.add(metadata_bytes);
		}
		let object_id = state
			.partial
			.as_ref()
			.and_then(|partial| partial.fields.as_ref())
			.map_or(state.next_index as u64, |fields| fields.id);
		if let Some(wire) = state.wire_mut() {
			wire.last = Some(object_id);
			wire.sparse |= object_id != (next_index - 1) as u64;
		}
		state.next_index = next_index;
		// Opening the frame is enough: the header carries the timestamp, so the group's
		// place in time is known before a single payload byte streams in.
		state.stamp(timestamp);
		let bound = state.settle(origin);
		self.handle.alive.checked.store(true, Ordering::Relaxed);
		drop(state);
		// The group's lock is released first: indexing reads it back through its slot.
		if let Some(track) = &track {
			track.first_frame(self.handle.info.sequence, bound);
		}
		drop(track);
		self.handle.start(producer);

		// With the group lock released (lock order is track then group), settle
		// eviction debt if enough has been written since the track last paid.
		self.handle.cache.settle(now);
		// An untimed frame presents nothing, so it can expire no read.
		if let Some(timestamp) = timestamp {
			self.handle.cache.wakes().presented(timestamp);
		}

		// Ingress payload: one frame opened; its bytes are counted per chunk as the
		// producer writes them.
		self.stats.frames(1);

		Ok(frame::Info {
			size: frame.size,
			timestamp,
		})
	}

	/// Wake consumers parked on the group channel (called after a partial write).
	pub(super) fn frame_notify(&self) {
		// The chunk that was just written is charged, and is a write access: restart the
		// retention clock so a straggler group streaming a large frame isn't expired
		// mid-write. `charge_partial` takes `&mut`, which marks the guard modified: kio
		// only notifies on a mutably-accessed guard's release, and that notify is what
		// delivers the chunk to parked readers.
		let now = self
			.handle
			.state
			.write()
			.ok()
			.and_then(|mut state| state.charge_partial());
		// A long streamed frame also counts as track activity for the independent
		// expiry time gate.
		self.handle.cache.settle(now);
	}

	/// Commit the in-flight frame as a completed frame (called by [`frame::Producer::finish`]).
	pub(super) fn frame_commit(&mut self, frame: Frame) -> Result<()> {
		let mut state = modify(&self.handle.state)?;
		// Completing the frame charges whatever it wrote since the last notify, and is a
		// write access like any chunk, and the only one the payload is guaranteed to get:
		// the wire ingest defers its chunk notifications to the poll boundary, so a tail
		// that arrives and completes in one turn never reaches [`Self::frame_notify`].
		// Without this, a group whose payload streamed in across an idle gap would expire
		// the instant it finished.
		let mut now = state.charge_partial();
		let partial = state.partial.take();
		let mut frame = frame;
		if partial.as_ref().is_some_and(|partial| partial.scratch) {
			let flate = state.flate.as_mut().expect("scratch only opens on a compressed group");
			let inflated = frame.payload.len() as u64;
			frame.payload = flate.deflate(&frame.payload);
			flate.push(inflated);
			let size = frame.payload.len() as u64;
			// The frame already counts against the group's frames.
			if state.would_overflow(0, size) {
				return Err(self.abort_too_large(state));
			}
			state.cache += size;
			now = state.charge.add(size);
			self.stats.bytes(size);
		} else if let (Some(flate), Some(inflated)) = (&mut state.flate, partial.as_ref().and_then(|p| p.inflated)) {
			// Admitted against the budget when it opened.
			flate.sizes.push_back(inflated);
		}
		if let Some(fields) = partial.and_then(|partial| partial.fields) {
			// The slot was reserved and charged by `charge_partial` above.
			let len = state.frames.len();
			let objects = &mut state.relay().objects;
			objects.resize(len, None);
			objects.push_back(Some(fields));
		}
		// Stamped once readable, so a slow DEFLATE above counts against its lateness.
		frame.arrived = super::clock::now();
		state.frames.push_back(frame);
		state.committed = state.next_index;
		self.advance_largest(&state);
		drop(state);

		// With the group lock released (lock order is track then group), settle
		// eviction debt and age idle content out, reusing the tick above.
		self.handle.cache.settle(now);
		Ok(())
	}

	/// Raise the track's largest to the last committed frame.
	fn advance_largest(&self, state: &GroupState) {
		self.handle.cache.advance_largest(track::Position {
			group: self.handle.info.sequence,
			frame: (state.committed - 1) as u64,
		});
	}

	/// Fail the group because an in-flight frame couldn't complete (called by
	/// [`frame::Producer::abort`] / its drop).
	pub(super) fn frame_abort(&mut self, err: Error) {
		let _ = self.handle.close_aborted(err);
	}

	/// Mark the group as complete; no more frames will be written.
	///
	/// Borrows rather than consumes, so a later failure can still be reported through
	/// [`abort`](Self::abort). The handle also keeps the cached frames readable.
	pub fn finish(&mut self) -> Result<()> {
		let mut state = modify(&self.handle.state)?;
		debug_assert!(state.partial.is_none(), "a frame is already open");
		let fin = state.next_index;
		state.fin = Some(fin);
		// No frame follows to compress.
		if let Some(flate) = &mut state.flate {
			flate.finish(fin);
		}
		drop(state);
		// An empty group counts as content once it ends.
		self.handle.start(None);
		// A finished group can no longer raise the track's largest, which may end its stream.
		self.handle.cache.finish_largest(self.sequence);
		Ok(())
	}

	/// Abort the group with the given error.
	///
	/// Consumes the writer. Drops the cached frames so a stale [`Consumer`] can't pin
	/// their buffers in memory forever; consumers that haven't drained yet surface the
	/// abort error instead of the leftover cache.
	pub fn abort(self, err: Error) -> Result<()> {
		self.handle.close_aborted(err)
	}

	/// Abort a write that would grow the group past its budget, holding the lock already
	/// taken for that write so nothing else lands in between.
	fn abort_too_large(&self, state: kio::Mut<'_, GroupState>) -> Error {
		self.handle.commit_abort(state, Error::GroupTooLarge);
		Error::GroupTooLarge
	}

	/// Block until the group is aborted, including by eviction. Finishing does not close it.
	#[cfg(feature = "async")]
	pub async fn closed(&self) -> Error {
		kio::wait(|waiter| self.handle.poll_closed(waiter)).await
	}
	/// The non-writing observer used by the cache and built-in sessions.
	// private: capability gap
	// Sessions need an abortable observer while public callers use Demand.
	pub(crate) fn handle(&self) -> Handle {
		self.handle.clone()
	}

	/// One past the index of the last frame written (completed or in-flight), which is
	/// also the index the next frame will get.
	///
	/// Counts any frames the group [started past](Producer::start_at), so it's the group's
	/// logical length rather than the number of frames this handle holds.
	pub fn frame_count(&self) -> usize {
		self.handle.frame_count()
	}

	/// Abort with `err` only while nothing consumes the group, returning whether it is
	/// closed. Consumer creation and the check share a lock, so a reader arriving after
	/// [`poll_unused`](Self::poll_unused) keeps the group alive instead of reading the abort.
	#[allow(
		dead_code,
		reason = "the answering API retains these on the writer as well as the session observer"
	)]
	// pending: /quest/m1/public-model/answer.md
	pub(crate) fn abort_unused(&self, err: Error) -> bool {
		self.handle.abort_unused(err)
	}

	/// Whether the group has been aborted (including pool eviction). The track's
	/// read paths treat an aborted cached group as absent.
	///
	/// Reads the mirror rather than the group's state, so a track scan holding the
	/// track lock never takes the group's. Monotone, and only ever conservative: a
	/// concurrent abort can still read as live for the length of [`Self::abort`],
	/// which hands out a group whose consumer then surfaces the abort.
	// private: test
	pub(crate) fn is_aborted(&self) -> bool {
		self.handle.is_aborted()
	}

	/// Create a new consumer for the group.
	pub fn consume(&self) -> Consumer {
		self.handle.consume()
	}

	/// Poll until the group is aborted, including by eviction; ready with the cause.
	/// Finishing does not close it.
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Error> {
		self.handle.poll_closed(waiter)
	}

	/// Watch reader demand without keeping the group alive.
	pub fn demand(&self) -> Demand {
		self.handle.demand()
	}

	/// Poll for the group becoming unused (every consumer dropped).
	#[allow(
		dead_code,
		reason = "the answering API retains these on the writer as well as the session observer"
	)]
	// pending: /quest/m1/public-model/answer.md
	pub(crate) fn poll_unused(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.handle.poll_unused(waiter)
	}
}

impl Handle {
	/// Abort the group without taking ownership of its writer.
	// private: capability gap
	// Sessions cancel a group while its writer is in a frame.
	pub(crate) fn abort(&self, err: Error) -> Result<()> {
		self.close_aborted(err)
	}

	/// Abort the group with `err` unless it already holds every frame: the publisher gave
	/// up a group still arriving, while one that finished here stays whole.
	// private: capability gap
	// Sessions cancel an unfinished FETCH while its writer belongs to its data stream.
	pub(crate) fn abort_open(&self, err: Error) {
		if let Ok(state) = self.state.write()
			&& state.fin.is_none()
		{
			self.commit_abort(state, err);
		}
	}

	/// Adopt the frames of `head` below where this copy starts, so it holds the group
	/// from `head`'s start, as if this route had delivered them.
	///
	/// A no-op unless `head` starts below this copy and reaches it, and the frames fit the
	/// group's budget. Readers keep their positions, since frames keep their indices. A
	/// compressed group adopts only a compressed head, taking its inflated sizes beside the
	/// slices, so its plain readers inflate from the adopted frames.
	pub(super) fn adopt_head(&self, head: &Head) {
		let Ok(mut state) = self.state.write() else {
			return;
		};
		let offset = state.offset;
		let reaches = head.start.saturating_add(head.frames.len()) >= offset;
		let same_form = head.sizes.is_some() == state.flate.is_some();
		if state.abort.is_some() || state.sparse() || !same_form || head.start >= offset || !reaches {
			return;
		}
		let count = offset - head.start;
		let frames = &head.frames[..count];
		let fields = || head.objects.iter().take(count).flatten();
		let payload: u64 = frames.iter().map(|frame| frame.payload.len() as u64).sum();
		let metadata: u64 = fields().map(|fields| fields.heap()).sum();
		let sizes = head.sizes.as_ref().map(|sizes| &sizes[..count]);
		let inflated: u64 = sizes.into_iter().flatten().sum();
		// The in-flight frame's remainder was admitted at its open and is charged as it
		// streams in, without another check.
		if state.would_overflow(count, payload + metadata + state.unwritten()) || state.inflated_overflow(inflated) {
			return;
		}
		if let (Some(flate), Some(sizes)) = (&mut state.flate, sizes) {
			for size in sizes.iter().rev() {
				flate.sizes.push_front(*size);
			}
			flate.inflated += inflated;
		}

		let before = state.frames.capacity().saturating_sub(FRAME_SLOTS);
		state.frames.reserve(count);
		let mut growth = ((state.frames.capacity().saturating_sub(FRAME_SLOTS) - before) * size_of::<Frame>()) as u64;
		for frame in frames.iter().rev() {
			state.frames.push_front(frame.clone());
		}
		// Keep the fields aligned with the frames: the adopted ones lead.
		if fields().next().is_some() || state.relay.as_ref().is_some_and(|relay| !relay.objects.is_empty()) {
			let relay = state.relay();
			let before = relay.objects.capacity();
			relay.objects.reserve(count);
			let slots = ((relay.objects.capacity() - before) * size_of::<Option<Arc<super::object::Fields>>>()) as u64;
			for index in (0..count).rev() {
				relay.objects.push_front(head.objects.get(index).cloned().flatten());
			}
			relay.bytes += slots + metadata;
			growth += slots;
		}
		state.offset = head.start;
		state.cache += payload;
		let start = frames.iter().find_map(|frame| frame.timestamp);
		state.timeline = match (state.timeline, start) {
			(Timeline::Timed { latest, .. }, Some(start)) => Timeline::Timed { start, latest },
			(Timeline::Empty, Some(start)) => Timeline::Timed {
				start,
				latest: frames.iter().rev().find_map(|frame| frame.timestamp).unwrap_or(start),
			},
			(Timeline::Empty, None) => Timeline::Untimed,
			(timeline, _) => timeline,
		};
		state.adopted = Some(head.first_object);
		if let Some(wire) = state.wire_mut() {
			wire.first_object = head.first_object;
			wire.first_id = head.start as u64;
			wire.last = wire.last.or(Some(offset as u64 - 1));
		}
		let now = state.charge.add(payload + metadata + growth);
		drop(state);
		self.start(None);
		self.cache.settle(now);
	}

	/// One past the index of the last frame written (completed or in-flight), which is
	/// also the index the next frame will get.
	///
	/// Counts any frames the group [started past](Producer::start_at), so it's the group's
	/// logical length rather than the number of frames this handle holds.
	// private: capability gap
	// Sessions observe a group while its sole writer belongs to a frame.
	pub(crate) fn frame_count(&self) -> usize {
		self.state.read().next_index
	}

	/// Cap the group at `frames`, for a track end inside it, or lift the cap with `None`:
	/// later writes past the cap are [`Error::Closed`]. A frame already at or past the cap is
	/// [`Error::ProtocolViolation`]. An aborted group gets no more frames, so it takes no cap.
	pub(super) fn limit(&self, frames: Option<u64>) -> Result<()> {
		let Ok(mut state) = self.state.write() else {
			return Ok(());
		};
		let frames = frames.map(|frames| usize::try_from(frames).unwrap_or(usize::MAX));
		if frames.is_some_and(|frames| state.next_index > frames) {
			return Err(Error::ProtocolViolation);
		}
		state.limit = frames;
		Ok(())
	}

	/// Abort with `err` only while nothing consumes the group, returning whether it is
	/// closed. Consumer creation and the check share a lock, so a reader arriving after
	/// [`poll_unused`](Self::poll_unused) keeps the group alive instead of reading the abort.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	// pending: /quest/m1/public-model/answer.md
	pub(crate) fn abort_unused(&self, err: Error) -> bool {
		match self.state.write_unused() {
			kio::Unused::Idle(guard) => {
				self.commit_abort(guard, err);
				true
			}
			kio::Unused::Closed => true,
			kio::Unused::Used => false,
		}
	}

	fn close_aborted(&self, err: Error) -> Result<()> {
		self.commit_abort(modify(&self.state)?, err);
		Ok(())
	}

	fn commit_abort(&self, mut guard: kio::Mut<'_, GroupState>, err: Error) {
		let policy = matches!(err, Error::Old | Error::Timeout | Error::Evicted);
		// Cache policy frees the frames, as does an oversized group whichever write found
		// it; a copy that failed or was given up lends them on.
		let salvage = !policy && !matches!(err, Error::GroupTooLarge);
		// A reset carrying a policy code is the upstream's cache policy, a cancel is a reader
		// no longer wanting the group, and a fill refused as unfetchable abandons backfill:
		// deliberate holes, never a cut readers see. The publisher giving the group up is.
		let cut = !policy
			&& !matches!(
				err,
				Error::Cancel
					| Error::Lagged | Error::NotFetchable
					| Error::Stream(
						StreamError::Cancel
							| StreamError::Old | StreamError::Evicted
							| StreamError::DeliveryTimeout
							| StreamError::TooFarBehind
							| StreamError::NotFetchable
					)
			);
		if cut {
			let _ = self.alive.cut.set(err.clone());
		}
		guard.abort = Some(err);
		self.alive.aborted.store(true, Ordering::Release);
		guard.release(salvage);
		guard.close();
		self.cache.aborted(self.sequence, cut);
	}

	/// Whether the group has been aborted (including pool eviction). The track's
	/// read paths treat an aborted cached group as absent.
	///
	/// Reads the mirror rather than the group's state, so a track scan holding the
	/// track lock never takes the group's. Monotone, and only ever conservative: a
	/// concurrent abort can still read as live for the length of [`Self::abort`],
	/// which hands out a group whose consumer then surfaces the abort.
	// private: test
	pub(crate) fn is_aborted(&self) -> bool {
		self.alive.aborted.load(Ordering::Acquire)
	}

	/// Why the publisher gave this group up, once it did, rather than cache policy
	/// skipping it.
	///
	/// Reads the mirror, like [`Self::is_aborted`], so a track scan never takes the group lock.
	pub(super) fn cut_error(&self) -> Option<&Error> {
		self.alive.cut.get()
	}

	/// Whether the group was finished: it holds every frame it will ever have.
	// private: test
	pub(crate) fn is_finished(&self) -> bool {
		self.state.read().fin.is_some()
	}

	/// The index of the first frame this group still holds, or `None` once it has been
	/// aborted. Non-zero when the group started later (see [`Self::start_at`]); a reader
	/// positioned below it is [`Error::Lagged`].
	///
	/// One guard for both halves, deliberately. The track asks this to decide whether a
	/// cached slot can still answer a request, and reading the abort and the offset
	/// separately lets the abort land between them: the slot reads live, then hands
	/// back an offset it only has because it is dead. The mirror
	/// ([`Self::is_aborted`]) is for scans that ask about the abort alone.
	pub(super) fn live_first_frame(&self) -> Option<usize> {
		let state = self.state.read();
		state.abort.is_none().then_some(state.offset)
	}

	/// Whether this copy still holds every committed frame below `end`, so a compressed
	/// tail requested there can inflate against it. Sparse object IDs are not ordinals.
	pub(super) fn holds_prefix(&self, end: u64) -> bool {
		let state = self.state.read();
		state.abort.is_none() && state.offset == 0 && !state.sparse() && state.committed as u64 >= end
	}

	/// One past the last frame committed to an unfinished group, when that is past its
	/// first: where a replacement route resumes. `None` once the group is finished, or
	/// while it holds nothing a replacement could continue.
	///
	/// The *committed* count, not the written one: a route dying midway through a
	/// chunked frame leaves that frame unusable, so the replacement has to send it
	/// again rather than start after it. Answered under one guard so the count can't be
	/// weighed against an offset from a different moment.
	///
	/// An aborted group still answers: readers that already consumed its head want the
	/// tail, and the count outlives the released cache.
	pub(super) fn resume_frame(&self) -> Option<usize> {
		let state = self.state.read();
		if state.fin.is_some() || state.sparse() {
			return None;
		}
		(state.committed > state.offset).then_some(state.committed)
	}

	/// Whether `other` is a handle to this same group.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(super) fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}

	/// Whether the group opened its first frame or finished, so the track counts it as
	/// content. An aborted group is not content either way.
	///
	/// Reads the mirror rather than the group's state, so a track scan holding the track
	/// lock never takes the group's.
	pub(super) fn is_started(&self) -> bool {
		self.alive.started.load(Ordering::Relaxed)
	}

	/// The first frame opened or the group finished: tell the track once, so it re-judges
	/// the reads that skipped this group while it was frameless. Called with the group
	/// lock released, since the track's is taken first.
	///
	/// `track` is the track a first frame already reached, so stamping a group upgrades
	/// its link once.
	fn start(&self, track: Option<kio::Producer<track::TrackState>>) {
		if self.alive.started.swap(true, Ordering::Relaxed) {
			return;
		}
		let Some(track) = track.or_else(|| self.cache.track()) else {
			return;
		};
		let answers = track.read().reveal(self);
		if answers && let Ok(mut state) = track.write() {
			state.shown(self.sequence);
		}
	}

	/// Whether this copy may yet present the group's frame 0: it holds it, and nothing
	/// opened or ended it.
	pub(super) fn may_start(&self) -> bool {
		let state = self.state.read();
		state.offset == 0 && !state.started()
	}

	/// Where the group starts in presentation time: its first frame's timestamp, or
	/// `None` while no frame has been opened or when that frame is untimed.
	///
	/// Stamped once, when the group's first frame arrives, so it measures the group's
	/// place in the media timeline rather than when it happened to be delivered. That
	/// is what lets the track tell a burst of old content apart from live content (see
	/// [`track::Subscriber`]).
	///
	/// `None` for a group accepted unverified below the track's eviction watermark too:
	/// nothing proves where it starts, so it drives no media-time expiry.
	pub(super) fn timestamp(&self) -> Option<Timestamp> {
		let state = self.state.read();
		match state.timeline {
			Timeline::Timed { start, .. } if state.origin != Origin::Unverified => Some(start),
			_ => None,
		}
	}

	/// The start this group lends its neighbors' rewind check: its first frame's timestamp,
	/// once frame 0 was checked to fit between theirs.
	pub(super) fn bound(&self) -> Option<Timestamp> {
		let state = self.state.read();
		match state.timeline {
			Timeline::Timed { start, .. } if state.origin == Origin::Bound => Some(start),
			_ => None,
		}
	}

	/// Where the group ends in presentation time: its newest timed frame's timestamp, or
	/// `None` while no frame has been opened or when the group is untimed.
	///
	/// This is what a drift budget measures an untouched group against. A group is not
	/// late because it *started* long ago: a two-second group whose tail is level with
	/// the live edge still has content nobody has read. Only once its newest frame has
	/// fallen behind is there nothing left worth delivering. Grows as the group does, so
	/// a group still receiving frames stays fresh and a stalled one ages in place.
	pub(super) fn latest(&self) -> Option<Timestamp> {
		let state = self.state.read();
		match state.timeline {
			Timeline::Timed { latest, .. } if state.origin != Origin::Unverified => Some(latest),
			_ => None,
		}
	}

	/// The group's full cached footprint (payload plus fixed overhead, and a compressed
	/// group's inflated copy as far as it is built), used by the track to size this group
	/// as an eviction victim.
	pub(super) fn cache_size(&self) -> u64 {
		let state = self.state.read();
		state.charge.size() + state.flate.as_ref().map_or(0, |flate| flate.plain.bill.size())
	}

	/// Whether the payload cached after the first frame exceeds `ratio` times the first
	/// frame: what [`track::Info::refresh`] judges a join by. A group holding no first
	/// frame (one started past it, or not written yet) has nothing to compare against.
	pub(super) fn over_refresh(&self, ratio: u32) -> bool {
		let state = self.state.read();
		if state.offset != 0 {
			return false;
		}
		let Some(first) = state.frames.front() else {
			return false;
		};
		let first = first.payload.len() as u64;
		state.cache.saturating_sub(first) > first.saturating_mul(ratio.into())
	}

	/// Tick of the group's last cache access, driving eviction protection and age
	/// expiry (see [`cache::Pool::average`]).
	pub(super) fn cache_accessed(&self) -> u64 {
		self.alive.access.get()
	}

	/// Coarse clock tick of the group's last cache access, used by age expiry.
	pub(super) fn cache_accessed_tick(&self, now: Option<u64>) -> Option<u64> {
		self.alive.access.tick(now)
	}

	/// Enter the group into the evictable population: demoted from the live edge,
	/// or inserted behind it. Idempotent; a no-op once the group is closed.
	pub(super) fn cache_demote(&self) {
		if let Ok(mut state) = self.state.write() {
			state.charge.demote();
		}
	}

	/// Record a cache access (delivery to a subscriber, a FETCH hit, or a fetched
	/// backfill's birth), protecting the group from eviction and restarting its
	/// expiry clock. Stamps through a read guard, whose release never notifies, so
	/// delivery can't wake every consumer parked on the group. Harmless on a
	/// closed group: its charge is already cleared.
	pub(super) fn cache_refresh(&self) {
		self.state.read().charge.refresh();
	}

	/// Create a new consumer for the group.
	// private: capability gap
	// Sessions observe a group while its sole writer belongs to a frame.
	pub(crate) fn consume(&self) -> Consumer {
		self.consumer(self.state.consume())
	}

	/// Create a consumer, or `None` once the group is aborted. Paired with
	/// [`abort_unused`](Self::abort_unused): the closed check and the count share its
	/// lock, so a consumer either exists in time to decline the abort or is never made.
	pub(super) fn try_consume(&self) -> Option<Consumer> {
		self.state.weak().try_consume().map(|state| self.consumer(state))
	}

	fn consumer(&self, state: kio::Consumer<GroupState>) -> Consumer {
		Consumer {
			info: self.info,
			track: self.track.clone(),
			cursor: Cursor {
				state,
				index: 0,
				end: None,
				prefetch: Prefetch::default(),
				raw: false,
				cache: self.cache.clone(),
				access: self.alive.access.clone(),
				refreshed: self.cache.pool().now(),
			},
			// Untagged: a tagged track attaches the egress meter via `with_meter`
			// when it hands the consumer to a subscriber/fetch.
			stats: stats::Meter::default(),
			expiry: None,
			expired: false,
			adopted: false,
			stale_counted: Arc::default(),
			recover: None,
		}
	}

	/// Register for the group's first frame, which settles its [`Self::timestamp`], while
	/// none has been opened.
	pub(super) fn poll_started(&self, waiter: &kio::Waiter) -> Poll<()> {
		match self.state.poll(waiter, |state| {
			if state.started() {
				Poll::Ready(())
			} else {
				Poll::Pending
			}
		}) {
			Poll::Ready(_) => Poll::Ready(()),
			Poll::Pending => Poll::Pending,
		}
	}

	/// Poll until the group is aborted, including by eviction; ready with the cause.
	/// Finishing does not close it.
	// private: capability gap
	// Sessions observe a group while its sole writer belongs to a frame.
	pub(crate) fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Error> {
		self.state.poll_closed(waiter).map(|()| self.abort_reason())
	}

	/// Watch reader demand without keeping the group alive.
	// private: capability gap
	// Sessions observe a group while its sole writer belongs to a frame.
	pub(crate) fn demand(&self) -> Demand {
		Demand {
			sequence: self.info.sequence,
			state: DemandSource::Group(self.state.weak()),
		}
	}

	/// Poll for the group becoming unused (every consumer dropped).
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	// pending: /quest/m1/public-model/answer.md
	pub(crate) fn poll_unused(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.state.poll_unused(waiter).map(|_| ())
	}

	/// The recorded abort reason, or [`Error::Dropped`] if the group closed without one.
	fn abort_reason(&self) -> Error {
		self.state.read().abort.clone().unwrap_or(Error::Dropped)
	}
}

/// A cloneable, watch-only handle to a group's readers or pending fetch callers.
///
/// An accepted or dropped request ends waits with [`Error::Dropped`]; a rejected request keeps
/// its rejection reason.
#[derive(Clone)]
pub struct Demand {
	sequence: u64,
	state: DemandSource,
}

#[derive(Clone)]
enum DemandSource {
	Group(kio::ProducerWeak<GroupState>),
	Fetch(kio::ProducerWeak<track::FetchOutcome>),
}

impl Demand {
	pub(super) fn fetch(sequence: u64, state: kio::ProducerWeak<track::FetchOutcome>) -> Self {
		Self {
			sequence,
			state: DemandSource::Fetch(state),
		}
	}

	/// The group sequence this handle watches.
	pub fn sequence(&self) -> u64 {
		self.sequence
	}

	/// Whether the group or fetch is open and has readers right now.
	pub fn is_used(&self) -> bool {
		match &self.state {
			DemandSource::Group(state) => !state.is_closed() && state.is_used(),
			DemandSource::Fetch(state) => !state.is_closed() && state.is_used(),
		}
	}

	/// Wait until at least one reader needs the group.
	#[cfg(feature = "async")]
	pub async fn used(&self) -> Result<()> {
		kio::wait(|waiter| self.poll_used(waiter)).await
	}

	/// Wait until no reader needs the group.
	#[cfg(feature = "async")]
	pub async fn unused(&self) -> Result<()> {
		kio::wait(|waiter| self.poll_unused(waiter)).await
	}

	/// Poll until at least one reader needs the group.
	pub fn poll_used(&self, waiter: &kio::Waiter) -> Poll<Result<()>> {
		match &self.state {
			DemandSource::Group(state) => state.poll_used(waiter),
			DemandSource::Fetch(state) => state.poll_used(waiter),
		}
		.map_err(|_| self.abort_reason())
	}

	/// Poll until no reader needs the group.
	pub fn poll_unused(&self, waiter: &kio::Waiter) -> Poll<Result<()>> {
		match &self.state {
			DemandSource::Group(state) => state.poll_unused(waiter),
			DemandSource::Fetch(state) => state.poll_unused(waiter),
		}
		.map_err(|_| self.abort_reason())
	}

	/// Wait until the group or fetch closes, returning its cause.
	#[cfg(feature = "async")]
	pub async fn closed(&self) -> Error {
		match &self.state {
			DemandSource::Group(state) => state.closed().await,
			DemandSource::Fetch(state) => state.closed().await,
		}
		self.abort_reason()
	}

	fn abort_reason(&self) -> Error {
		match &self.state {
			DemandSource::Group(state) => state.read().abort.clone(),
			DemandSource::Fetch(state) => state.read().rejected.clone(),
		}
		.unwrap_or(Error::Dropped)
	}
}

/// A small batch of completed frames, drained from the shared group state under one
/// lock and then handed out without re-locking.
///
/// Each [`Consumer::read_frame`] otherwise takes the group mutex and allocates a
/// waker just to clone one `Bytes`; draining a batch amortizes both across `CAP`
/// frames. Storage is uninitialized and allocated by the first non-empty fill, so a
/// consumer that never reads whole frames, or drains through a higher-level buffer,
/// pays nothing. Inline storage would triple the size of every [`Consumer`], which
/// moves by value through each layer that hands out a group.
#[derive(Default)]
struct Prefetch {
	// Initialized, not-yet-taken frames are `frames[pos..len]`; the rest are uninitialized.
	frames: Option<Box<[MaybeUninit<Frame>; Self::CAP]>>,
	// Relay metadata for `frames`, allocated by the first batch that carries any.
	fields: Option<Box<[Option<Arc<super::object::Fields>>; Self::CAP]>>,
	// Inflated sizes for `frames`, filled only by a batch of a compressed group's stored slices.
	inflated: Option<Box<[u64; Self::CAP]>>,
	pos: usize,
	len: usize,
}

impl Prefetch {
	const CAP: usize = 8;

	/// The inflated size of the stored slice [`Self::pop`] just returned.
	fn take_inflated(&self) -> Option<u64> {
		self.inflated.as_ref()?.get(self.pos.checked_sub(1)?).copied()
	}

	/// Take the next buffered frame, or `None` if the batch is drained.
	fn pop(&mut self) -> Option<Frame> {
		if self.pos == self.len {
			return None;
		}
		let frames = self.frames.as_mut()?;
		// SAFETY: `pos < len`, so this slot was written by `fill` and not yet taken.
		let frame = unsafe { frames[self.pos].assume_init_read() };
		self.pos += 1;
		Some(frame)
	}

	/// The relay metadata of the frame [`Self::pop`] just returned.
	fn take_fields(&mut self) -> Option<Arc<super::object::Fields>> {
		self.fields.as_mut()?.get_mut(self.pos.checked_sub(1)?)?.take()
	}

	/// Refill with up to `budget` of the completed frames from `local` on. Must be
	/// drained first (`pop` returned `None`).
	///
	/// A compressed group fills its stored slices, with `raw` their inflated sizes beside
	/// them, and otherwise returns the job [`Self::inflate`] runs once the guard is released.
	fn fill(&mut self, state: &GroupState, local: usize, budget: usize, raw: bool) -> Result<Option<Inflate>> {
		debug_assert_eq!(self.pos, self.len, "fill on a non-empty batch would leak frames");
		self.pos = 0;
		self.len = 0;
		let count = state.frames.len().saturating_sub(local).min(budget).min(Self::CAP);
		// Taken before any slot is written, so a refusal leaves the batch empty.
		let job = match &state.flate {
			Some(flate) if !raw && count > 0 => Some(state.inflate(flate, local + count - 1)?),
			_ => None,
		};
		if count > 0 {
			let frames = self
				.frames
				.get_or_insert_with(|| Box::new([const { MaybeUninit::uninit() }; Self::CAP]));
			for frame in state.frames.range(local..local + count) {
				frames[self.len].write(frame.clone());
				self.len += 1;
			}
		}
		match &state.flate {
			Some(flate) if raw => {
				let inflated = self.inflated.get_or_insert_default();
				for (slot, size) in inflated.iter_mut().zip(flate.sizes.range(local..local + self.len)) {
					*slot = *size;
				}
			}
			_ => self.inflated = None,
		}
		// Plain frames never reach the metadata table, so they skip this entirely.
		if local < state.objects() {
			let fields = self.fields.get_or_insert_default();
			for (i, slot) in fields.iter_mut().enumerate() {
				*slot = if i < self.len { state.fields(local + i) } else { None };
			}
		} else if let Some(fields) = &mut self.fields {
			fields.fill(None);
		}
		Ok(job)
	}

	/// Replace a filled batch's stored slices, from local frame `local`, with their inflated
	/// frames. A frame that fails to inflate ends the batch, failing it if first.
	fn inflate(&mut self, job: &Inflate, local: usize, meter: &stats::Meter) -> Result<()> {
		job.run(meter);
		let Some(frames) = &mut self.frames else {
			return Ok(());
		};
		for i in self.pos..self.len {
			match job.get(local + i) {
				// SAFETY: slots in `pos..len` are initialized (written by `fill`, not yet popped).
				Ok(payload) => unsafe { frames[i].assume_init_mut() }.payload = payload,
				Err(err) => {
					for slot in &mut frames[i..self.len] {
						// SAFETY: as above, and `len` drops below them right after.
						unsafe { slot.assume_init_drop() };
					}
					self.len = i;
					return if i == self.pos { Err(err) } else { Ok(()) };
				}
			}
		}
		Ok(())
	}

	/// `(frame count, total payload bytes)` of the buffered, not-yet-taken frames.
	/// Read once per fill to bump the egress payload counters for the whole batch.
	fn buffered(&self) -> (u64, u64) {
		let mut bytes = 0u64;
		let Some(frames) = &self.frames else {
			return (0, 0);
		};
		for slot in &frames[self.pos..self.len] {
			// SAFETY: slots in `pos..len` are initialized (written by `fill`, not yet popped).
			bytes += unsafe { slot.assume_init_ref() }.payload.len() as u64;
		}
		((self.len - self.pos) as u64, bytes)
	}
}

impl Drop for Prefetch {
	fn drop(&mut self) {
		let Some(frames) = &mut self.frames else {
			return;
		};
		for slot in &mut frames[self.pos..self.len] {
			// SAFETY: slots in `pos..len` are initialized and were never taken.
			unsafe { slot.assume_init_drop() };
		}
	}
}

/// Consume a group, frame-by-frame.
pub struct Consumer {
	cursor: Cursor,

	// Immutable stream state.
	info: Info,

	// The parent track's info, inherited from the producer. Its `timescale` lets the
	// wire publisher emit per-frame timestamps at the right scale for a fetched group.
	track: track::Info,

	// Egress payload meter, set by a tagged track via [`Self::with_meter`]. Empty
	// (no-op) for an untagged group. Also owns unread content discarded by expiry.
	stats: stats::Meter,

	// Subscriber-specific drift policy. A group can become stale after the track
	// hands it out, while its reader is waiting for the first or next frame.
	expiry: Option<Arc<dyn Expiry>>,
	expired: bool,
	// A replacement copy was adopted and has not yet returned a frame that matches. Its
	// ordinals match this group's only while its Object IDs do.
	adopted: bool,
	// Cloned cursors are parallel views of one handed-out delivery. Whichever
	// observes expiry first records its unread tail; the others must not repeat it.
	stale_counted: Arc<AtomicBool>,
	// Handed out from a front's logical track: carries the read across route changes.
	// Boxed: it is the rare case.
	recover: Option<Box<super::resume::Recover>>,
}

/// Subscriber-specific policy for expiring a group after it was handed out.
///
/// Unwind safe so the [`Consumer`] holding it is, and so the published
/// [`track::Fetching`] that holds a consumer stays unwind safe too.
pub(super) trait Expiry: Send + Sync + std::panic::UnwindSafe + std::panic::RefUnwindSafe {
	/// Return whether the group is stale, registering `waiter` for anything that
	/// could change the answer while the group remains live.
	/// A logical reader supplies its current budget after the original copy is gone.
	fn is_expired(&self, max_delay: Option<std::time::Duration>, waiter: &kio::Waiter) -> bool;

	/// Keep the reader's budget and cap while following a replacement track's edge.
	fn for_track(&self, track: &track::Consumer) -> Arc<dyn Expiry>;
}

/// The read cursor over a [`Producer`]'s shared state.
struct Cursor {
	// Shared state with the producer.
	state: kio::Consumer<GroupState>,

	// The index of the next frame to read.
	// NOTE: Cloned readers inherit this offset, but then run in parallel.
	index: usize,

	// Exclusive cap on `index`, set by [`Consumer::set_frames`]. Reads end cleanly at it.
	end: Option<usize>,

	// A batch of completed frames drained ahead under one lock (whole-frame reads only).
	prefetch: Prefetch,

	// Read a compressed group's stored slices rather than its inflated copy.
	raw: bool,

	// Record prefetched reads without entering the group's state on every frame.
	cache: Arc<cache::Track>,
	access: Arc<cache::Access>,
	refreshed: u64,
}

impl Clone for Cursor {
	fn clone(&self) -> Self {
		// A clone shares the channel and inherits `index`, but starts with an empty
		// prefetch: it re-reads its batch from the shared state, in parallel.
		Self {
			state: self.state.clone(),
			index: self.index,
			end: self.end,
			prefetch: Prefetch::default(),
			raw: self.raw,
			cache: self.cache.clone(),
			access: self.access.clone(),
			refreshed: self.refreshed,
		}
	}
}

impl Clone for Consumer {
	fn clone(&self) -> Self {
		Self {
			cursor: self.cursor.clone(),
			info: self.info,
			track: self.track.clone(),
			// Inherit the meter without re-counting the group: the original already
			// counted it when the track handed it out.
			stats: self.stats.clone(),
			expiry: self.expiry.clone(),
			expired: self.expired,
			adopted: self.adopted,
			stale_counted: self.stale_counted.clone(),
			recover: self.recover.clone(),
		}
	}
}

impl std::ops::Deref for Consumer {
	type Target = Info;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl Consumer {
	#[cfg(feature = "async")]
	// private: passthrough
	pub(crate) fn last_object(&self) -> Option<u64> {
		let state = self.cursor.state.read();
		match state.wire() {
			Some(wire) => wire.last,
			None => (state.next_index as u64).checked_sub(1),
		}
	}

	#[cfg(feature = "async")]
	// private: passthrough
	pub(crate) fn wire(&self) -> Option<Wire> {
		self.cursor.state.read().wire().cloned()
	}

	/// Whether the group was relayed from moq-transport, whose Object IDs may not be its frame
	/// ordinals.
	pub(super) fn relayed(&self) -> bool {
		self.cursor.state.read().wire().is_some()
	}

	/// Read a compressed group's stored DEFLATE slices instead of plain frames, with each
	/// frame's inflated size beside it ([`frame::Object::inflated`],
	/// [`frame::Consumer::inflated`]), for egress that carries compression. A no-op on a
	/// plain group.
	// private: passthrough
	pub(crate) fn compressed(mut self) -> Self {
		self.cursor.raw = true;
		self.cursor.prefetch = Prefetch::default();
		self
	}

	/// The index of the first frame the group holds (see [`Producer::start_at`]).
	// private: passthrough
	pub(crate) fn first_frame(&self) -> u64 {
		self.cursor.state.read().offset as u64
	}

	#[cfg(feature = "async")]
	// private: passthrough
	pub(crate) fn end_object(&self) -> Option<Arc<super::object::Fields>> {
		self.cursor.state.read().relay.as_ref()?.end_object.clone()
	}

	/// Fill a private batch without losing metadata through the public frame view.
	#[cfg(feature = "async")]
	// private: passthrough
	pub(crate) fn poll_read_objects<const N: usize>(
		&mut self,
		waiter: &kio::Waiter,
		out: &mut arrayvec::ArrayVec<frame::Object, N>,
	) -> Poll<Result<usize>> {
		out.clear();
		while !out.is_full() {
			match self.poll_read_object(waiter) {
				Poll::Ready(Ok(Some(frame))) => out.push(frame),
				Poll::Ready(Ok(None)) => break,
				Poll::Ready(Err(err)) if out.is_empty() => return Poll::Ready(Err(err)),
				Poll::Ready(Err(_)) => break,
				Poll::Pending if out.is_empty() => return Poll::Pending,
				Poll::Pending => break,
			}
		}
		Poll::Ready(Ok(out.len()))
	}

	/// Snapshot the content this cursor would discard if its group were skipped.
	pub(super) fn content(&self) -> stats::Content {
		self.cursor.state.read().content()
	}

	/// Attach an egress payload meter, counting this as one delivered group.
	/// Called by a tagged track when it hands the consumer to a subscriber or fetch.
	pub(super) fn with_meter(mut self, meter: stats::Meter) -> Self {
		meter.group();
		self.stats = meter;
		self
	}

	/// Keep applying this subscription's drift budget while the group is read.
	pub(super) fn with_expiry(mut self, expiry: Arc<dyn Expiry>) -> Self {
		self.expiry = Some(expiry);
		self
	}

	/// The hand-off of this copy's head, shared by every reader that could resume it
	/// elsewhere.
	pub(super) fn handoff(&self) -> Arc<Handoff> {
		let state = self.cursor.state.read();
		let mut handoff = state.handoff.lock().unwrap_or_else(PoisonError::into_inner);
		if let Some(handoff) = handoff.upgrade() {
			return handoff;
		}
		let shared = Arc::new(Handoff {
			state: self.cursor.state.weak(),
			salvaged: Mutex::default(),
		});
		*handoff = Arc::downgrade(&shared);
		shared
	}

	/// Carry this group across a front's route changes; see [`super::resume`].
	pub(super) fn with_recover(mut self, recover: super::resume::Recover) -> Self {
		self.recover = Some(Box::new(recover));
		self
	}

	/// Run `read`, and once this copy fails with its route, or stalls while a newer route
	/// serves, continue from the serving route's copy at the same frame and read again.
	fn poll_resumed<T>(
		&mut self,
		waiter: &kio::Waiter,
		mut read: impl FnMut(&mut Self, &kio::Waiter) -> Poll<Result<Option<T>>>,
	) -> Poll<Result<Option<T>>> {
		loop {
			let res = read(self, waiter);
			// The reader's own budget gave up on it: that is no route's doing.
			if self.expired {
				return res;
			}
			let Some(recover) = self.recover.as_mut() else {
				return res;
			};
			let failed = match &res {
				Poll::Ready(Ok(Some(_))) => {
					// The old copy was dense through this frame, so the replacement must be too.
					// The refusal stays: a batch that already holds frames swallows one error.
					if self.adopted {
						if self.cursor.state.read().sparse() {
							// `read_frame` pops prefetched frames without coming back here.
							self.cursor.prefetch = Prefetch::default();
							return Poll::Ready(Err(Error::Unsupported));
						}
						self.adopted = false;
					}
					return res;
				}
				// Read to its end: no route is needed for it any more.
				Poll::Ready(Ok(None)) => {
					self.recover = None;
					return res;
				}
				// Content that failed to inflate is the group's under its name, so no copy fixes it.
				Poll::Ready(Err(_)) if !self.cursor.raw && self.cursor.state.read().malformed() => return res,
				Poll::Ready(Err(err)) => Some(err.clone()),
				Poll::Pending => None,
			};
			if !recover.wants(failed.as_ref(), waiter) {
				return res;
			}
			// An ordinal FETCH would request different content after an Object ID gap.
			if self.cursor.state.read().sparse() {
				return Poll::Ready(Err(Error::Unsupported));
			}
			let replacement = match recover.poll(self.cursor.index as u64, failed.as_ref(), waiter) {
				Poll::Ready(Ok(group)) => group,
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
				Poll::Pending => return Poll::Pending,
			};
			// A plain read inflates from frame 0, which a replacement starting later lacks
			// when the failed route's head was not lent to it.
			if !self.cursor.raw
				&& replacement.group.first_frame() > 0
				&& replacement.group.cursor.state.read().flate.is_some()
			{
				return Poll::Ready(Err(failed.unwrap_or(Error::Lagged)));
			}
			// Same frames by index, so only the channel changes; read progress and the
			// cap stay this cursor's.
			let (index, end, raw) = (self.cursor.index, self.cursor.end, self.cursor.raw);
			recover.adopt(&replacement);
			self.expiry = self.expiry.as_ref().map(|expiry| expiry.for_track(&replacement.copy));
			self.cursor = replacement.group.cursor;
			self.cursor.index = index;
			self.cursor.end = end;
			self.cursor.raw = raw;
			self.adopted = true;
		}
	}

	/// Check the parent subscription while a wire publisher drains detached payload.
	// pending: /quest/m1/public-model/serve.md
	pub(crate) fn poll_expired(&mut self, waiter: &kio::Waiter) -> bool {
		self.poll_expired_while_pending(waiter, false)
	}

	/// Apply the drift budget to a read that found nothing and is about to park, failing
	/// it with [`Error::Old`] once the budget gives up.
	///
	/// A group with frames in hand is always drained to its end: the budget bounds a
	/// group that has *stalled* while the live edge moved on, not one whose reader is
	/// merely slower than the wire. Judging every read instead would truncate the tail
	/// of every group, since the arrival of the next group is exactly what makes the
	/// current one no longer newest.
	///
	/// A reader that took every frame still fails: the producer never finished the
	/// group, so a clean end would pass the prefix off as the whole group to anyone it
	/// is served to.
	///
	/// Keeping it off the ready path also keeps it off the hot path: evaluating the
	/// policy walks the track's group cache under its lock, which is shared by every
	/// subscriber of that track.
	fn poll_expired_if_blocked<T>(&mut self, waiter: &kio::Waiter, res: Poll<Result<T>>) -> Poll<Result<T>> {
		match res.is_pending() && self.poll_expired(waiter) {
			true => Poll::Ready(Err(Error::Old)),
			false => res,
		}
	}

	/// Keep checking expiry while a wire publisher still owns buffered group data.
	// pending: /quest/m1/public-model/serve.md
	pub(crate) fn poll_expired_while_pending(&mut self, waiter: &kio::Waiter, pending: bool) -> bool {
		if !self.expired
			&& (pending || self.expiry_pending())
			&& self.expiry.as_ref().is_some_and(|expiry| {
				let budget = self.recover.as_ref().and_then(|recover| recover.poll_budget(waiter));
				expiry.is_expired(budget, waiter)
			}) {
			self.expired = true;
			if !self.stale_counted.swap(true, Ordering::Relaxed) {
				self.stats.stale(self.cursor.unread_content());
			}
		}
		self.expired
	}

	/// Why a wire publisher waiting on the transport, for stream credit or to drain, should
	/// give up on this group: the drift budget convicted it ([`Error::Old`]), or its copy
	/// was aborted (evicted, cut, or failed upstream) before this cursor read it all and no
	/// route will continue it. `pending` as in [`Self::poll_expired_while_pending`].
	///
	/// A machine parked on stream credit reads nothing, so without this an aborted group,
	/// which never expires, would wait for credit it could only spend on a reset.
	// pending: /quest/m1/public-model/serve.md
	#[cfg(feature = "async")]
	pub(crate) fn poll_abandoned(&mut self, waiter: &kio::Waiter, pending: bool) -> Option<Error> {
		if self.poll_expired_while_pending(waiter, pending) {
			return Some(Error::Old);
		}
		// Registered on the close alone: an abort closes the channel, and waking on every
		// frame written to a parked group would cost a stalled subscriber per frame.
		if self.cursor.state.poll_closed(waiter).is_pending() {
			return None;
		}
		let aborted = {
			let state = self.cursor.state.read();
			let unread = !self.cursor.capped() && state.fin.is_none_or(|fin| self.cursor.index < fin);
			match &state.abort {
				// The rest of the group is gone; what was read already is not.
				Some(err) if unread => err.clone(),
				// Read to its end, or finished and closed with its last producer: still servable.
				_ => return None,
			}
		};
		// A front's reader may continue the group from another route's copy when it reads.
		if self
			.recover
			.as_mut()
			.is_some_and(|recover| recover.wants(Some(&aborted), waiter))
		{
			return None;
		}
		Some(aborted)
	}

	/// Whether expiry can still discard content or unblock a group that may grow.
	fn expiry_pending(&self) -> bool {
		self.cursor.expiry_pending()
	}

	/// Mark the group as still being read, so a slow batch drain does not expire it.
	pub fn keep_alive(&self) {
		self.cursor.state.read().charge.refresh();
	}

	/// Record a cache access from the consumer side: a parked group re-offered to
	/// its subscriber. Same stamp as [`Producer::cache_refresh`].
	pub(super) fn cache_refresh(&self) {
		self.keep_alive();
	}

	/// The parent track's timescale, or `None` when it declared no timeline.
	pub fn timescale(&self) -> Option<Timescale> {
		self.track.timescale
	}

	/// The timescale a moq-lite hop serves the parent track at (see
	/// [`track::Info::lite_timescale`]).
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	// private: passthrough
	pub(crate) fn lite_timescale(&self, ietf: bool) -> Option<Timescale> {
		self.track.lite_timescale(ietf)
	}

	/// The draft the parent track's moq-transport publisher spoke, which serialized every
	/// object's properties, or `None` for a moq-lite one.
	#[cfg(feature = "async")]
	// private: passthrough
	pub(crate) fn ietf(&self) -> Option<crate::ietf::Version> {
		self.track.ietf()
	}

	/// The index of the next frame this consumer will return.
	///
	/// Starts at 0, or at the group's first available frame once [`Self::set_frames`] has
	/// clamped it, and advances by one per frame read.
	pub fn index(&self) -> u64 {
		self.cursor.index as u64
	}

	/// Limit subsequent reads to these frame indices without rewinding read progress.
	///
	/// `2..=5` includes frames 2 through 5; `2..5` excludes frame 5. An omitted
	/// start preserves read progress, and an omitted end removes the cap.
	/// Raising the cap makes unread cached frames available again.
	pub fn set_frames(&mut self, frames: impl RangeBounds<u64>) {
		let (start, end) = super::subscription::sequence_bounds(frames);
		self.start_at(start);
		self.end_at(end.map_or(Bound::Unbounded, Bound::Excluded));
	}

	/// Skip ahead so the next frame returned is `index`, discarding anything buffered
	/// below it.
	///
	/// Clamped *up* to the group's first available frame: frames the group never held
	/// (see [`Producer::start_at`]) can't be returned, so asking for one just starts at
	/// the first that exists. Read [`Self::index`] back to learn where the cursor
	/// actually landed.
	/// Only moves forward; a lower `index` is ignored, since the frames behind the
	/// cursor may already have been handed out.
	// pending: /quest/m1/public-model/serve.md
	pub(crate) fn start_at(&mut self, index: u64) {
		self.cursor.start_at(index);
	}

	/// Advance the read cursor to `index`, skipping every frame below it.
	///
	/// Unlike [`Self::set_frames`], this does not clamp past a requested frame the group
	/// never held. A [`Producer::start_at`] floor above `index` still surfaces as
	/// [`Error::Lagged`].
	pub fn skip_to(&mut self, index: u64) {
		self.cursor.skip_to(index);
	}

	/// Stop reading at `end`, or remove the cap with `..`.
	///
	/// `..=2` reads through frame 2, `..2` stops before it, and `..0` is the empty range:
	/// no frame is delivered. Reads past the cap end cleanly (`None`), as if the group
	/// finished there. The cap can move in either direction: raising it re-offers
	/// frames that are still cached.
	// pending: /quest/m1/public-model/serve.md
	pub(crate) fn end_at(&mut self, end: impl Into<Cap>) {
		let end = end.into().exclusive();
		self.cursor.end = end.map(|end| usize::try_from(end).unwrap_or(usize::MAX));
	}

	/// The number of frames written so far (completed plus any in-flight), independent of
	/// how many this consumer has read. The final total once the group is finished.
	pub fn frame_count(&self) -> usize {
		let state = self.cursor.state.read();
		state.fin.unwrap_or(state.next_index)
	}

	/// Return a consumer for the next frame for chunked reading.
	#[cfg(feature = "async")]
	pub async fn next_frame(&mut self) -> Result<Option<frame::Consumer>> {
		kio::wait(|waiter| self.poll_next_frame(waiter)).await
	}

	/// Poll for the next frame, without blocking.
	///
	/// Returns None if the group is finished and the index is out of range, or the cursor
	/// passed the [`Self::set_frames`] cap.
	pub fn poll_next_frame(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<frame::Consumer>>> {
		if self.recover.is_none() {
			return self.poll_next_frame_once(waiter);
		}
		let res = self.poll_resumed(waiter, Self::poll_next_frame_once);
		match (res, &self.recover) {
			// A frame read from a copy that may fail too carries on the same way.
			(Poll::Ready(Ok(Some(frame))), Some(recover)) => Poll::Ready(Ok(Some(
				frame.with_recover((**recover).clone(), self.cursor.index as u64 - 1),
			))),
			(res, _) => res,
		}
	}

	fn poll_next_frame_once(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<frame::Consumer>>> {
		if self.expired {
			return Poll::Ready(Err(Error::Old));
		}
		let stats = self.stats.clone();
		let expiry = self
			.expiry
			.as_ref()
			.map(|policy| frame::Expiry::new(policy.clone(), stats.clone(), self.stale_counted.clone()));
		let res = self.cursor.poll_next_frame(waiter, &stats, expiry);
		self.poll_expired_if_blocked(waiter, res)
	}

	/// Read the next frame (timestamp and payload) all at once, without blocking.
	pub fn poll_read_frame(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<frame::Frame>>> {
		if self.recover.is_none() {
			return self.poll_read_frame_once(waiter);
		}
		self.poll_resumed(waiter, Self::poll_read_frame_once)
	}

	/// Read the next frame with its relay metadata, for wire egress.
	#[cfg(feature = "async")]
	// private: passthrough
	pub(crate) fn poll_read_object(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<frame::Object>>> {
		// Every frame is served through the cursor's prefetch, which holds its fields.
		let frame = ready!(self.poll_read_frame(waiter))?;
		Poll::Ready(Ok(frame.map(|frame| frame::Object {
			frame,
			fields: self.cursor.prefetch.take_fields(),
			inflated: self.cursor.prefetch.take_inflated(),
		})))
	}

	fn poll_read_frame_once(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<frame::Frame>>> {
		if self.expired {
			return Poll::Ready(Err(Error::Old));
		}
		let res = self.cursor.poll_read_frame(waiter, &self.stats);
		self.poll_expired_if_blocked(waiter, res)
	}

	/// Read the next frame (timestamp and payload) all at once.
	#[cfg(feature = "async")]
	pub async fn read_frame(&mut self) -> Result<Option<frame::Frame>> {
		// A prefetched frame is already buffered, so the drift budget (which only judges
		// a read that would park) can never apply to it.
		// Serve from the prefetched batch without building a future or allocating a waker.
		if !self.expired
			&& !self.cursor.capped()
			&& let Some(frame) = self.cursor.prefetch.pop()
		{
			self.cursor.refresh_if_stale();
			self.cursor.index += 1;
			return Ok(Some(frame));
		}
		kio::wait(|waiter| self.poll_read_frame(waiter)).await
	}

	/// Fill `out` with every frame that is ready, up to its capacity, without blocking.
	///
	/// This is a short read: it returns as soon as anything is ready rather than
	/// waiting for `out` to fill. A zero count means the group ended when the buffer
	/// has non-zero capacity.
	pub fn poll_read_frames<const N: usize>(
		&mut self,
		waiter: &kio::Waiter,
		out: &mut frame::Buffer<N>,
	) -> Poll<Result<usize>> {
		out.clear();
		if out.capacity() == 0 {
			return Poll::Ready(Ok(0));
		}

		while !out.is_full() {
			match self.poll_read_frame(waiter) {
				Poll::Ready(Ok(Some(frame))) => out.push(frame).expect("buffer capacity checked"),
				Poll::Ready(Ok(None)) => break,
				Poll::Ready(Err(err)) => {
					if out.is_empty() {
						return Poll::Ready(Err(err));
					}
					break;
				}
				Poll::Pending if !out.is_empty() => break,
				Poll::Pending => return Poll::Pending,
			}
		}

		Poll::Ready(Ok(out.len()))
	}

	/// Fill `out` with every frame that is ready, blocking until a frame arrives or
	/// the group ends. Returns the current batch, empty only at the end of the group.
	#[cfg(feature = "async")]
	pub async fn read_frames<'a, const N: usize>(
		&mut self,
		out: &'a mut frame::Buffer<N>,
	) -> Result<&'a mut [frame::Frame]> {
		kio::wait(|waiter| self.poll_read_frames(waiter, out)).await?;
		Ok(out.filled_mut())
	}

	/// Poll until the group terminates, returning this cursor's next frame index.
	pub fn poll_finished(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64>> {
		if self.recover.is_none() {
			return self.poll_finished_once(waiter);
		}
		let res = self.poll_resumed(waiter, |this, waiter| {
			this.poll_finished_once(waiter).map(|res| res.map(Some))
		});
		res.map(|res| res.map(|index| index.expect("finished with an index")))
	}

	fn poll_finished_once(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64>> {
		if self.expired {
			return Poll::Ready(Err(Error::Old));
		}
		let index = self.cursor.index;
		let res = self
			.cursor
			.poll(waiter, |state| state.poll_end(index))
			.map(|res| res.map(|()| index as u64));
		self.poll_expired_if_blocked(waiter, res)
	}

	/// Block until the group terminates, returning this cursor's next frame index.
	///
	/// This answers for the cursor, not the group: a reader that drained every frame gets the
	/// clean end even if the group was aborted afterwards to release its cache, while one that
	/// stopped short gets that abort. A prior [`Self::skip_to`] contributes to the index even
	/// though those frames were not read. Use [`Self::frame_count`] for the producer's total.
	#[cfg(feature = "async")]
	pub async fn finished(&mut self) -> Result<u64> {
		kio::wait(|waiter| self.poll_finished(waiter)).await
	}
}

impl Cursor {
	/// Whether this cursor still has unread content or may receive another frame.
	fn expiry_pending(&self) -> bool {
		if self.capped() {
			return false;
		}

		let state = self.state.read();
		state.abort.is_none() && state.fin.is_none_or(|fin| self.index < fin)
	}

	/// Content this cursor has neither returned nor already counted in a prefetch batch.
	fn unread_content(&self) -> stats::Content {
		let prefetched = self.prefetch.buffered().0 as usize;
		let start = self.index.saturating_add(prefetched);
		let end = self.end.unwrap_or(usize::MAX);
		self.state.read().content_range(start, end)
	}

	/// Record prefetched reads, updating the eviction rank once per sampled tick.
	fn refresh_if_stale(&mut self) {
		self.access.touch();
		let tick = self.cache.pool().now();
		if tick != self.refreshed {
			self.state.read().charge.refresh();
			self.refreshed = tick;
		}
	}

	// A helper to automatically apply Dropped if the state is closed without an error.
	fn poll<F, R>(&self, waiter: &kio::Waiter, f: F) -> Poll<Result<R>>
	where
		F: Fn(&kio::Ref<'_, GroupState>) -> Poll<Result<R>>,
	{
		Poll::Ready(match ready!(self.state.poll(waiter, f)) {
			Ok(res) => res,
			// We try to clone abort just in case the function forgot to check for terminal state.
			Err(state) => Err(state.abort.clone().unwrap_or(Error::Dropped)),
		})
	}

	/// Whether the cursor has passed the `end_at` cap.
	fn capped(&self) -> bool {
		self.end.is_some_and(|end| self.index >= end)
	}

	fn start_at(&mut self, index: u64) {
		let index = usize::try_from(index).unwrap_or(usize::MAX);
		let index = index.max(self.state.read().offset);
		if index <= self.index {
			return;
		}
		self.index = index;
		// The batch was drained from below the new cursor, so it can't be reused.
		self.prefetch = Prefetch::default();
	}

	fn skip_to(&mut self, index: u64) {
		let index = usize::try_from(index).unwrap_or(usize::MAX);
		if index <= self.index {
			return;
		}
		self.index = index;
		self.prefetch = Prefetch::default();
	}

	fn poll_next_frame(
		&mut self,
		waiter: &kio::Waiter,
		stats: &stats::Meter,
		expiry: Option<frame::Expiry>,
	) -> Poll<Result<Option<frame::Consumer>>> {
		if self.capped() {
			return Poll::Ready(Ok(None));
		}
		let end = self.end.unwrap_or(usize::MAX);

		// Hand out any frames a prior read_frame prefetched before touching the tail.
		// Their bytes were already counted at the batch fill, so the frame::Consumer
		// carries no meter.
		if let Some(frame) = self.prefetch.pop() {
			self.refresh_if_stale();
			self.index += 1;
			let tail = self.index.saturating_add(self.prefetch.buffered().0 as usize)..end;
			let info = frame::Info {
				size: frame.payload.len() as u64,
				timestamp: frame.timestamp,
			};
			let source = frame::Source::Complete(frame.payload);
			let header = frame::Header {
				info,
				fields: self.prefetch.take_fields(),
				inflated: self.prefetch.take_inflated(),
			};
			let frame = frame::Consumer::new(self.state.clone(), header, source);
			return Poll::Ready(Ok(Some(match expiry {
				Some(expiry) => frame.with_expiry(expiry.for_frame(tail, false)),
				None => frame,
			})));
		}

		let (index, raw) = (self.index, self.raw);
		let Some(source) = ready!(self.poll(waiter, |state| state.poll_frame_source(index, raw))?) else {
			return Poll::Ready(Ok(None));
		};
		// Inflated with the group's guard released.
		let (info, source) = match source {
			Source::Ready(info, source) => (info, source),
			Source::Inflate {
				job,
				local,
				timestamp,
				fields,
			} => {
				job.run(stats);
				let payload = job.get(local)?;
				let info = frame::Info {
					size: payload.len() as u64,
					timestamp,
				};
				let header = frame::Header {
					info,
					fields,
					inflated: None,
				};
				(header, frame::Source::Complete(payload))
			}
		};

		self.index += 1;
		// A direct read (not prefetched): count the frame here; the frame::Consumer
		// counts its bytes per chunk as they're read out.
		stats.frames(1);
		let frame = frame::Consumer::new(self.state.clone(), info, source).with_meter(stats.clone());
		Poll::Ready(Ok(Some(match expiry {
			Some(expiry) => frame.with_expiry(expiry.for_frame(self.index..end, true)),
			None => frame,
		})))
	}

	fn poll_read_frame(&mut self, waiter: &kio::Waiter, stats: &stats::Meter) -> Poll<Result<Option<Frame>>> {
		if self.capped() {
			return Poll::Ready(Ok(None));
		}

		// Fast path: serve from the prefetched batch without locking or allocating a waker.
		if let Some(frame) = self.prefetch.pop() {
			self.refresh_if_stale();
			self.index += 1;
			return Poll::Ready(Ok(Some(frame)));
		}

		// The batch is drained: refill it under a single lock, registering the waiter if
		// nothing is ready. Borrow the two fields disjointly so the closure can fill.
		let index = self.index;
		// Never buffer past the cap: `end_at` can be raised later, and those frames must
		// come from the shared state then, not from a batch drained under the old cap.
		let budget = self.end.map_or(usize::MAX, |end| end.saturating_sub(index));
		let prefetch = &mut self.prefetch;
		let raw = self.raw;
		let res = self.state.poll(waiter, |state| {
			if index < state.offset {
				return Poll::Ready(Err(Error::Lagged));
			}
			// `local` can run past the buffered count when frames were cleared out from
			// under us (abort, unfinished drop); clamp so `range` never panics on an
			// out-of-bounds start. `fill` always resets the batch, so an empty range
			// leaves `len == 0` and the terminal checks below resolve abort/fin/pending.
			let local = (index - state.offset).min(state.frames.len());
			let job = prefetch.fill(state, local, budget, raw)?;
			if prefetch.len > 0 {
				// One stamp covers the whole batch: frames popped from the prefetch
				// don't re-stamp until the next refill, which `CAP` bounds.
				state.charge.refresh();
				return Poll::Ready(Ok(job));
			}
			// Nothing completed at `index`: an in-flight tail waits, otherwise resolve
			// the terminal state (whole-frame reads never stream the partial).
			state.poll_terminal(index).map_ok(|()| None)
		});

		match ready!(res) {
			// Inflated with the group's guard released. Only a group from frame 0 inflates.
			Ok(Ok(Some(job))) => self.prefetch.inflate(&job, index, stats)?,
			Ok(Ok(None)) => {}
			Ok(Err(err)) => return Poll::Ready(Err(err)),
			Err(state) => return Poll::Ready(Err(state.abort.clone().unwrap_or(Error::Dropped))),
		}

		// The refill already updated the eviction rank under the group lock.
		self.refreshed = self.cache.pool().now();

		// A fresh batch was just filled (empty only on a clean end). Count the whole
		// batch once here, under no lock, so the drained pops that follow stay free.
		let (frames, bytes) = self.prefetch.buffered();
		stats.frames(frames);
		stats.bytes(bytes);

		Poll::Ready(Ok(self.prefetch.pop().inspect(|_| {
			self.index += 1;
		})))
	}
}

/// Options for a one-shot [`track::Consumer::fetch_group`] of a past group.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Fetch {
	/// Delivery priority for the fetched group's stream, higher first. Defaults to 0.
	pub priority: i8,

	/// Maximum age behind the live edge this caller accepts.
	///
	/// `None` applies only the publisher's retention bound. An explicit value can
	/// tighten, but never relax, that bound.
	pub max_delay: Option<std::time::Duration>,

	/// Index of the first frame to fetch within the group. Defaults to 0, the whole group.
	///
	/// Use this to fill a hole left by a route change: the group's head is already
	/// cached locally and only the tail is missing.
	///
	/// There is no matching end: a fetch always runs to the end of the group, and a
	/// caller wanting less caps the returned consumer with [`Consumer::set_frames`]. Stopping
	/// the *fetch* short would put a group in the cache that is indistinguishable from a
	/// complete one, so a later fetch of the whole group would resolve from it and come
	/// up short.
	pub frame_start: u64,
}

impl Fetch {
	/// Set the delivery priority, returning `self` for chaining.
	pub fn with_priority(mut self, priority: i8) -> Self {
		self.priority = priority;
		self
	}

	/// Limit how far behind the live edge the fetched group may fall.
	pub fn with_max_delay(mut self, max_delay: std::time::Duration) -> Self {
		self.max_delay = Some(max_delay);
		self
	}

	/// Set the first frame to fetch, returning `self` for chaining.
	pub fn with_frame_start(mut self, frame_start: u64) -> Self {
		self.frame_start = frame_start;
		self
	}
}

/// A consumer's request for a single past group, handed to a handler via
/// [`track::Dynamic::poll_requested_group`].
///
/// The handler fulfills it by calling [`Self::accept`], which inserts the group
/// into the track cache (resolving every [`track::Consumer::fetch_group`] that joined the
/// attempt) and returns a [`Producer`] to fill. A relay typically opens a wire
/// FETCH and waits for the publisher to answer before accepting, so a group the
/// publisher lacks is rejected rather than accepted and then aborted. The request
/// carries its own producer handle, so it works the same whether or not the track
/// has been accepted yet.
pub struct Request {
	// pending: /quest/m1/public-model/deferred.md
	pub(crate) state: kio::Producer<track::TrackState>,
	pub(super) fetch: kio::Shared<track::FetchState>,
	pub(super) sequence: u64,
	pub(super) priority: i8,
	pub(super) max_delay: Option<std::time::Duration>,
	pub(super) frame_start: u64,
	// pending: /quest/m1/public-model/deferred.md
	pub(crate) result: kio::Producer<track::FetchOutcome>,
	pub(super) done: bool,
}

#[cfg(test)]
mod test {
	use super::*;
	use crate::model::test_tracing::count_drop_warnings;
	use bytes::Bytes;
	#[cfg(feature = "async")]
	use futures::FutureExt;

	/// Fields sit beside the frames only from the first fielded one, so every read path
	/// must line them up by index across plain frames and prefetch batches.
	#[test]
	#[cfg(feature = "async")]
	fn object_fields_follow_their_frames_through_every_read() {
		let pool = cache::Pool::unbounded();
		let cache = cache::Track::new(pool.clone(), Default::default(), kio::Weak::new());
		let mut group = Producer::new(Info::from(0u64), track::Info::default(), cache);
		group.write_frame(None, Bytes::from_static(b"p")).unwrap();
		let plain = pool.used();
		// Ten fielded frames span two prefetch batches, then plain frames follow.
		for id in 1..=10u64 {
			let header = frame::Header {
				info: frame::Info {
					size: 1,
					timestamp: None,
				},
				fields: Some(Arc::new(super::super::object::Fields {
					id: id * 10,
					properties: Default::default(),
				})),
				inflated: None,
			};
			let mut frame = group.create_object(header).unwrap();
			frame.write(Bytes::from_static(b"f")).unwrap();
			group = frame.finish().unwrap();
		}
		assert!(pool.used() > plain + 10, "the metadata table is charged");
		for _ in 0..Prefetch::CAP {
			group.write_frame(None, Bytes::from_static(b"p")).unwrap();
		}
		group.finish().unwrap();
		let expected = |index: u64| (1..=10).contains(&index).then_some(index * 10);

		let mut objects = group.consume();
		let waiter = kio::Waiter::noop();
		for index in 0..(11 + Prefetch::CAP as u64) {
			let Poll::Ready(Ok(Some(object))) = objects.poll_read_object(&waiter) else {
				panic!("frame {index} is ready");
			};
			assert_eq!(object.fields.map(|fields| fields.id), expected(index), "frame {index}");
		}

		// A public read leaves the batch's fields for a later streamed read to find.
		let mut mixed = group.consume();
		mixed.read_frame().now_or_never().unwrap().unwrap().unwrap();
		for index in 1..Prefetch::CAP as u64 {
			let frame = mixed.next_frame().now_or_never().unwrap().unwrap().unwrap();
			assert_eq!(
				frame.fields.as_ref().map(|fields| fields.id),
				expected(index),
				"frame {index}"
			);
		}
		// Public reads never take a batch's fields, so a plain batch after them must not
		// hand the stale ones to a wire read.
		for _ in 0..Prefetch::CAP {
			mixed.read_frame().now_or_never().unwrap().unwrap().unwrap();
		}
		for index in (2 * Prefetch::CAP as u64)..(11 + Prefetch::CAP as u64) {
			let Poll::Ready(Ok(Some(object))) = mixed.poll_read_object(&waiter) else {
				panic!("frame {index} is ready");
			};
			assert_eq!(object.fields.map(|fields| fields.id), None, "frame {index}");
		}

		drop((objects, mixed));
		group.abort(Error::Cancel).unwrap();
		assert_eq!(pool.used(), 0);
	}

	#[test]
	#[cfg(feature = "async")]
	fn terminal_metadata_stays_charged_after_frames_are_released() {
		let pool = cache::Pool::unbounded();
		let cache = cache::Track::new(pool.clone(), Default::default(), kio::Weak::new());
		let mut group = Producer::new(Info::from(0u64), track::Info::default(), cache);
		group.write_frame(None, Bytes::from_static(b"payload")).unwrap();
		let fields = super::super::object::Fields {
			id: 4,
			properties: Default::default(),
		};
		let charged = size_of::<Relay>() as u64 + fields.heap();
		group.set_end_object(fields.clone()).unwrap();
		assert!(matches!(group.set_end_object(fields), Err(Error::MalformedTrack)));
		group.finish().unwrap();
		let consumer = group.consume();
		group.abort(Error::Cancel).unwrap();
		assert_eq!(consumer.end_object().unwrap().id, 4);
		assert_eq!(pool.used(), charged);
		drop(consumer);
		assert_eq!(pool.used(), 0);
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_leading_gap_may_reach_below_where_the_copy_starts() {
		for (gap, fetchable) in [(Some(6), true), (Some(4), true), (Some(3), false), (None, false)] {
			let mut group = Info { sequence: 0 }.produce();
			group.start_at(2).unwrap();
			group
				.set_wire(Wire {
					sparse: true,
					first_object: false,
					first_id: 6,
					extensions: true,
					has_end: true,
					fetchable: true,
					last: None,
				})
				.unwrap();
			group.mark_gap(6, gap).unwrap();
			assert_eq!(group.wire().unwrap().fetchable, fetchable, "{gap:?}");
		}
	}

	#[test]
	#[cfg(feature = "async")]
	fn demand_watches_readers_without_keeping_the_group_alive() {
		let producer = Info { sequence: 7 }.produce();
		let demand = producer.demand();
		assert_eq!(demand.sequence(), 7);
		assert!(!demand.is_used());
		let first = producer.consume();
		let second = first.clone();
		assert!(demand.is_used());
		drop(first);
		assert!(demand.poll_unused(&kio::Waiter::noop()).is_pending());
		drop(second);
		assert!(matches!(demand.poll_unused(&kio::Waiter::noop()), Poll::Ready(Ok(()))));
		drop(producer);
		assert!(!demand.is_used());
		assert!(matches!(demand.used().now_or_never(), Some(Err(Error::Dropped))));
	}

	#[test]
	fn frame_capacity_growth_is_charged_and_released() {
		for mode in 0..3 {
			let pool = cache::Pool::unbounded();
			let cache = cache::Track::new(pool.clone(), Default::default(), kio::Weak::new());
			let mut group = Producer::new(Info::from(0u64), track::Info::timed(), cache);
			for _ in 0..FRAME_SLOTS {
				group.write_frame(Timestamp::ZERO, bytes::Bytes::new()).unwrap();
			}
			let before = pool.used();
			match mode {
				0 => group.write_frame(Timestamp::ZERO, bytes::Bytes::new()).unwrap(),
				1 => {
					let mut buffer = frame::Buffer::<1>::new();
					buffer.push(Frame::new(Timestamp::ZERO, bytes::Bytes::new())).unwrap();
					group.write_frames(&mut buffer).unwrap();
				}
				_ => {
					let frame = group
						.create_frame(frame::Info {
							size: 0,
							timestamp: Some(Timestamp::ZERO),
						})
						.unwrap();
					group = frame.finish().unwrap();
				}
			}
			let capacity = group.handle.state.read().frames.capacity();
			assert!(capacity > FRAME_SLOTS);
			assert_eq!(
				pool.used() - before,
				((capacity - FRAME_SLOTS) * size_of::<Frame>()) as u64
			);
			let consumer = group.consume();
			group.abort(Error::Cancel).unwrap();
			assert_eq!(consumer.cursor.state.read().frames.capacity(), 0);
		}
	}

	/// [`FRAME_SLOTS`] is std's rounding, not ours, so measure it: a larger real value
	/// would undercharge every cached group without touching a line of this crate.
	#[test]
	fn one_frame_fits_the_charged_slots() {
		let mut frames: VecDeque<Frame> = VecDeque::new();
		frames.push_back(Frame::new(Timestamp::ZERO, Bytes::new()));
		let capacity = frames.capacity();
		assert!(
			capacity <= FRAME_SLOTS,
			"a one-frame deque now allocates {capacity} slots"
		);
	}

	#[test]
	#[cfg(feature = "async")]
	fn basic_frame_reading() {
		let mut producer = Info { sequence: 0 }.produce();
		producer
			.write_frame(Timestamp::ZERO, Bytes::from_static(b"frame0"))
			.unwrap();
		producer
			.write_frame(Timestamp::ZERO, Bytes::from_static(b"frame1"))
			.unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let f0 = consumer.next_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(f0.size, 6);
		let f1 = consumer.next_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(f1.size, 6);
		let end = consumer.next_frame().now_or_never().unwrap().unwrap();
		assert!(end.is_none());
	}

	#[test]
	#[cfg(feature = "async")]
	fn read_frame_preserves_timestamp() {
		let mut producer = Info { sequence: 0 }.produce();
		let timestamp = Timestamp::from_micros(20_000).unwrap();
		producer.write_frame(timestamp, Bytes::from_static(b"hello")).unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let frame = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(frame.timestamp.unwrap().as_micros(), 20_000);
		assert_eq!(frame.payload, Bytes::from_static(b"hello"));
	}

	/// An untimed frame reads back untimed, and still counts as the group's first frame:
	/// an empty group and a group on an untimed track are different answers.
	#[test]
	#[cfg(feature = "async")]
	fn an_untimed_frame_still_starts_the_group() {
		let mut producer = Info { sequence: 0 }.produce_untimed();
		let waiter = kio::Waiter::noop();
		assert!(
			producer.handle.poll_started(&waiter).is_pending(),
			"nothing presented yet"
		);

		producer.write_frame(None, Bytes::from_static(b"hello")).unwrap();
		producer.finish().unwrap();

		assert!(producer.handle.poll_started(&waiter).is_ready());
		assert_eq!(producer.handle.timestamp(), None);
		assert_eq!(producer.handle.latest(), None);

		let mut consumer = producer.consume();
		let frame = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(frame.timestamp, None);
		assert_eq!(frame.payload, Bytes::from_static(b"hello"));
	}

	/// A track is all timed or all untimed, so a frame that doesn't match it is refused.
	#[test]
	fn a_mismatched_frame_is_refused() {
		let mut untimed = Info { sequence: 0 }.produce_untimed();
		assert!(matches!(
			untimed.write_frame(Timestamp::ZERO, Bytes::from_static(b"x")),
			Err(Error::TimestampMismatch)
		));

		let mut timed = Info { sequence: 0 }.produce();
		assert!(matches!(
			timed.write_frame(None, Bytes::from_static(b"x")),
			Err(Error::TimestampMismatch)
		));
		assert!(matches!(
			timed.create_frame(frame::Info {
				size: 1,
				timestamp: None
			}),
			Err(Error::TimestampMismatch)
		));
	}

	#[test]
	#[cfg(feature = "async")]
	fn chunked_frame_reads_whole() {
		let producer = Info { sequence: 0 }.produce();
		let mut frame = producer
			.create_frame(frame::Info {
				size: 10,
				timestamp: Some(Timestamp::ZERO),
			})
			.unwrap();
		frame.write(Bytes::from_static(b"hello")).unwrap();
		frame.write(Bytes::from_static(b"world")).unwrap();
		let mut producer = frame.finish().unwrap();
		producer.finish().unwrap();

		// Frame data is held in a single per-frame buffer; a whole-frame read returns
		// the full contents in one slice.
		let mut consumer = producer.consume();
		let frame = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(frame.payload, Bytes::from_static(b"helloworld"));
	}

	#[test]
	#[cfg(feature = "async")]
	fn chunked_frame_streams_partial() {
		let producer = Info { sequence: 0 }.produce();
		let mut consumer = producer.consume();

		let mut frame = producer
			.create_frame(frame::Info {
				size: 6,
				timestamp: Some(Timestamp::ZERO),
			})
			.unwrap();
		frame.write(Bytes::from_static(b"foo")).unwrap();

		// A consumer can stream the in-flight tail before it's finished.
		let mut f = consumer.next_frame().now_or_never().unwrap().unwrap().unwrap();
		let c1 = f.read_chunk().now_or_never().unwrap().unwrap();
		assert_eq!(c1, Some(Bytes::from_static(b"foo")));
		assert!(f.read_chunk().now_or_never().is_none());

		frame.write(Bytes::from_static(b"bar")).unwrap();
		let _producer = frame.finish().unwrap();

		let c2 = f.read_chunk().now_or_never().unwrap().unwrap();
		assert_eq!(c2, Some(Bytes::from_static(b"bar")));
		let c3 = f.read_chunk().now_or_never().unwrap().unwrap();
		assert_eq!(c3, None);
	}

	#[test]
	#[cfg(feature = "async")]
	fn group_finish_returns_none() {
		let mut producer = Info { sequence: 0 }.produce();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let end = consumer.next_frame().now_or_never().unwrap().unwrap();
		assert!(end.is_none());
	}

	/// Draining the first parked group must not enter an unrelated group's lock.
	/// Holding an unrelated group guard makes a backlog walk deadlock.
	#[test]
	fn draining_a_parked_group_does_not_lock_the_backlog() {
		let broadcast = super::super::broadcast::Producer::new(Default::default());
		let track = broadcast.create_track("parked", track::Info::timed()).unwrap();
		let mut subscriber =
			track.subscribe(track::Subscription::default().with_max_delay(std::time::Duration::from_secs(3600)));
		let groups: Vec<_> = (0..3)
			.map(|sequence| {
				let mut group = track.create_group(Info { sequence }).unwrap();
				group.write_frame(Timestamp::ZERO, b"f".as_ref()).unwrap();
				group.finish().unwrap();
				group
			})
			.collect();
		subscriber.set_groups(..0);
		let waiter = kio::Waiter::noop();
		assert!(subscriber.poll_recv_group(&waiter).is_pending());
		subscriber.set_groups(..1);
		let _unrelated = groups[2].handle.state.read();
		let Poll::Ready(Ok(Some(group))) = subscriber.poll_recv_group(&waiter) else {
			panic!("group zero is in range")
		};
		assert_eq!(group.sequence, 0);
	}

	#[test]
	#[cfg(feature = "async")]
	fn abort_propagates() {
		let producer = Info { sequence: 0 }.produce();
		let mut consumer = producer.consume();
		producer.abort(crate::Error::Cancel).unwrap();

		let result = consumer.next_frame().now_or_never().unwrap();
		assert!(matches!(result, Err(crate::Error::Cancel)));
	}

	#[test]
	fn abort_unused_pairs_with_try_consume() {
		let producer = Info { sequence: 0 }.produce();

		let consumer = producer.handle.try_consume().expect("open");
		assert!(
			!producer.abort_unused(crate::Error::Cancel),
			"a reader declines the abort"
		);
		drop(consumer);

		assert!(producer.abort_unused(crate::Error::Cancel));
		assert!(
			producer.handle.try_consume().is_none(),
			"an aborted group mints no reader"
		);
	}

	#[test]
	fn abort_clears_cached_frames() {
		let mut producer = Info { sequence: 0 }.produce();
		producer
			.write_frame(Timestamp::ZERO, Bytes::from_static(b"data"))
			.unwrap();

		// A stale consumer that never reads must not pin the cached frames.
		let _consumer = producer.consume();
		assert_eq!(producer.handle.state.read().frames.len(), 1);

		producer.handle().abort(crate::Error::Cancel).unwrap();

		let state = producer.handle.state.read();
		assert!(state.frames.is_empty(), "cached frames should be dropped on abort");
		assert_eq!(state.cache, 0);
	}

	/// A failed copy moves its frames to the readers that could resume it, so they outlive
	/// the release only as long as one of those readers does; cache policy and an oversized
	/// group keep nothing.
	#[test]
	fn an_abort_hands_the_head_to_its_resumers() {
		for (err, kept) in [
			(Error::Cancel, true),
			(Error::Evicted, false),
			(Error::GroupTooLarge, false),
		] {
			let pool = cache::Pool::unbounded();
			let cache = cache::Track::new(pool.clone(), Default::default(), kio::Weak::new());
			let mut producer = Producer::new(Info::from(0u64), track::Info::default(), cache);
			producer.write_frame(None, Bytes::from_static(b"a")).unwrap();
			producer.write_frame(None, Bytes::from_static(b"b")).unwrap();
			let handoff = producer.consume().handoff();
			assert_eq!(handoff.head().map(|head| head.frames.len()), Some(2), "read live");

			producer.handle().abort(err.clone()).unwrap();
			assert!(producer.handle.state.read().frames.is_empty(), "{err}: released");
			assert_eq!(pool.used() > 0, kept, "{err}: billed only while held");
			let head = handoff.head();
			assert_eq!(head.is_some(), kept, "{err}");
			if let Some(head) = head {
				assert_eq!((head.start, head.first_object, head.frames.len()), (0, true, 2));
				let held = Arc::downgrade(&head);
				drop((head, handoff));
				assert!(held.upgrade().is_none(), "freed with the last resumer");
				assert_eq!(pool.used(), 0, "unbilled with it");
			}
		}
	}

	/// A resuming reader that stops polling holds a failed copy's head no longer than an
	/// idle group stays cached.
	#[test]
	fn a_held_head_expires_with_the_pool_window() {
		let pool = cache::Pool::new(cache::Config::default().with_expiry(std::time::Duration::from_secs(1)));
		let cache = cache::Track::new(pool.clone(), Default::default(), kio::Weak::new());
		let mut producer = Producer::new(Info::from(0u64), track::Info::default(), cache);
		producer.write_frame(None, Bytes::from_static(b"a")).unwrap();
		let handoff = producer.consume().handoff();
		producer.handle().abort(Error::Cancel).unwrap();
		drop(producer);
		assert!(handoff.head().is_some());

		pool.step(std::time::Duration::from_secs(2));
		pool.sweep();
		assert!(handoff.head().is_none(), "the sweep drops a held head past the window");
		assert_eq!(pool.used(), 0);
	}

	/// The registry the sweep expires held heads from grows with the heads held, not with
	/// every group that failed, and gives back what a burst grew it to once they expire.
	#[test]
	fn the_held_registry_follows_the_heads_held() {
		const BURST: usize = 64;
		let pool = cache::Pool::new(cache::Config::default().with_expiry(std::time::Duration::from_secs(1)));
		let cache = cache::Track::new(pool.clone(), Default::default(), kio::Weak::new());
		let fail = |sequence: u64| {
			let mut producer = Producer::new(Info::from(sequence), track::Info::default(), cache.clone());
			producer.write_frame(None, Bytes::from_static(b"a")).unwrap();
			let handoff = producer.consume().handoff();
			producer.abort(Error::Cancel).unwrap();
			handoff
		};

		// Recovery adopts each head as soon as its group fails.
		for sequence in 0..BURST as u64 {
			drop(fail(sequence));
		}
		assert!(pool.held_capacity() < BURST, "adopted hand-offs pinned the registry");

		// Readers that stop polling hold every head until the window passes.
		let held: Vec<_> = (0..BURST as u64).map(fail).collect();
		assert!(pool.held_capacity() >= BURST);
		pool.step(std::time::Duration::from_secs(2));
		pool.sweep();
		assert!(held.iter().all(|handoff| handoff.head().is_none()));
		assert!(pool.held_capacity() < BURST, "the sweep kept a burst's allocation");
	}

	/// A failed copy's head is held only while the pool has room for it.
	#[test]
	fn a_full_pool_holds_no_head() {
		let pool = cache::Pool::new(cache::Config::default().with_capacity(1));
		let cache = cache::Track::new(pool.clone(), Default::default(), kio::Weak::new());
		let mut producer = Producer::new(Info::from(0u64), track::Info::default(), cache);
		producer.write_frame(None, Bytes::from_static(b"a")).unwrap();
		let handoff = producer.consume().handoff();
		producer.handle().abort(Error::Cancel).unwrap();
		assert!(handoff.head().is_none());
		assert_eq!(pool.used(), 0);
	}

	/// The in-flight frame's unwritten remainder counts against the group budget, so an
	/// adoption cannot take the group past it once the frame finishes streaming.
	#[test]
	fn an_open_frame_reserves_its_remainder_from_adoption() {
		let pool = cache::Pool::unbounded();
		let cache = cache::Track::new(pool, Default::default(), kio::Weak::new());
		let mut tail = Producer::new(Info::from(0u64), track::Info::default(), cache);
		tail.start_at(1).unwrap();
		let adopter = tail.handle();
		let _frame = tail
			.create_frame(frame::Info {
				size: MAX_CACHE_BYTES - 1,
				timestamp: None,
			})
			.unwrap();
		let head = Head {
			start: 0,
			first_object: true,
			frames: vec![Frame::new(None, Bytes::from_static(b"ab"))],
			objects: Vec::new(),
			sizes: None,
		};
		adopter.adopt_head(&head);
		assert_eq!(
			adopter.state.read().offset,
			1,
			"the head does not fit beside the open frame"
		);
	}

	/// An adopted head leads the fields and the wire header, whether the header was set
	/// before the adoption or arrives after it.
	#[test]
	#[cfg(feature = "async")]
	fn an_adopted_head_keeps_fields_and_the_wire_aligned() {
		let fields = |id| {
			Some(Arc::new(super::super::object::Fields {
				id,
				properties: Default::default(),
			}))
		};
		let head = Head {
			start: 0,
			first_object: true,
			frames: vec![
				Frame::new(None, Bytes::from_static(b"a")),
				Frame::new(None, Bytes::from_static(b"b")),
			],
			objects: vec![None, fields(1)],
			sizes: None,
		};
		let wire = Wire {
			sparse: false,
			first_object: false,
			first_id: 2,
			extensions: true,
			has_end: true,
			fetchable: true,
			last: None,
		};
		for wired_first in [true, false] {
			let pool = cache::Pool::unbounded();
			let cache = cache::Track::new(pool.clone(), Default::default(), kio::Weak::new());
			let mut tail = Producer::new(Info::from(0u64), track::Info::default(), cache);
			tail.start_at(2).unwrap();
			if wired_first {
				tail.set_wire(wire.clone()).unwrap();
			}
			let mut reader = tail.consume();
			reader.start_at(2);
			let before = pool.used();
			tail.handle.adopt_head(&head);
			assert!(pool.used() >= before + 2, "the adopted frames are charged");
			if !wired_first {
				tail.set_wire(wire.clone()).unwrap();
			}
			let written = tail.handle.state.read().wire().cloned().unwrap();
			assert_eq!(
				(written.first_object, written.first_id, written.last),
				(true, 0, Some(1))
			);
			tail.mark_gap(2, None).unwrap();
			assert!(
				tail.handle.state.read().wire().unwrap().fetchable,
				"no gap below the stream"
			);
			tail.write_frame(None, Bytes::from_static(b"c")).unwrap();
			tail.finish().unwrap();

			let waiter = kio::Waiter::noop();
			let mut whole = tail.consume();
			let mut read = Vec::new();
			while let Poll::Ready(Ok(Some(object))) = whole.poll_read_object(&waiter) {
				read.push((object.frame.payload, object.fields.map(|fields| fields.id)));
			}
			assert_eq!(
				read,
				[
					(Bytes::from_static(b"a"), None),
					(Bytes::from_static(b"b"), Some(1)),
					(Bytes::from_static(b"c"), None)
				]
			);
			let next = reader.read_frame().now_or_never().unwrap().unwrap();
			assert_eq!(
				next.map(|frame| frame.payload),
				Some(Bytes::from_static(b"c")),
				"a reader keeps its place"
			);
		}
	}

	#[test]
	#[cfg(feature = "async")]
	fn drop_unfinished_clears_cached_frames() {
		let mut writer = Info { sequence: 0 }.produce();
		let producer = writer.handle();
		writer
			.write_frame(Timestamp::ZERO, Bytes::from_static(b"data"))
			.unwrap();

		// A stale consumer keeps the channel (and thus the cache) alive.
		let mut consumer = producer.consume();
		assert_eq!(producer.state.read().frames.len(), 1);

		// Drop every producer without finishing: the cache is released.
		drop(writer);
		drop(producer);

		let result = consumer.next_frame().now_or_never().unwrap();
		assert!(matches!(result, Err(crate::Error::Dropped)));
	}

	#[test]
	fn drop_after_abort_does_not_warn() {
		let warns = count_drop_warnings("group::Producer dropped without finish", || {
			let mut writer = Info { sequence: 0 }.produce();
			let keep = writer.handle();
			let producer = writer.handle();
			writer
				.write_frame(Timestamp::ZERO, Bytes::from_static(b"data"))
				.unwrap();
			let _consumer = producer.consume();
			writer.abort(crate::Error::Cancel).unwrap();
			drop(keep);
		});
		assert_eq!(warns, 0, "abort-then-drop must not emit unfinished-producer WARN");
	}

	#[test]
	fn drop_unfinished_warns() {
		let warns = count_drop_warnings("group::Producer dropped without finish", || {
			let mut writer = Info { sequence: 0 }.produce();
			let producer = writer.handle();
			writer
				.write_frame(Timestamp::ZERO, Bytes::from_static(b"data"))
				.unwrap();
			let _consumer = producer.consume();
			drop(writer);
			drop(producer);
		});
		assert_eq!(warns, 1, "unfinished drop must emit one unfinished-producer WARN");
	}

	#[test]
	#[cfg(feature = "async")]
	fn drop_finished_keeps_cached_frames() {
		let mut producer = Info { sequence: 0 }.produce();
		producer
			.write_frame(Timestamp::ZERO, Bytes::from_static(b"data"))
			.unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		drop(producer);

		// A cleanly finished group keeps its cache so the consumer can still drain.
		let frame = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(frame.payload, Bytes::from_static(b"data"));
	}

	#[test]
	#[cfg(feature = "async")]
	fn pending_then_ready() {
		let mut producer = Info { sequence: 0 }.produce();
		let mut consumer = producer.consume();

		// Consumer blocks because no frames yet.
		assert!(consumer.next_frame().now_or_never().is_none());

		producer
			.write_frame(Timestamp::ZERO, Bytes::from_static(b"data"))
			.unwrap();
		producer.finish().unwrap();

		let frame = consumer.next_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(frame.size, 4);
	}

	#[test]
	#[cfg(feature = "async")]
	fn overflow_aborts_the_group() {
		let mut producer = Info { sequence: 0 }.produce();
		let mut consumer = producer.consume();

		let big = Bytes::from(vec![0u8; MAX_CACHE_BYTES as usize]);
		producer.write_frame(Timestamp::ZERO, big.clone()).unwrap();
		assert!(matches!(
			producer.write_frame(Timestamp::ZERO, big),
			Err(Error::GroupTooLarge)
		));

		{
			let state = producer.handle.state.read();
			assert!(matches!(state.abort, Some(Error::GroupTooLarge)));
			assert!(state.frames.is_empty());
			assert_eq!(state.offset, 0);
		}

		let result = consumer.next_frame().now_or_never().unwrap();
		assert!(matches!(result, Err(Error::GroupTooLarge)));
	}

	#[test]
	fn no_overflow_under_budget() {
		let mut producer = Info { sequence: 0 }.produce();
		// 8192 one-byte frames is the largest legal group; they all stay cached.
		for _ in 0..MAX_GROUP_FRAMES {
			producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"x")).unwrap();
		}
		producer.finish().unwrap();

		let state = producer.handle.state.read();
		assert_eq!(state.offset, 0);
		assert_eq!(state.frames.len(), MAX_GROUP_FRAMES);
		assert!(state.abort.is_none());
	}

	#[test]
	fn writer_sees_group_too_large_on_the_8193rd_frame() {
		let mut producer = Info { sequence: 0 }.produce();
		for _ in 0..MAX_GROUP_FRAMES {
			producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"x")).unwrap();
		}
		assert!(matches!(
			producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"x")),
			Err(Error::GroupTooLarge)
		));
		assert!(matches!(producer.handle.state.read().abort, Some(Error::GroupTooLarge)));
	}

	#[test]
	#[cfg(feature = "async")]
	fn clone_consumer_independent() {
		let mut producer = Info { sequence: 0 }.produce();
		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"a")).unwrap();

		let mut c1 = producer.consume();
		// Read one frame from c1
		let _ = c1.next_frame().now_or_never().unwrap().unwrap().unwrap();

		// Clone c1, inheriting its index (past first frame).
		let mut c2 = c1.clone();

		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"b")).unwrap();
		producer.finish().unwrap();

		// c2 should get the second frame (inherited index)
		let f = c2.next_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(f.size, 1); // "b"

		let end = c2.next_frame().now_or_never().unwrap().unwrap();
		assert!(end.is_none());
	}

	#[cfg(feature = "async")]
	fn prefetched_consumer(pool: &cache::Pool, max_age: std::time::Duration) -> (Producer, Consumer) {
		let cache = cache::Track::new(pool.clone(), Default::default(), kio::Weak::new());
		let track = track::Info::timed().with_max_age(max_age);
		let mut producer = Producer::new(Info { sequence: 0 }, track, cache);
		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"a")).unwrap();
		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"b")).unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		(producer, consumer)
	}

	#[test]
	#[cfg(feature = "async")]
	fn keep_alive_restarts_expiry_within_one_sampled_tick() {
		let pool = cache::Pool::new(cache::Config::default().with_expiry(std::time::Duration::from_secs(1)));
		let (producer, consumer) = prefetched_consumer(&pool, std::time::Duration::MAX);
		consumer.keep_alive();
		let stamp = producer.handle.cache_accessed();
		assert_eq!(producer.handle.cache_accessed_tick(Some(pool.now())), Some(pool.now()));
		consumer.keep_alive();
		assert_eq!(
			producer.handle.cache_accessed(),
			stamp,
			"same-tick reads do not inflate eviction rank"
		);
		assert_eq!(
			producer.handle.cache_accessed_tick(None),
			None,
			"every frame resets the idle clock"
		);
	}

	#[test]
	#[cfg(feature = "async")]
	fn keep_alive_keeps_the_counted_access_mean_balanced() {
		let pool = cache::Pool::new(cache::Config::default().with_capacity(1_000_000));
		let (producer, consumer) = prefetched_consumer(&pool, std::time::Duration::MAX);
		producer.handle.cache_demote();
		consumer.keep_alive();
		assert_eq!(pool.average(), Some(producer.handle.cache_accessed()));
		let before = producer.handle.cache_accessed();
		pool.step(std::time::Duration::from_millis(100));
		consumer.keep_alive();
		assert!(producer.handle.cache_accessed() > before);
		assert_eq!(pool.average(), Some(producer.handle.cache_accessed()));
		producer.abort(Error::Cancel).unwrap();
		assert_eq!(
			pool.average(),
			None,
			"removal subtracts the same sample that was inserted"
		);
	}

	#[test]
	#[cfg(feature = "async")]
	fn prefetch_refresh_honors_pool_expiry() {
		let config = cache::Config::default().with_expiry(std::time::Duration::from_secs(1));
		let pool = cache::Pool::new(config);
		let (producer, mut consumer) = prefetched_consumer(&pool, std::time::Duration::MAX);
		let before = producer.handle.cache_accessed();

		pool.step(std::time::Duration::from_millis(600));
		consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();

		assert!(producer.handle.cache_accessed() > before, "the pool cadence is used");
	}

	#[test]
	#[cfg(feature = "async")]
	fn prefetch_refresh_honors_track_max_age() {
		let config = cache::Config::default().with_expiry(std::time::Duration::from_secs(30));
		let pool = cache::Pool::new(config);
		let (producer, mut consumer) = prefetched_consumer(&pool, std::time::Duration::from_secs(1));
		let before = producer.handle.cache_accessed();

		pool.step(std::time::Duration::from_millis(600));
		consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();

		assert!(
			producer.handle.cache_accessed() > before,
			"the track cadence remains in force"
		);
	}

	/// Reading more than one prefetch batch drains every frame in order across the
	/// batch boundary (the refill starts exactly where the previous batch ended).
	#[test]
	#[cfg(feature = "async")]
	fn read_frame_crosses_prefetch_batches() {
		let n = Prefetch::CAP * 3 + 5;
		let mut producer = Info { sequence: 0 }.produce();
		for i in 0..n {
			producer
				.write_frame(Timestamp::ZERO, Bytes::from(vec![i as u8; 4]))
				.unwrap();
		}
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		for i in 0..n {
			let frame = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
			assert_eq!(frame.payload, Bytes::from(vec![i as u8; 4]));
		}
		assert!(consumer.read_frame().now_or_never().unwrap().unwrap().is_none());
	}

	/// A finished group is still aborted once its frames are released to free memory (the
	/// track's max age window, or the cache pool). A reader that already drained every frame
	/// is missing nothing, so it must see the clean end of group rather than the abort.
	#[test]
	#[cfg(feature = "async")]
	fn abort_after_finish_keeps_the_clean_end_for_a_drained_reader() {
		let mut producer = Info { sequence: 0 }.produce();
		producer
			.write_frame(Timestamp::ZERO, Bytes::from_static(b"hello"))
			.unwrap();
		producer.finish().unwrap();

		let mut drained = producer.consume();
		let mut behind = producer.consume();
		let frame = drained.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(frame.payload, Bytes::from_static(b"hello"));

		producer.abort(Error::Old).unwrap();

		// Drained everything before the abort: nothing is missing.
		assert!(drained.read_frame().now_or_never().unwrap().unwrap().is_none());
		assert!(drained.next_frame().now_or_never().unwrap().unwrap().is_none());

		// Never read the frame, and its bytes are gone: a truncated stream, not a clean end.
		assert!(matches!(behind.read_frame().now_or_never().unwrap(), Err(Error::Old)));
	}

	/// `finished` answers for the cursor: a drained reader gets the clean end even after the
	/// abort that released the cache, and one that stopped short gets that abort. The
	/// producer's total stays available on `frame_count`.
	#[test]
	#[cfg(feature = "async")]
	fn finished_answers_for_the_cursor() {
		let mut producer = Info { sequence: 0 }.produce();
		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"a")).unwrap();
		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"b")).unwrap();
		producer.finish().unwrap();

		let mut drained = producer.consume();
		let mut behind = producer.consume();
		while drained.read_frame().now_or_never().unwrap().unwrap().is_some() {}
		behind.read_frame().now_or_never().unwrap().unwrap().unwrap();

		producer.abort(Error::Old).unwrap();

		assert_eq!(drained.finished().now_or_never().unwrap().unwrap(), 2);
		assert!(matches!(behind.finished().now_or_never().unwrap(), Err(Error::Old)));
		assert_eq!(behind.frame_count(), 2);
	}

	/// A cursor on a group aborted for overflowing its budget can never reach the end,
	/// so `finished` reports that abort instead of parking forever.
	#[test]
	#[cfg(feature = "async")]
	fn finished_reports_a_group_too_large() {
		let mut producer = Info { sequence: 0 }.produce();
		let mut consumer = producer.consume();

		let big = Bytes::from(vec![0u8; MAX_CACHE_BYTES as usize]);
		producer.write_frame(Timestamp::ZERO, big.clone()).unwrap();
		assert!(matches!(
			producer.write_frame(Timestamp::ZERO, big),
			Err(Error::GroupTooLarge)
		));

		assert!(matches!(
			consumer.finished().now_or_never().unwrap(),
			Err(Error::GroupTooLarge)
		));
	}

	/// `next_frame` drains frames a prior `read_frame` prefetched, preserving order.
	#[test]
	#[cfg(feature = "async")]
	fn interleave_read_and_next_frame() {
		let mut producer = Info { sequence: 0 }.produce();
		for i in 0..5u8 {
			producer.write_frame(Timestamp::ZERO, Bytes::from(vec![i; 1])).unwrap();
		}
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		// The first whole-frame read prefetches all five frames into the batch.
		let f0 = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(f0.payload, Bytes::from(vec![0u8; 1]));

		// next_frame must continue from the batch, not skip ahead or repeat.
		for i in 1..5u8 {
			let mut f = consumer.next_frame().now_or_never().unwrap().unwrap().unwrap();
			let data = f.read_all().now_or_never().unwrap().unwrap();
			assert_eq!(data, Bytes::from(vec![i; 1]));
		}
		assert!(consumer.next_frame().now_or_never().unwrap().unwrap().is_none());
	}

	/// A `read_frame` whose index sits past the buffered frames (cleared by an abort)
	/// must surface the error, not panic on an out-of-range `range(local..)`.
	#[test]
	#[cfg(feature = "async")]
	fn read_frame_past_cleared_frames_does_not_panic() {
		let mut producer = Info { sequence: 0 }.produce();
		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"a")).unwrap();
		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"b")).unwrap();

		let mut consumer = producer.consume();
		consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();

		// Abort clears the cached frames but leaves the consumer's index (2) past them, so the
		// refill's `local` (2) exceeds `frames.len()` (0).
		producer.abort(Error::Cancel).unwrap();

		let result = consumer.read_frame().now_or_never().unwrap();
		assert!(matches!(result, Err(Error::Cancel)), "expected Cancel, got {result:?}");
	}

	/// Dropping a consumer mid-batch must drop the buffered-but-untaken frames
	/// (exercises the `MaybeUninit` Drop path; run under miri to catch leaks/UB).
	#[test]
	#[cfg(feature = "async")]
	fn drop_with_partial_batch() {
		let mut producer = Info { sequence: 0 }.produce();
		for _ in 0..Prefetch::CAP {
			producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"x")).unwrap();
		}
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		// Take one frame so the batch is filled but only partially drained.
		let _ = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		drop(consumer);
	}

	/// A parked chunk reader is woken by each chunk write. kio only notifies when
	/// a write guard was mutably accessed, so `frame_notify` must mark the guard
	/// modified; a guard dropped untouched wakes nobody and the reader would
	/// stall until the frame completed.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn chunk_write_wakes_parked_reader() {
		let producer = Info { sequence: 0 }.produce();
		let mut consumer = producer.consume();
		let mut frame = producer
			.create_frame(frame::Info {
				size: 6,
				timestamp: Some(Timestamp::ZERO),
			})
			.unwrap();
		let mut f = consumer.next_frame().await.unwrap().unwrap();
		let handle = moq_clock::sim::spawn(async move { f.read_chunk().await });
		// Let the reader park on the empty partial before the chunk lands.
		moq_clock::sleep(std::time::Duration::from_millis(50)).await;
		frame.write(Bytes::from_static(b"foo")).unwrap();
		let chunk = moq_clock::timeout(std::time::Duration::from_secs(2), handle)
			.await
			.expect("parked chunk reader was never woken by the chunk write")
			.unwrap()
			.unwrap();
		assert_eq!(chunk, Some(Bytes::from_static(b"foo")));
	}

	/// Each frame is stamped when its last byte lands, whichever way it is written, and
	/// readers see that stamp however late they read.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn frames_carry_when_they_landed() {
		use std::time::Duration;

		let start = moq_clock::now();
		let mut producer = Info { sequence: 0 }.produce();
		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"a")).unwrap();

		moq_clock::sleep(Duration::from_millis(10)).await;
		let mut chunked = producer
			.create_frame(frame::Info {
				size: 2,
				timestamp: Some(Timestamp::ZERO),
			})
			.unwrap();
		chunked.write(Bytes::from_static(b"b")).unwrap();
		moq_clock::sleep(Duration::from_millis(10)).await;
		chunked.write(Bytes::from_static(b"c")).unwrap();
		producer = chunked.finish().unwrap();

		moq_clock::sleep(Duration::from_millis(10)).await;
		let mut batch = frame::Buffer::<2>::new();
		batch
			.push(Frame::new(Timestamp::ZERO, Bytes::from_static(b"d")))
			.unwrap();
		moq_clock::sleep(Duration::from_millis(10)).await;
		batch
			.push(Frame::new(Timestamp::ZERO, Bytes::from_static(b"e")))
			.unwrap();
		producer.write_frames(&mut batch).unwrap();
		producer.finish().unwrap();

		moq_clock::sleep(Duration::from_secs(1)).await;
		let mut consumer = producer.consume();
		let mut arrived = Vec::new();
		while let Some(frame) = consumer.read_frame().await.unwrap() {
			arrived.push(frame.arrived().duration_since(start).as_millis());
		}
		assert_eq!(arrived, [0, 20, 40, 40], "stamped on completion, a batch at its write");
	}

	/// A frame whose timestamp is at a different scale is converted to the group's
	/// scale by `create_frame`.
	#[test]
	fn create_frame_converts_mismatched_scale() {
		use crate::{Timescale, Timestamp};

		let producer = Producer::new(
			Info { sequence: 0 },
			track::Info::default().with_timescale(Timescale::MICRO),
			Default::default(),
		);
		let frame = frame::Info {
			size: 3,
			timestamp: Some(Timestamp::from_millis(1).unwrap()), // 1ms -> 1000µs
		};
		let writer = producer.create_frame(frame).unwrap();
		assert_eq!(writer.timestamp.unwrap().scale(), Timescale::MICRO);
		assert_eq!(writer.timestamp.unwrap().value(), 1000);
	}

	/// An explicit current timestamp is converted to the group's scale.
	#[test]
	fn create_frame_converts_current_timestamp() {
		use crate::Timescale;

		let producer = Producer::new(
			Info { sequence: 0 },
			track::Info::default().with_timescale(Timescale::MICRO),
			Default::default(),
		);
		let writer = producer
			.create_frame(frame::Info {
				size: 3,
				timestamp: Some(Timestamp::now()),
			})
			.unwrap();
		assert_eq!(writer.timestamp.unwrap().scale(), Timescale::MICRO);
		assert!(!writer.timestamp.unwrap().is_zero(), "local clock should be non-zero");
	}

	/// A group can start partway in, so a route can serve the tail of a group whose
	/// head came from somewhere else.
	#[test]
	#[cfg(feature = "async")]
	fn start_at_starts_the_group_later() {
		let mut producer = Info { sequence: 0 }.produce();
		producer.start_at(3).unwrap();
		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"d")).unwrap();
		producer.finish().unwrap();

		// The frame landed at index 3, so the group's length counts the missing head.
		assert_eq!(producer.frame_count(), 4);

		let mut consumer = producer.consume();
		assert_eq!(consumer.frame_count(), 4);

		// A reader positioned at the start is missing the head, and `finished` answers
		// for that cursor.
		assert!(matches!(
			consumer.finished().now_or_never().unwrap(),
			Err(Error::Lagged)
		));
		assert!(matches!(
			consumer.read_frame().now_or_never().unwrap(),
			Err(Error::Lagged)
		));
	}

	/// Seeking to the group's first available frame is how a front's pump picks up
	/// the tail; a lower index clamps up rather than failing.
	#[test]
	#[cfg(feature = "async")]
	fn start_at_clamps_up_to_the_first_frame() {
		let mut producer = Info { sequence: 0 }.produce();
		producer.start_at(3).unwrap();
		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"d")).unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		consumer.start_at(1);
		assert_eq!(consumer.index(), 3, "clamped up to the first frame that exists");
		assert_eq!(
			consumer.read_frame().now_or_never().unwrap().unwrap().unwrap().payload,
			Bytes::from_static(b"d")
		);
	}

	/// `end_at` ends the read cleanly at the cap, and raising it re-offers the frames
	/// still cached behind it.
	#[test]
	#[cfg(feature = "async")]
	fn end_at_caps_and_reopens() {
		let mut producer = Info { sequence: 0 }.produce();
		for i in 0..4u8 {
			producer.write_frame(Timestamp::ZERO, Bytes::from(vec![i])).unwrap();
		}
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		consumer.set_frames(..2);
		assert_eq!(
			consumer.read_frame().now_or_never().unwrap().unwrap().unwrap().payload[0],
			0
		);
		assert_eq!(
			consumer.read_frame().now_or_never().unwrap().unwrap().unwrap().payload[0],
			1
		);
		assert!(
			consumer.read_frame().now_or_never().unwrap().unwrap().is_none(),
			"capped reads end cleanly"
		);

		consumer.set_frames(..);
		assert_eq!(
			consumer.read_frame().now_or_never().unwrap().unwrap().unwrap().payload[0],
			2
		);
	}

	#[test]
	#[cfg(feature = "async")]
	fn frame_ranges_preserve_progress_and_make_inclusion_explicit() {
		let mut producer = Info { sequence: 0 }.produce();
		for i in 0..4u8 {
			producer.write_frame(Timestamp::ZERO, Bytes::from(vec![i])).unwrap();
		}
		producer.finish().unwrap();
		let mut consumer = producer.consume();
		consumer.set_frames(1..=1);
		assert_eq!(
			consumer.read_frame().now_or_never().unwrap().unwrap().unwrap().payload[0],
			1
		);
		assert!(consumer.read_frame().now_or_never().unwrap().unwrap().is_none());
		consumer.set_frames(..3);
		assert_eq!(
			consumer.read_frame().now_or_never().unwrap().unwrap().unwrap().payload[0],
			2
		);
		assert!(consumer.read_frame().now_or_never().unwrap().unwrap().is_none());
		consumer.set_frames(0..=3);
		assert_eq!(
			consumer.read_frame().now_or_never().unwrap().unwrap().unwrap().payload[0],
			3
		);
	}

	/// An exclusive cap at 0 is the empty range: no frame is delivered, and raising
	/// it re-offers the held frames.
	#[test]
	#[cfg(feature = "async")]
	fn end_at_zero_is_empty() {
		let mut producer = Info { sequence: 0 }.produce();
		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"x")).unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		consumer.set_frames(..0);
		assert!(
			consumer.read_frame().now_or_never().unwrap().unwrap().is_none(),
			"empty cap delivers nothing"
		);

		consumer.set_frames(..1);
		assert_eq!(
			consumer.read_frame().now_or_never().unwrap().unwrap().unwrap().payload,
			Bytes::from_static(b"x")
		);
	}

	/// Where the group begins is part of its shape, so it can't move once frames exist.
	#[test]
	fn start_at_rejected_after_a_frame() {
		let mut producer = Info { sequence: 0 }.produce();
		// Re-declaring before the first frame is fine; the shape isn't committed yet.
		producer.start_at(2).unwrap();
		producer.start_at(3).unwrap();

		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"a")).unwrap();
		assert!(matches!(producer.start_at(4), Err(Error::Closed)));
		assert_eq!(producer.frame_count(), 4, "the frame landed at index 3");

		// Finishing likewise settles the shape.
		let mut producer = Info { sequence: 1 }.produce();
		producer.finish().unwrap();
		assert!(matches!(producer.start_at(1), Err(Error::Closed)));
	}

	/// The start must leave room for at least one frame index.
	#[test]
	fn start_at_rejects_the_largest_index() {
		let mut producer = Info { sequence: 0 }.produce();
		assert!(matches!(
			producer.start_at(usize::MAX as u64),
			Err(Error::BoundsExceeded(_))
		));
	}

	/// The per-frame size cap (the group byte budget) is enforced before allocating.
	#[test]
	fn create_frame_rejects_oversized() {
		let producer = Info { sequence: 0 }.produce();
		let result = producer.create_frame(frame::Info {
			size: MAX_CACHE_BYTES + 1,
			timestamp: Some(Timestamp::ZERO),
		});
		assert!(matches!(result, Err(Error::FrameTooLarge)));
	}

	fn sized(size: u64) -> frame::Info {
		frame::Info {
			size,
			timestamp: Some(Timestamp::ZERO),
		}
	}

	/// A declared size costs the peer nothing, so the pool is charged as the bytes
	/// arrive. Charging the declaration would let a peer evict the cache without
	/// sending anything.
	#[test]
	fn frame_is_charged_as_written() {
		let pool = cache::Pool::unbounded();
		let cache = cache::Track::new(pool.clone(), Default::default(), kio::Weak::new());
		let producer = Producer::new(Info { sequence: 0 }, track::Info::timed(), cache);
		let before = pool.used();

		let observing = producer.handle();
		let mut frame = producer.create_frame(sized(MAX_CACHE_BYTES)).unwrap();
		assert_eq!(pool.used(), before, "the declared size was charged");

		frame.write_deferred(Bytes::from(vec![0u8; 100])).unwrap();
		assert_eq!(pool.used(), before, "charged before the write was published");
		frame.notify();
		assert_eq!(pool.used(), before + 100);
		assert_eq!(observing.state.read().cache, 100);
		// A reader skipping the group still misses the whole declared frame.
		assert_eq!(observing.state.read().content().bytes, MAX_CACHE_BYTES);

		// Aborting releases what was charged.
		frame.abort(Error::Cancel).unwrap();
		assert_eq!(observing.state.read().cache, 0);
		assert!(pool.used() <= before);
	}

	/// A writer names the group it fills.
	#[test]
	fn info_names_the_group() {
		let producer = Info { sequence: 7 }.produce();
		assert_eq!(producer.info(), Info { sequence: 7 });
	}

	/// An open frame still reads its group: which one it is, how many frames it holds,
	/// and whether anyone reads it. Finishing the frame hands the same writer back.
	#[test]
	#[cfg(feature = "async")]
	fn an_open_frame_reads_its_group() {
		let mut producer = Info { sequence: 3 }.produce();
		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"a")).unwrap();
		let mut consumer = producer.consume();

		let mut frame = producer.create_frame(sized(1)).unwrap();
		let group = frame.group();
		assert_eq!(group.info().sequence, 3);
		assert_eq!(group.frame_count(), 2, "the open frame counts");
		assert!(group.demand().is_used());
		assert!(group.poll_closed(&kio::Waiter::noop()).is_pending());

		frame.write(Bytes::from_static(b"b")).unwrap();
		let mut producer = frame.finish().unwrap();
		producer.finish().unwrap();
		let mut read = || consumer.read_frame().now_or_never().unwrap().unwrap();
		assert_eq!(read().unwrap().payload, Bytes::from_static(b"a"));
		assert_eq!(read().unwrap().payload, Bytes::from_static(b"b"));
		assert!(read().is_none());
	}

	#[test]
	fn refused_frame_aborts_with_its_error() {
		let producer = Info { sequence: 0 }.produce();
		let handle = producer.handle();
		assert!(matches!(
			producer.create_frame(sized(MAX_CACHE_BYTES + 1)),
			Err(Error::FrameTooLarge)
		));
		assert!(matches!(
			handle.poll_closed(&kio::Waiter::noop()),
			Poll::Ready(Error::FrameTooLarge)
		));
	}

	/// Completing a frame charges whatever it wrote since the last notify.
	#[test]
	fn frame_commit_charges_the_tail() {
		let pool = cache::Pool::unbounded();
		let cache = cache::Track::new(pool.clone(), Default::default(), kio::Weak::new());
		let producer = Producer::new(Info { sequence: 0 }, track::Info::timed(), cache);
		let before = pool.used();

		let observing = producer.handle();
		let mut frame = producer.create_frame(sized(300)).unwrap();
		frame.write(Bytes::from(vec![0u8; 100])).unwrap();
		frame.write_deferred(Bytes::from(vec![0u8; 200])).unwrap();
		let _producer = frame.finish().unwrap();
		assert_eq!(pool.used(), before + 300);
		assert_eq!(observing.state.read().cache, 300);
	}

	/// Frames within their origin's budget allocate their declared size up front; past
	/// it, a frame's buffer holds only what has arrived. A frame hands its share back
	/// once it ends.
	#[test]
	fn frame_budget_bounds_upfront_allocation() {
		let budget = frame::Budget::new(1024);
		let pool = cache::Pool::unbounded();
		let group = |sequence| {
			let cache = cache::Track::new(pool.clone(), budget.clone(), kio::Weak::new());
			Producer::new(Info { sequence }, track::Info::timed(), cache)
		};
		let a = group(0);
		let b = group(1);

		let mut first = a.create_frame(sized(1024)).unwrap();
		first.write(Bytes::from_static(b"x")).unwrap();
		assert_eq!(first.allocated(), 1024, "a frame within budget is allocated up front");

		let mut second = b.create_frame(sized(MAX_CACHE_BYTES)).unwrap();
		second.write(Bytes::from(vec![0u8; 100])).unwrap();
		assert_eq!(
			second.allocated(),
			100,
			"a frame past budget allocated its declared size"
		);
		second.abort(Error::Cancel).unwrap();

		first.write(Bytes::from(vec![0u8; 1023])).unwrap();
		let mut a = first.finish().unwrap();
		a.finish().unwrap();

		let a = group(2);
		let mut third = a.create_frame(sized(1024)).unwrap();
		third.write(Bytes::from_static(b"x")).unwrap();
		assert_eq!(third.allocated(), 1024, "the finished frame did not return its share");
	}

	/// A repetitive frame DEFLATE shrinks, distinct per index.
	fn verbose(index: usize) -> Bytes {
		Bytes::from(format!("{index}:{}", "moq".repeat(300)))
	}

	fn untimed(size: usize) -> frame::Info {
		frame::Info {
			size: size as u64,
			timestamp: None,
		}
	}

	fn compressed_group(pool: &cache::Pool) -> Producer {
		let cache = cache::Track::new(pool.clone(), Default::default(), kio::Weak::new());
		Producer::new(Info::from(0u64), track::Info::default().with_compress(true), cache)
	}

	fn read_plain(consumer: &mut Consumer) -> Result<Option<Bytes>> {
		match consumer.poll_read_frame(&kio::Waiter::noop()) {
			Poll::Ready(res) => res.map(|frame| frame.map(|frame| frame.payload)),
			Poll::Pending => panic!("the frame is ready"),
		}
	}

	/// A compressed group stores DEFLATE slices, hands plain readers the frames as written
	/// from one shared inflated copy, and hands egress the slices with their sizes.
	#[test]
	#[cfg(feature = "async")]
	fn compressed_group_stores_slices_and_inflates_once() {
		let pool = cache::Pool::unbounded();
		let mut group = compressed_group(&pool);
		group.write_frame(None, verbose(0)).unwrap();
		// A chunked frame is invisible until its last chunk, then compressed whole.
		let mut frame = group.create_frame(untimed(verbose(1).len())).unwrap();
		for chunk in verbose(1).chunks(100) {
			frame.write(Bytes::copy_from_slice(chunk)).unwrap();
		}
		group = frame.finish().unwrap();
		group.write_frame(None, verbose(2)).unwrap();
		group.finish().unwrap();

		let plain: usize = (0..3).map(|index| verbose(index).len()).sum();
		let stored = pool.used();
		let uncompressed = cache::Pool::unbounded();
		let mut reference = Producer::new(
			Info::from(0u64),
			track::Info::default(),
			cache::Track::new(uncompressed.clone(), Default::default(), kio::Weak::new()),
		);
		for index in 0..3 {
			reference.write_frame(None, verbose(index)).unwrap();
		}
		reference.finish().unwrap();
		assert!(
			stored + (plain as u64) / 2 < uncompressed.used(),
			"slices are stored: {stored} against {}",
			uncompressed.used()
		);

		let mut first = group.consume();
		let mut second = group.consume();
		for index in 0..3 {
			assert_eq!(
				read_plain(&mut first).unwrap().unwrap(),
				verbose(index),
				"frame {index}"
			);
		}
		let inflated = pool.used();
		assert!(inflated >= stored + plain as u64, "the inflated copy is billed");
		for index in 0..3 {
			assert_eq!(
				read_plain(&mut second).unwrap().unwrap(),
				verbose(index),
				"frame {index}"
			);
		}
		assert_eq!(pool.used(), inflated, "a second reader shares the inflated copy");
		assert!(read_plain(&mut first).unwrap().is_none());

		let mut raw = group.consume().compressed();
		let mut decoder = flate::Decoder::new();
		for index in 0..3 {
			let Poll::Ready(Ok(Some(object))) = raw.poll_read_object(&kio::Waiter::noop()) else {
				panic!("frame {index} is ready");
			};
			let size = object.inflated.expect("a stored slice carries its size");
			assert_eq!(size, verbose(index).len() as u64);
			let frame = decoder.frame(&object.frame.payload, size as usize).unwrap();
			assert_eq!(frame, verbose(index), "slice {index}");
		}

		drop((first, second, raw));
		drop(group);
		assert_eq!(pool.used(), 0, "the inflated copy is released with the group");
	}

	/// A compressed successor adopts a failed compressed copy's slices with their inflated
	/// sizes, so its plain readers inflate the group from its first frame; a plain head is
	/// not its form and stays out.
	#[test]
	#[cfg(feature = "async")]
	fn a_compressed_group_adopts_a_compressed_head() {
		let pool = cache::Pool::unbounded();
		let fail = |group: &mut Producer| {
			group.write_frame(None, verbose(0)).unwrap();
			group.write_frame(None, verbose(1)).unwrap();
			let handoff = group.consume().handoff();
			group.handle().abort(Error::Cancel).unwrap();
			handoff.head().expect("held for its resumers")
		};
		let successor = || {
			let mut group = compressed_group(&pool);
			group.start_at(2).unwrap();
			group
		};

		let plain = fail(&mut Producer::new(
			Info::from(0u64),
			track::Info::default(),
			cache::Track::new(pool.clone(), Default::default(), kio::Weak::new()),
		));
		let refused = successor();
		refused.handle.adopt_head(&plain);
		assert_eq!(refused.handle.state.read().offset, 2, "a plain head stays out");

		let head = fail(&mut compressed_group(&pool));
		let mut group = successor();
		group.handle.adopt_head(&head);
		group.finish().unwrap();
		let mut reader = group.consume();
		for index in 0..2 {
			assert_eq!(
				read_plain(&mut reader).unwrap().unwrap(),
				verbose(index),
				"frame {index}"
			);
		}
		assert!(read_plain(&mut reader).unwrap().is_none());
	}

	/// The slices a plain read up to local frame `last` takes under the group's guard.
	#[cfg(feature = "async")]
	fn take_slices(group: &Producer, last: usize) -> Inflate {
		let state = group.handle.state.read();
		state.inflate(state.flate.as_ref().unwrap(), last).unwrap()
	}

	/// Plain readers inflate with the group's guard released, into one shared copy: a reader
	/// that took the same slices skips what another inflated, the bytes count toward the
	/// group's eviction size and the inflating reader's stats once, and a reader that took
	/// its slices before a release bills nothing after it.
	#[test]
	#[cfg(feature = "async")]
	fn inflation_is_shared_and_stops_at_release() {
		let registry = stats::Registry::new(stats::Config::new());
		let meter = registry.publisher_meter("demo", &stats::Tier::default());
		let inflated = || {
			registry
				.snapshot()
				.traffic()
				.into_iter()
				.find(|(_, role, _)| *role == stats::Role::Publisher)
				.map_or(0, |(_, _, traffic)| traffic.inflated)
		};
		let plain: u64 = (0..3).map(|index| verbose(index).len() as u64).sum();
		let pool = cache::Pool::unbounded();
		let write = || {
			let mut group = compressed_group(&pool);
			for index in 0..3 {
				group.write_frame(None, verbose(index)).unwrap();
			}
			group.finish().unwrap();
			group
		};

		let group = write();
		let (stored, used) = (group.handle.cache_size(), pool.used());
		let (first, second) = (take_slices(&group, 2), take_slices(&group, 2));
		first.run(&meter);
		assert_eq!(
			group.handle.cache_size(),
			stored + plain,
			"the copy counts toward eviction once built"
		);
		assert_eq!(pool.used(), used + plain);
		second.run(&meter);
		assert_eq!(
			group.handle.cache_size(),
			stored + plain,
			"the second reader inflates nothing"
		);
		assert_eq!(second.get(1).unwrap(), verbose(1));
		assert_eq!(inflated(), plain, "each inflated byte counts once");

		let group = write();
		let late = take_slices(&group, 1);
		group.handle().abort(Error::Evicted).unwrap();
		let used = pool.used();
		late.run(&meter);
		assert!(matches!(late.get(0), Err(Error::Evicted)));
		assert_eq!(pool.used(), used, "a late inflater bills nothing past the release");
		assert_eq!(inflated(), plain);
	}

	/// A compressed group meters `bytes` as stored, whether a frame is written whole, in a
	/// batch, or in chunks.
	#[test]
	#[cfg(feature = "async")]
	fn compressed_writes_meter_the_stored_bytes() {
		let registry = stats::Registry::new(stats::Config::new());
		let metered = || {
			registry
				.snapshot()
				.traffic()
				.into_iter()
				.find(|(_, role, _)| *role == stats::Role::Publisher)
				.map_or(0, |(_, _, traffic)| traffic.bytes)
		};
		let pool = cache::Pool::unbounded();
		let meter = registry.publisher_meter("demo", &stats::Tier::default());
		let mut group = compressed_group(&pool).with_meter(meter);

		group.write_frame(None, verbose(0)).unwrap();
		let mut batch = frame::Buffer::<2>::new();
		for index in 1..3 {
			batch.push(Frame::new(None, verbose(index))).unwrap();
		}
		group.write_frames(&mut batch).unwrap();
		let mut frame = group.create_frame(untimed(verbose(3).len())).unwrap();
		for chunk in verbose(3).chunks(100) {
			frame.write(Bytes::copy_from_slice(chunk)).unwrap();
		}
		group = frame.finish().unwrap();
		group.finish().unwrap();

		let stored: u64 = (group.handle.state.read().frames.iter())
			.map(|frame| frame.payload.len() as u64)
			.sum();
		let plain: u64 = (0..4).map(|index| verbose(index).len() as u64).sum();
		assert!(stored < plain, "the frames compressed");
		assert_eq!(metered(), stored);
	}

	/// The codec state counts toward the pool while it lives: the encoder until the group
	/// finishes, and a reader's decoder until the last frame inflates.
	#[test]
	#[cfg(feature = "async")]
	fn codec_state_is_billed_while_held() {
		let pool = cache::Pool::unbounded();
		let mut group = compressed_group(&pool);
		group.write_frame(None, verbose(0)).unwrap();
		let stored = group.handle.state.read().charge.size();
		assert_eq!(group.handle.cache_size(), stored + flate::ENCODER_HEAP);
		group.write_frame(None, verbose(1)).unwrap();

		// The read inflates both frames ahead, but the open group may still get a third.
		let mut reader = group.consume();
		assert_eq!(read_plain(&mut reader).unwrap().unwrap(), verbose(0));
		let inflated = (verbose(0).len() + verbose(1).len()) as u64;
		let stored = group.handle.state.read().charge.size();
		let held = stored + flate::ENCODER_HEAP + flate::DECODER_HEAP + inflated;
		assert_eq!(group.handle.cache_size(), held, "an open group keeps both");

		group.finish().unwrap();
		assert_eq!(group.handle.cache_size(), stored + inflated, "finishing drops both");
		assert_eq!(read_plain(&mut reader).unwrap().unwrap(), verbose(1));

		// A finished group's decoder goes with the last frame it inflates.
		let mut group = compressed_group(&pool);
		group.write_frame(None, verbose(0)).unwrap();
		let mut reader = group.consume();
		assert_eq!(read_plain(&mut reader).unwrap().unwrap(), verbose(0));
		group.write_frame(None, verbose(1)).unwrap();
		group.finish().unwrap();
		let stored = group.handle.state.read().charge.size();
		assert_eq!(read_plain(&mut reader).unwrap().unwrap(), verbose(1));
		assert_eq!(group.handle.cache_size(), stored + inflated);

		// So does one whose reader took the last slice before the group finished.
		let mut group = compressed_group(&pool);
		group.write_frame(None, verbose(0)).unwrap();
		group.write_frame(None, verbose(1)).unwrap();
		let job = take_slices(&group, 1);
		group.finish().unwrap();
		job.run(&stats::Meter::default());
		let stored = group.handle.state.read().charge.size();
		assert_eq!(group.handle.cache_size(), stored + inflated, "the decoder is dropped");
	}

	/// A plain read of a compressed group that started past frame 0 lags on every read,
	/// never handing out the stored slices it cannot inflate.
	#[test]
	fn a_plain_read_past_frame_zero_lags() {
		let pool = cache::Pool::unbounded();
		let mut group = compressed_group(&pool);
		group.start_at(1).unwrap();
		group.write_frame(None, verbose(1)).unwrap();
		group.finish().unwrap();

		let mut consumer = group.consume();
		consumer.set_frames(1..);
		assert!(matches!(read_plain(&mut consumer), Err(Error::Lagged)));
		assert!(matches!(read_plain(&mut consumer), Err(Error::Lagged)));
	}

	/// A received slice that inflates to other than its declared size fails the read.
	#[test]
	fn a_received_size_mismatch_fails_the_read() {
		let pool = cache::Pool::unbounded();
		let group = compressed_group(&pool);
		let slice = flate::Encoder::new().frame(&verbose(0));
		let header = frame::Header {
			info: untimed(slice.len()),
			fields: None,
			inflated: Some(verbose(0).len() as u64 + 1),
		};
		let mut frame = group.create_object(header).unwrap();
		frame.write(slice).unwrap();
		let mut group = frame.finish().unwrap();
		group.finish().unwrap();

		assert!(matches!(read_plain(&mut group.consume()), Err(Error::WrongSize)));
	}

	/// A declared inflated size past the cache budget is refused before anything inflates,
	/// and only a compressed track takes one.
	#[test]
	fn an_inflated_size_is_checked_on_open() {
		let header = |inflated| frame::Header {
			info: untimed(1),
			fields: None,
			inflated: Some(inflated),
		};
		let pool = cache::Pool::unbounded();
		let res = compressed_group(&pool).create_object(header(MAX_CACHE_BYTES + 1));
		assert!(matches!(res, Err(Error::FrameTooLarge)));

		let res = Info::from(1u64).produce_untimed().create_object(header(1));
		assert!(matches!(res, Err(Error::ProtocolViolation)));
	}

	/// A budget that gives up exactly when the test says so.
	struct Switch(Arc<AtomicBool>);

	impl Expiry for Switch {
		fn is_expired(&self, _: Option<std::time::Duration>, _: &kio::Waiter) -> bool {
			self.0.load(Ordering::Relaxed)
		}

		fn for_track(&self, _: &track::Consumer) -> Arc<dyn Expiry> {
			Arc::new(Switch(self.0.clone()))
		}
	}

	/// A reader that took every frame of an unfinished group still fails with `Old` when
	/// the budget gives up: the producer never finished it, so a clean end would pass the
	/// prefix off as the whole group.
	#[test]
	fn a_drained_unfinished_group_expires_as_old() {
		let mut producer = Info { sequence: 0 }.produce();
		producer
			.write_frame(crate::Timestamp::ZERO, Bytes::from_static(b"a"))
			.unwrap();

		let expired = Arc::new(AtomicBool::new(false));
		let mut group = producer.consume().with_expiry(Arc::new(Switch(expired.clone())));
		let waiter = kio::Waiter::noop();
		assert!(matches!(group.poll_read_frame(&waiter), Poll::Ready(Ok(Some(_)))));
		assert!(group.poll_read_frame(&waiter).is_pending(), "parked at the tail");

		expired.store(true, Ordering::Relaxed);
		let mut next = group.clone();
		let mut finished = group.clone();
		assert!(matches!(group.poll_read_frame(&waiter), Poll::Ready(Err(Error::Old))));
		assert!(
			matches!(group.poll_read_frame(&waiter), Poll::Ready(Err(Error::Old))),
			"and stays failed when probed again"
		);
		assert!(matches!(next.poll_next_frame(&waiter), Poll::Ready(Err(Error::Old))));
		assert!(matches!(finished.poll_finished(&waiter), Poll::Ready(Err(Error::Old))));
	}
}
