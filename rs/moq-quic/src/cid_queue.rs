use std::ops::Range;

use crate::{ConnectionId, ResetToken, frame::NewConnectionId};

/// DataType stored in CidQueue buffer
type CidData = (ConnectionId, Option<ResetToken>);

/// Sliding window of active Connection IDs
///
/// May contain gaps due to packet loss or reordering
#[derive(Debug)]
pub(crate) struct CidQueue {
	/// Ring buffer indexed by `self.cursor`
	buffer: [Option<CidData>; Self::LEN],
	/// Index at which circular buffer addressing is based
	cursor: usize,
	/// Sequence number of `self.buffer[cursor]`
	///
	/// The sequence number of the active CID; must be the smallest among CIDs in `buffer`.
	offset: u64,
}

impl CidQueue {
	pub(crate) fn new(cid: ConnectionId) -> Self {
		let mut buffer = [None; Self::LEN];
		buffer[0] = Some((cid, None));
		Self {
			buffer,
			cursor: 0,
			offset: 0,
		}
	}

	/// Handle a `NEW_CONNECTION_ID` frame
	///
	/// Fails when its sequence number is past the window the connection ID limit allows.
	pub(crate) fn insert(&mut self, cid: NewConnectionId) -> Result<Inserted, ExceedsLimit> {
		// Position of new CID wrt. the current active CID
		let Some(index) = cid.sequence.checked_sub(self.offset) else {
			return Ok(Inserted::Stale);
		};

		let retired_count = cid.retire_prior_to.saturating_sub(self.offset);
		if index >= Self::LEN as u64 + retired_count {
			return Err(ExceedsLimit);
		}

		// Discard retired CIDs, if any
		for i in 0..(retired_count.min(Self::LEN as u64) as usize) {
			self.buffer[(self.cursor + i) % Self::LEN] = None;
		}

		// Record the new CID
		let index = ((self.cursor as u64 + index) % Self::LEN as u64) as usize;
		self.buffer[index] = Some((cid.id, Some(cid.reset_token)));

		if retired_count == 0 {
			return Ok(Inserted::Queued);
		}

		// The active CID was retired. Find the first known CID with sequence number of at least
		// retire_prior_to, and inform the caller that all prior CIDs have been retired, and of
		// the new CID's reset token.
		self.cursor = ((self.cursor as u64 + retired_count) % Self::LEN as u64) as usize;
		let (i, (_, token)) = self
			.iter()
			.next()
			.expect("it is impossible to retire a CID without supplying a new one");
		self.cursor = (self.cursor + i) % Self::LEN;
		let orig_offset = self.offset;
		self.offset = cid.retire_prior_to + i as u64;
		// We don't immediately retire CIDs in the range (orig_offset +
		// Self::LEN)..self.offset. These are CIDs that we haven't yet received from a
		// NEW_CONNECTION_ID frame, since having previously received them would violate the
		// connection ID limit we specified based on Self::LEN. If we do receive a such a frame
		// in the future, e.g. due to reordering, we'll retire it then. This ensures we can't be
		// made to buffer an arbitrarily large number of RETIRE_CONNECTION_ID frames.
		Ok(Inserted::Advanced {
			retired: orig_offset..self.offset.min(orig_offset + Self::LEN as u64),
			reset_token: token.expect("non-initial CID missing reset token"),
		})
	}

	/// Switch to next active CID if possible, return
	/// 1) the corresponding ResetToken and 2) a non-empty range preceding it to retire
	pub(crate) fn next(&mut self) -> Option<(ResetToken, Range<u64>)> {
		let (i, cid_data) = self.iter().nth(1)?;
		self.buffer[self.cursor] = None;

		let orig_offset = self.offset;
		self.offset += i as u64;
		self.cursor = (self.cursor + i) % Self::LEN;
		Some((cid_data.1.unwrap(), orig_offset..self.offset))
	}

	/// Switch to the CID right after the active one, if known, returning its reset token and
	/// the retired active sequence number
	pub(crate) fn next_in_order(&mut self) -> Option<(ResetToken, Range<u64>)> {
		let next = (self.cursor + 1) % Self::LEN;
		let (_, token) = self.buffer[next]?;
		self.buffer[self.cursor] = None;
		self.cursor = next;
		self.offset += 1;
		Some((
			token.expect("non-initial CID missing reset token"),
			self.offset - 1..self.offset,
		))
	}

	/// Iterate CIDs in CidQueue that are not `None`, including the active CID
	fn iter(&self) -> impl Iterator<Item = (usize, CidData)> + '_ {
		(0..Self::LEN).filter_map(move |step| {
			let index = (self.cursor + step) % Self::LEN;
			self.buffer[index].map(|cid_data| (step, cid_data))
		})
	}

	/// Replace the initial CID
	pub(crate) fn update_initial_cid(&mut self, cid: ConnectionId) {
		debug_assert_eq!(self.offset, 0);
		self.buffer[self.cursor] = Some((cid, None));
	}

	/// Return active remote CID itself
	pub(crate) fn active(&self) -> ConnectionId {
		self.buffer[self.cursor].unwrap().0
	}

	/// Return the sequence number of active remote CID
	pub(crate) fn active_seq(&self) -> u64 {
		self.offset
	}

	pub(crate) const LEN: usize = 5;
}

/// What a `NEW_CONNECTION_ID` frame did, from [`CidQueue::insert`]
#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum Inserted {
	/// Queued, or already known, with the active CID unchanged
	Queued,
	/// It retired the active CID: retire the non-empty range of sequence numbers, and switch to
	/// the reset token of the new active CID
	Advanced {
		retired: Range<u64>,
		reset_token: ResetToken,
	},
	/// The CID was already retired
	Stale,
}

/// A `NEW_CONNECTION_ID` sequence number past the leading edge of the window
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) struct ExceedsLimit;

#[cfg(test)]
mod tests {
	use super::*;

	fn cid(sequence: u64, retire_prior_to: u64) -> NewConnectionId {
		NewConnectionId {
			sequence,
			id: ConnectionId::new(&[0xAB; 8]),
			reset_token: ResetToken::from([0xCD; crate::RESET_TOKEN_SIZE]),
			retire_prior_to,
		}
	}

	/// The sequence numbers an insert retired.
	fn retired(inserted: Result<Inserted, ExceedsLimit>) -> Range<u64> {
		match inserted {
			Ok(Inserted::Advanced { retired, .. }) => retired,
			other => panic!("retired nothing: {other:?}"),
		}
	}

	fn initial_cid() -> ConnectionId {
		ConnectionId::new(&[0xFF; 8])
	}

	#[test]
	fn next_dense() {
		let mut q = CidQueue::new(initial_cid());
		assert!(q.next().is_none());
		assert!(q.next().is_none());

		for i in 1..CidQueue::LEN as u64 {
			q.insert(cid(i, 0)).unwrap();
		}
		for i in 1..CidQueue::LEN as u64 {
			let (_, retire) = q.next().unwrap();
			assert_eq!(q.active_seq(), i);
			assert_eq!(retire.end - retire.start, 1);
		}
		assert!(q.next().is_none());
	}
	#[test]
	fn next_sparse() {
		let mut q = CidQueue::new(initial_cid());
		let seqs = (1..CidQueue::LEN as u64).filter(|x| x % 2 == 0);
		for i in seqs.clone() {
			q.insert(cid(i, 0)).unwrap();
		}
		for i in seqs {
			let (_, retire) = q.next().unwrap();
			dbg!(&retire);
			assert_eq!(q.active_seq(), i);
			assert_eq!(retire, (q.active_seq().saturating_sub(2))..q.active_seq());
		}
		assert!(q.next().is_none());
	}

	#[test]
	fn next_in_order_waits_for_the_gap() {
		let mut q = CidQueue::new(initial_cid());
		q.insert(cid(4, 0)).unwrap();
		q.insert(cid(2, 0)).unwrap();
		assert!(q.next_in_order().is_none(), "sequence 1 is still in flight");
		assert_eq!(q.active_seq(), 0);

		q.insert(cid(1, 0)).unwrap();
		assert_eq!(q.next_in_order().unwrap().1, 0..1);
		assert_eq!(q.active_seq(), 1);
		assert_eq!(q.next_in_order().unwrap().1, 1..2);
		assert!(q.next_in_order().is_none(), "sequence 3 is still in flight");

		// The CIDs that arrived early are still there to use.
		q.insert(cid(3, 0)).unwrap();
		assert_eq!(q.next_in_order().unwrap().1, 2..3);
		assert_eq!(q.next_in_order().unwrap().1, 3..4);
		assert_eq!(q.active_seq(), 4);
		assert!(q.next_in_order().is_none());
	}

	#[test]
	fn wrap() {
		let mut q = CidQueue::new(initial_cid());

		for i in 1..CidQueue::LEN as u64 {
			q.insert(cid(i, 0)).unwrap();
		}
		for _ in 1..(CidQueue::LEN as u64 - 1) {
			q.next().unwrap();
		}
		for i in CidQueue::LEN as u64..(CidQueue::LEN as u64 + 3) {
			q.insert(cid(i, 0)).unwrap();
		}
		for i in (CidQueue::LEN as u64 - 1)..(CidQueue::LEN as u64 + 3) {
			q.next().unwrap();
			assert_eq!(q.active_seq(), i);
		}
		assert!(q.next().is_none());
	}

	#[test]
	fn retire_dense() {
		let mut q = CidQueue::new(initial_cid());

		for i in 1..CidQueue::LEN as u64 {
			q.insert(cid(i, 0)).unwrap();
		}
		assert_eq!(q.active_seq(), 0);

		assert_eq!(retired(q.insert(cid(4, 2))), 0..2);
		assert_eq!(q.active_seq(), 2);
		assert_eq!(q.insert(cid(4, 2)), Ok(Inserted::Queued));

		for i in 2..(CidQueue::LEN as u64 - 1) {
			let _ = q.next().unwrap();
			assert_eq!(q.active_seq(), i + 1);
			assert_eq!(q.insert(cid(i + 1, i + 1)), Ok(Inserted::Queued));
		}

		assert!(q.next().is_none());
	}

	#[test]
	fn retire_sparse() {
		// Retiring CID 0 when CID 1 is not known should retire CID 1 as we move to CID 2
		let mut q = CidQueue::new(initial_cid());
		q.insert(cid(2, 0)).unwrap();
		assert_eq!(retired(q.insert(cid(3, 1))), 0..2);
		assert_eq!(q.active_seq(), 2);
	}

	#[test]
	fn retire_many() {
		let mut q = CidQueue::new(initial_cid());
		q.insert(cid(2, 0)).unwrap();
		assert_eq!(retired(q.insert(cid(1_000_000, 1_000_000))), 0..CidQueue::LEN as u64);
		assert_eq!(q.active_seq(), 1_000_000);
	}

	#[test]
	fn insert_limit() {
		let mut q = CidQueue::new(initial_cid());
		assert_eq!(q.insert(cid(CidQueue::LEN as u64 - 1, 0)), Ok(Inserted::Queued));
		assert_eq!(q.insert(cid(CidQueue::LEN as u64, 0)), Err(ExceedsLimit));
	}

	#[test]
	fn insert_duplicate() {
		let mut q = CidQueue::new(initial_cid());
		q.insert(cid(0, 0)).unwrap();
		q.insert(cid(0, 0)).unwrap();
	}

	#[test]
	fn insert_retired() {
		let mut q = CidQueue::new(initial_cid());
		assert_eq!(
			q.insert(cid(0, 0)),
			Ok(Inserted::Queued),
			"reinserting active CID succeeds"
		);
		assert!(q.next().is_none(), "active CID isn't requeued");
		q.insert(cid(1, 0)).unwrap();
		q.next().unwrap();
		assert_eq!(
			q.insert(cid(0, 0)),
			Ok(Inserted::Stale),
			"previous active CID is already retired"
		);
	}

	#[test]
	fn retire_then_insert_next() {
		let mut q = CidQueue::new(initial_cid());
		for i in 1..CidQueue::LEN as u64 {
			q.insert(cid(i, 0)).unwrap();
		}
		q.next().unwrap();
		q.insert(cid(CidQueue::LEN as u64, 0)).unwrap();
		assert_eq!(q.insert(cid(CidQueue::LEN as u64 + 1, 0)), Err(ExceedsLimit));
	}

	#[test]
	fn always_valid() {
		let mut q = CidQueue::new(initial_cid());
		assert!(q.next().is_none());
		assert_eq!(q.active(), initial_cid());
		assert_eq!(q.active_seq(), 0);
	}
}
