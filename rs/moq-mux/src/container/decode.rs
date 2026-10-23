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
/// That settling, and a delay that only grows, depend on everything read before, so two
/// exporters that joined or lost frames at different points can clock the same frame apart.
/// [`Self::pop_counted`] depends on the frames held alone, for exporters that must agree.
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
		Some((item, self.after_last(dts)))
	}

	/// The next frame and its DTS, once more than `depth` frames are held, or at once when
	/// `flush`. `depth` is the most frames the sequence header lets precede one in decode order
	/// and follow it in output order, so no frame still to come is presented before all of
	/// them. The delay is `delay` throughout, and a DTS never passes its PTS.
	pub(crate) fn pop_counted(&mut self, depth: usize, delay: u64, flush: bool) -> Option<(T, u64)> {
		if !flush && self.held.len() <= depth {
			return None;
		}
		let (item, pts) = self.held.pop_front()?;
		let slot = self.slots.remove(0);
		let dts = slot.saturating_sub(delay).min(pts);
		Some((item, self.after_last(dts)))
	}

	/// Forget every held frame, as if it had never been read.
	pub(crate) fn discard(&mut self) {
		self.held.clear();
		self.slots.clear();
	}

	fn after_last(&mut self, dts: u64) -> u64 {
		let dts = match self.last {
			Some(last) if dts <= last => last + 1,
			_ => dts,
		};
		self.last = Some(dts);
		dts
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A B-pyramid (`max_num_reorder_frames` 2) in decode order, in 40 ms frames.
	const PYRAMID: [u64; 9] = [0, 4, 2, 1, 3, 8, 6, 5, 7];
	const FRAME: u64 = 3_600;

	/// Every frame of `read` with its DTS, popped as each is read: counted at a depth of 2, or
	/// by the timing rule with the lookahead that depth implies.
	fn clocked(read: &[u64], counted: bool) -> Vec<(u64, u64)> {
		let mut clock = DecodeClock::default();
		let mut out = Vec::new();
		let pop = |clock: &mut DecodeClock<u64>, flush| match counted {
			true => clock.pop_counted(2, 2 * FRAME, flush),
			false => clock.pop(4 * FRAME, 2 * FRAME, flush),
		};
		for &pts in read {
			clock.push(pts, pts);
			out.extend(std::iter::from_fn(|| pop(&mut clock, false)));
		}
		out.extend(std::iter::from_fn(|| pop(&mut clock, true)));
		out
	}

	#[test]
	fn counted_slots_are_the_encoders() {
		let out = clocked(&PYRAMID.map(|i| (i + 2) * FRAME), true);
		let dts: Vec<u64> = out.iter().map(|&(_, dts)| dts / FRAME).collect();
		assert_eq!(dts, (0..9).collect::<Vec<_>>());
	}

	/// Two references spaced wider than the lookahead a depth of 2 implies, then the frames
	/// presented between them: a conforming depth of 2, which the timing rule settles before
	/// those frames arrive, and the count waits for.
	#[test]
	fn references_far_apart_keep_their_slots() {
		let read = [0, 6, 10, 1, 2, 3, 4, 5, 7, 8, 9].map(|i| (i + 2) * FRAME);
		let out = clocked(&read, true);
		let dts: Vec<u64> = out.iter().map(|&(_, dts)| dts / FRAME).collect();
		assert_eq!(dts, (0..11).collect::<Vec<_>>());
		assert_ne!(clocked(&read, false), out);
	}

	/// A group cut short after a frame presented below one it never received: its held frames
	/// take slots from an incomplete set, but the next group keeps the DTS it would have had,
	/// where the timing rule raises its delay for good.
	#[test]
	fn a_cut_group_leaves_the_next_alone() {
		let first = PYRAMID.map(|i| (i + 2) * FRAME);
		let next = PYRAMID.map(|i| (i + 20) * FRAME);
		let full: Vec<u64> = first.into_iter().chain(next).collect();
		let cut: Vec<u64> = first[..8].iter().copied().chain(next).collect();
		let tail = |out: Vec<(u64, u64)>| out[out.len() - next.len()..].to_vec();
		assert_eq!(tail(clocked(&full, true)), tail(clocked(&cut, true)));
		assert_ne!(tail(clocked(&full, false)), tail(clocked(&cut, false)));
	}

	/// Frames discarded at a cut leave every frame the clock gives out at the DTS it has with
	/// nothing lost.
	#[test]
	fn a_discard_keeps_the_frames_given_out() {
		let first = PYRAMID.map(|i| (i + 2) * FRAME);
		let next = PYRAMID.map(|i| (i + 20) * FRAME);
		let full: Vec<u64> = first.into_iter().chain(next).collect();
		let full = clocked(&full, true);

		let mut clock = DecodeClock::default();
		let mut out = Vec::new();
		for &pts in &first[..8] {
			clock.push(pts, pts);
			out.extend(std::iter::from_fn(|| clock.pop_counted(2, 2 * FRAME, false)));
		}
		clock.discard();
		for pts in next {
			clock.push(pts, pts);
			out.extend(std::iter::from_fn(|| clock.pop_counted(2, 2 * FRAME, false)));
		}
		out.extend(std::iter::from_fn(|| clock.pop_counted(2, 2 * FRAME, true)));
		assert!(out.iter().all(|frame| full.contains(frame)), "{out:?}");
		assert_eq!(out.len(), 6 + next.len());
	}
}
