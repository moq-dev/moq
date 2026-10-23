use std::collections::VecDeque;

/// The largest supported reorder span in 90 kHz ticks.
pub(crate) const MAX_REORDER: u64 = 2 * 90_000;

/// A video rendition's decode clock, authored from presentation timestamps.
///
/// Declared-depth clocks release the n-th group frame at its (n - depth)-th
/// presentation slot. The first depth slots precede the group's keyframe by one
/// 90 kHz tick each, so DTS may be negative. Draining after every push bounds the
/// held payloads by the depth, independently of frame spacing and group length.
/// Group completion flushes the remaining slots; a cut discards them and resets
/// the next group's origin. Invalid slots fail rather than move the timeline.
///
/// Undeclared legacy renditions still settle slots by timestamp lookahead, or
/// use a caller-supplied counted delay. Those paths retain their timing history.
/// Ticks are continuous 90 kHz; a container applies its wire representation later.
pub(crate) struct DecodeClock<T> {
	fixed: Option<Fixed>,
	/// Frames read but not yet given a DTS, in decode order, with their PTS.
	held: VecDeque<(T, u64)>,
	/// Presentation slots not yet assigned to a frame, in ascending order.
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
			fixed: None,
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
	/// Author group-local decode slots at this rendition's fixed reorder depth.
	pub(crate) fn new(reorder: usize) -> crate::Result<Self> {
		if reorder > crate::codec::video::Reorder::MAX_DEPTH as usize {
			return Err(ClockError::Depth(reorder).into());
		}
		Ok(Self {
			fixed: Some(Fixed {
				depth: reorder,
				leading: reorder,
				first: None,
				last: None,
				highest: Vec::new(),
			}),
			..Self::default()
		})
	}

	pub(crate) fn is_fixed(&self) -> bool {
		self.fixed.is_some()
	}

	#[cfg(test)]
	pub(crate) fn len(&self) -> usize {
		self.held.len()
	}

	/// Hold the next frame in decode order, presented at `pts`.
	pub(crate) fn push(&mut self, item: T, pts: u64) -> crate::Result<()> {
		if let Some(fixed) = &mut self.fixed {
			if self.held.len() > fixed.depth {
				return Err(ClockError::Undrained.into());
			}
			let at = fixed.highest.partition_point(|&prior| prior <= pts);
			if fixed.highest.len() - at > fixed.depth {
				return Err(ClockError::Reordered(fixed.depth).into());
			}
			fixed.highest.insert(at, pts);
			if fixed.highest.len() > fixed.depth + 1 {
				fixed.highest.remove(0);
			}
			fixed.first.get_or_insert(pts);
		} else {
			if let Some(high) = self.high
				&& pts < high
			{
				self.reach = self.reach.max(high - pts + 1).min(MAX_REORDER);
			}
			self.high = Some(self.high.map_or(pts, |high| high.max(pts)));
		}
		let at = self.slots.partition_point(|&slot| slot <= pts);
		self.slots.insert(at, pts);
		self.held.push_back((item, pts));
		Ok(())
	}

	/// Release a frame once its declared lookahead is present, or the group is complete.
	pub(crate) fn pop(&mut self, flush: bool) -> crate::Result<Option<(T, i128)>> {
		let fixed = self.fixed.as_mut().ok_or(ClockError::Undeclared)?;
		if self.held.is_empty() || (!flush && self.held.len() <= fixed.depth) {
			return Ok(None);
		}
		let dts = match fixed.leading {
			0 => i128::from(self.slots.remove(0)),
			leading => {
				fixed.leading -= 1;
				i128::from(fixed.first.expect("a held frame supplies the origin")) - leading as i128
			}
		};
		let (item, pts) = self.held.pop_front().expect("the held queue is nonempty");
		if dts > i128::from(pts) || fixed.last.is_some_and(|last| dts <= last) {
			return Err(ClockError::Presentation.into());
		}
		fixed.last = Some(dts);
		Ok(Some((item, dts)))
	}

	/// The next frame and its DTS, once its slot is settled `lookahead` past (the catalog
	/// `jitter`), or at once when `flush`. `floor` is the least reorder delay, in ticks.
	pub(crate) fn settled(&mut self, lookahead: u64, floor: u64, flush: bool) -> Option<(T, u64)> {
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
		if let Some(fixed) = &mut self.fixed {
			fixed.leading = fixed.depth;
			fixed.first = None;
			fixed.last = None;
			fixed.highest.clear();
		}
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

struct Fixed {
	depth: usize,
	leading: usize,
	first: Option<u64>,
	last: Option<i128>,
	// Only the largest depth + 1 earlier PTS are needed to detect a deeper reorder.
	highest: Vec<u64>,
}

#[derive(Debug, thiserror::Error)]
enum ClockError {
	#[error("reorder depth {0} exceeds the supported codec depth")]
	Depth(usize),
	#[error("video reorders past its declared depth {0}")]
	Reordered(usize),
	#[error("video presentation slots cannot author increasing DTS at or before PTS")]
	Presentation,
	#[error("the decode clock must be drained before reading another frame")]
	Undrained,
	#[error("the decode clock has no fixed reorder declaration")]
	Undeclared,
}

impl From<ClockError> for crate::Error {
	fn from(err: ClockError) -> Self {
		anyhow::Error::new(err).into()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn fixed_slots(read: &[u64]) -> Vec<i128> {
		let mut clock = DecodeClock::new(2).unwrap();
		let mut out = Vec::new();
		for &pts in read {
			clock.push((), pts).unwrap();
			while let Some(((), dts)) = clock.pop(false).unwrap() {
				out.push(dts);
			}
			assert!(clock.held.len() <= 2);
			assert!(clock.slots.len() <= 4);
		}
		while let Some(((), dts)) = clock.pop(true).unwrap() {
			out.push(dts);
		}
		out
	}

	#[test]
	fn fixed_slots_shift_by_count_and_start_before_zero() {
		let read = PYRAMID.map(|i| i * FRAME);
		let want: Vec<i128> = [-2, -1].into_iter().chain((0..7).map(|i| i * FRAME as i128)).collect();
		assert_eq!(fixed_slots(&read), want);
		let far = [0, 6, 10, 1, 2, 3, 4, 5, 7, 8, 9].map(|i| i * FRAME);
		let want: Vec<i128> = [-2, -1].into_iter().chain((0..9).map(|i| i * FRAME as i128)).collect();
		assert_eq!(fixed_slots(&far), want);
	}

	#[test]
	fn a_fixed_group_keeps_no_previous_groups_history() {
		let next = PYRAMID.map(|i| (i + 20) * FRAME);
		let want = fixed_slots(&next);
		for count in 0..PYRAMID.len() {
			let mut clock = DecodeClock::new(2).unwrap();
			for pts in PYRAMID[..count].iter().map(|i| (i + 2) * FRAME) {
				clock.push((), pts).unwrap();
				while clock.pop(false).unwrap().is_some() {}
			}
			clock.discard();
			let mut out = Vec::new();
			for pts in next {
				clock.push((), pts).unwrap();
				while let Some(((), dts)) = clock.pop(false).unwrap() {
					out.push(dts);
				}
			}
			while let Some(((), dts)) = clock.pop(true).unwrap() {
				out.push(dts);
			}
			assert_eq!(out, want, "lost prefix of {count} frames");
		}
	}

	#[test]
	fn malformed_presentation_is_refused_without_retaining_its_payload() {
		for backwards in [false, true] {
			for count in [16, 128, 1024] {
				for size in [256, 16_384] {
					let mut clock = DecodeClock::new(2).unwrap();
					let mut refused = false;
					for index in 0..count {
						let pts = 90_000 - if backwards { index } else { 0 };
						if clock.push(vec![0u8; size], pts).is_err() || clock.pop(false).is_err() {
							refused = true;
							break;
						}
						assert!(clock.held.len() <= 2);
						assert!(clock.slots.len() <= 4);
					}
					assert!(refused);
					assert!(clock.held.len() <= 2);
				}
			}
		}
	}

	#[test]
	fn a_zero_depth_has_no_lookahead_or_decode_delay() {
		let mut clock = DecodeClock::new(0).unwrap();
		for pts in [0, 1, 90_000, u64::MAX] {
			clock.push(pts, pts).unwrap();
			assert_eq!(clock.pop(false).unwrap(), Some((pts, i128::from(pts))));
			assert!(clock.held.is_empty());
		}
		assert!(DecodeClock::<()>::new(17).is_err());
	}

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
			false => clock.settled(4 * FRAME, 2 * FRAME, flush),
		};
		for &pts in read {
			clock.push(pts, pts).unwrap();
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
			clock.push(pts, pts).unwrap();
			out.extend(std::iter::from_fn(|| clock.pop_counted(2, 2 * FRAME, false)));
		}
		clock.discard();
		for pts in next {
			clock.push(pts, pts).unwrap();
			out.extend(std::iter::from_fn(|| clock.pop_counted(2, 2 * FRAME, false)));
		}
		out.extend(std::iter::from_fn(|| clock.pop_counted(2, 2 * FRAME, true)));
		assert!(out.iter().all(|frame| full.contains(frame)), "{out:?}");
		assert_eq!(out.len(), 6 + next.len());
	}
}
