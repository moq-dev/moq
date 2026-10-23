use std::collections::VecDeque;

/// The largest supported reorder span in 90 kHz ticks.
pub(crate) const MAX_REORDER: u64 = 2 * 90_000;

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
/// Ticks are continuous (unwrapped) 90 kHz, so the clock never wraps mid-stream; the 33-bit
/// wire wrap happens once at emission.
pub(crate) struct DecodeClock<T> {
	/// Frames read but not yet given a DTS, in decode order, with their PTS.
	held: VecDeque<(T, u64)>,
	/// The held frames' PTS, ascending: the slots they decode in, in turn.
	slots: Vec<u64>,
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
			slots: Vec::new(),
			high: None,
			reach: 0,
			delay: 0,
			last: None,
		}
	}
}

impl<T> DecodeClock<T> {
	/// Hold the next frame in decode order, presented at `pts`.
	pub(crate) fn push(&mut self, item: T, pts: u64) {
		if let Some(high) = self.high
			&& pts < high
		{
			self.reach = self.reach.max(high - pts + 1).min(MAX_REORDER);
		}
		self.high = Some(self.high.map_or(pts, |high| high.max(pts)));
		let at = self.slots.partition_point(|&slot| slot <= pts);
		self.slots.insert(at, pts);
		self.held.push_back((item, pts));
	}

	/// The next frame and its DTS, once its slot is settled `lookahead` past (the catalog
	/// `jitter`), or at once when `flush`. `floor` is the least reorder delay, in ticks.
	pub(crate) fn pop(&mut self, lookahead: u64, floor: u64, flush: bool) -> Option<(T, u64)> {
		let high = self.high?;
		let lookahead = lookahead.max(self.reach);
		let settled = |slot: u64| flush || high >= slot.saturating_add(lookahead);
		if !settled(*self.slots.first()?) {
			return None;
		}
		// Every settled frame bounds the delay, so one decoding later never steps it back.
		let mut delay = self.delay.max(floor);
		for (&slot, (_, pts)) in self.slots.iter().zip(&self.held) {
			if !settled(slot) {
				break;
			}
			delay = delay.max(slot.saturating_sub(*pts));
		}
		self.delay = delay.min(MAX_REORDER);

		let slot = self.slots.remove(0);
		let (item, _) = self.held.pop_front()?;
		let dts = slot.saturating_sub(self.delay);
		let dts = match self.last {
			Some(last) if dts <= last => last + 1,
			_ => dts,
		};
		self.last = Some(dts);
		Some((item, dts))
	}
}
