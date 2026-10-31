//! The bench `decode_clock` compiles this file on its own, so it names nothing else in the crate.

use std::collections::VecDeque;
use std::time::Duration;

use moq_clock::Instant;

/// The largest supported reorder span in 90 kHz ticks.
pub(crate) const MAX_REORDER: u64 = 2 * 90_000;

/// The most frames any supported codec reorders: H.264's and H.265's 16-picture DPB.
const MAX_REORDER_FRAMES: usize = 16;

/// Which frames [`DecodeClock::pop`] gives out.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Settle {
	/// Those whose slot the highest PTS read settled.
	Due,
	/// Those, and the frame held longest, which is overdue ([`DecodeClock::settle`]).
	Front,
	/// Every frame held, as nothing still to come is presented before them.
	All,
}

/// A video rendition's decode clock, authored from presentation timestamps.
///
/// [`crate::container::Frame`] carries only a presentation timestamp and frames reach the muxer in decode order,
/// so a B-frame stream arrives with non-monotonic PTS and no decode time. An encoder decodes
/// the n-th frame in decode order at the n-th presentation time, a fixed reorder delay early,
/// so that is the DTS authored here: with decode-order PTS 0, 120, 40, 80 and a delay of one
/// 40 ms frame, the frames decode at -40, 0, 40, 80. It keeps the encoder's own spacing, a
/// frame's or a field's, through any switch between frame and field coding, and the delay is
/// a time, the least that keeps every DTS at or before its PTS (or the SPS's declared depth,
/// when that is more). On a source muxed that way this is its DTS, frame for frame.
///
/// A frame's slot is settled once no frame still to come can be presented before it. A later
/// frame decodes after this one, and is presented at most the catalog `jitter` after it
/// decodes, so the slot is settled once the highest PTS read is that far past it. Frames wait
/// here until then, as long as the reordering spans and well inside the jitter buffer's delay.
/// Without a `jitter`, the SPS's depth bounds it, and so does how far a frame was presented
/// below the highest PTS read before it. A frame that still comes in below a settled slot
/// (reordering deeper than all of those) is nudged one tick past the last DTS, so the clock
/// stays strictly increasing.
///
/// A source whose PTS stops advancing would hold every frame after it here, so a frame that
/// has waited longer than its lookahead plus the export's delay is overdue, and settled at
/// once ([`Self::settle`]): a live source's PTS keeps pace with its arrival, so the frames
/// held stay within what the source sends in that time, like the jitter buffer's. A burst read
/// at once never makes a frame overdue, so the frame held longest is also settled once the clock
/// holds the export's byte budget and more frames than any codec reorders: no frame still to
/// come can then belong before it, however large one frame is.
///
/// That settling, and a delay that only grows, depend on everything read before, so two
/// exporters that joined or lost frames at different points can clock the same frame apart.
/// A rendition that declares its catalog `reorder` gets a [`DeclaredClock`] instead.
///
/// Ticks are continuous (unwrapped) 90 kHz, so the clock never wraps mid-stream; the 33-bit
/// wire wrap happens once at emission.
pub(crate) struct DecodeClock<T> {
	/// Frames read but not yet given a DTS, in decode order, with their PTS, arrival and cost.
	held: VecDeque<(T, u64, Instant, usize)>,
	/// What the held frames cost: their payloads and their own footprint.
	bytes: usize,
	/// The held frames' PTS, ascending: the slots they decode in, in turn.
	slots: VecDeque<u64>,
	/// How many leading slots, paired with the held frame of the same rank, already bound the
	/// delay, so draining a backlog scans each pair once.
	bounded: usize,
	/// The highest PTS read.
	high: Option<u64>,
	/// The furthest a frame was presented below the highest PTS read before it, plus a tick.
	reach: u64,
	/// The reorder delay in effect. It never shrinks: that would step the clock back.
	delay: u64,
	/// The last DTS handed out.
	last: Option<u64>,
}

impl<T> Default for DecodeClock<T> {
	fn default() -> Self {
		Self {
			held: VecDeque::new(),
			bytes: 0,
			slots: VecDeque::new(),
			bounded: 0,
			high: None,
			reach: 0,
			delay: 0,
			last: None,
		}
	}
}

impl<T> DecodeClock<T> {
	/// Hold the next frame in decode order, presented at `pts`, which `arrived` when read and
	/// carries `bytes` of payload.
	pub(crate) fn push(&mut self, item: T, pts: u64, arrived: Instant, bytes: usize) {
		if let Some(high) = self.high
			&& pts < high
		{
			self.reach = self.reach.max(high - pts + 1).min(MAX_REORDER);
		}
		self.high = Some(self.high.map_or(pts, |high| high.max(pts)));
		let at = self.slots.partition_point(|&slot| slot <= pts);
		self.slots.insert(at, pts);
		self.bounded = self.bounded.min(at);
		let cost = bytes + std::mem::size_of::<(T, u64, Instant, usize)>();
		self.bytes += cost;
		self.held.push_back((item, pts, arrived, cost));
	}

	/// The next frame and its DTS, once its slot is settled `lookahead` past (the catalog
	/// `jitter`), or at once as `settle` says. `floor` is the least reorder delay, in ticks.
	pub(crate) fn pop(&mut self, lookahead: u64, floor: u64, settle: Settle) -> Option<(T, u64)> {
		let high = self.high?;
		let lookahead = self.lookahead(lookahead);
		let front = *self.slots.front()?;
		let settled = |slot: u64| match settle {
			Settle::All => true,
			Settle::Front if slot <= front => true,
			_ => high >= slot.saturating_add(lookahead),
		};
		if !settled(front) {
			return None;
		}
		// Every settled frame bounds the delay, so one decoding later never steps it back.
		let mut delay = self.delay.max(floor);
		let mut bounded = self.bounded;
		for (&slot, (_, pts, ..)) in self.slots.range(bounded..).zip(self.held.range(bounded..)) {
			if !settled(slot) {
				break;
			}
			delay = delay.max(slot.saturating_sub(*pts));
			bounded += 1;
		}
		self.delay = delay.min(MAX_REORDER);
		self.bounded = bounded - 1;

		let slot = self.slots.pop_front()?;
		let (item, .., cost) = self.held.pop_front()?;
		self.bytes -= cost;
		let dts = slot.saturating_sub(self.delay);
		let dts = match self.last {
			Some(last) if dts <= last => last + 1,
			_ => dts,
		};
		self.last = Some(dts);
		Some((item, dts))
	}

	/// What [`Self::pop`] settles short of a flush: [`Settle::Front`] once the frame held
	/// longest is overdue, having waited, by the `latest` arrival read, longer than `lookahead`
	/// plus the export's `delay`, else [`Settle::Due`]. Once the clock holds `max_bytes` in more
	/// frames than any codec reorders, [`Settle::All`]: the frame held longest goes out, and the
	/// delay counts every frame held, as the B-frames already read may be what sets it.
	pub(crate) fn settle(&self, lookahead: u64, latest: Instant, delay: Duration, max_bytes: usize) -> Settle {
		let Some((_, _, arrived, _)) = self.held.front() else {
			return Settle::Due;
		};
		if self.bytes >= max_bytes && self.held.len() > MAX_REORDER_FRAMES {
			return Settle::All;
		}
		let lookahead = Duration::from_nanos(self.lookahead(lookahead) * 1_000_000_000 / 90_000);
		match latest.saturating_duration_since(*arrived) > lookahead + delay {
			true => Settle::Front,
			false => Settle::Due,
		}
	}

	/// How many frames are held.
	#[cfg(test)]
	pub(crate) fn len(&self) -> usize {
		self.held.len()
	}

	/// How far past a slot the highest PTS must be to settle it, under `lookahead`.
	fn lookahead(&self, lookahead: u64) -> u64 {
		lookahead.max(self.reach).min(MAX_REORDER)
	}
}
