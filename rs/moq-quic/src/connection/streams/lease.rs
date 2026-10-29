//! Receive leases: bytes a reader has seen but not released, which keep their connection credit,
//! and the eviction that recovers a connection window nobody can make progress in.

use std::cmp::Reverse;
use std::collections::VecDeque;

use bytes::{Buf, Bytes, BytesMut};
use thiserror::Error;

use super::{RecvStream, ShouldTransmit, StreamEvent, StreamId};
use super::{StreamsState, state::get_or_insert_recv};
use crate::VarInt;
use crate::connection::assembler::Assembler;

/// The bytes of a receive stream read in order but not yet released.
///
/// They keep their connection flow control credit until released, so the connection receive
/// window bounds what every lease on the connection holds together. Borrowed with
/// [`RecvStream::lease()`] or [`Connection::leased()`](crate::connection::Connection::leased).
#[derive(Debug, Default)]
pub struct Leased {
	chunks: VecDeque<Bytes>,
	/// Bytes in `chunks`.
	len: u64,
	/// Bytes whose connection credit is withheld: `len`, plus bytes taken or advanced past but
	/// not yet released.
	held: u64,
	/// How the stream ended, once the last byte is leased.
	end: Option<End>,
	priority: i64,
	/// The `len` a reader waits for.
	want: Option<u64>,
	/// Data arrived for a waiting reader that has not looked yet.
	ready: bool,
	/// Whether a reader has leased the stream yet; see [`StreamsState::evict_if_stuck`].
	filled: bool,
	/// Whether the stream is in [`StreamsState::ranks`].
	ranked: bool,
	/// Discarded to free a stuck window, with the reader and the peer yet to hear of it from a
	/// stop.
	pub(super) evicted: bool,
}

/// A receive stream's place in the eviction order: lowest priority first, then the newest, which
/// has the higher index.
pub(super) type Rank = (i64, Reverse<StreamId>);

/// Whether a stream's reader may progress without new connection credit: it never leased what
/// arrived, data arrived while it waited, or it waits behind a gap. A waiting reader's unread
/// bytes all sit past a gap, since its lease took every byte in order, and retransmitting the
/// gap spends no credit. Nothing is evicted meanwhile.
fn unlooked(unread: u64, lease: &Leased) -> bool {
	(unread > 0 && (!lease.filled || lease.want.is_some())) || lease.ready
}

#[derive(Debug, Clone, Copy)]
enum End {
	Fin,
	Reset(VarInt),
}

impl Leased {
	/// The number of leased bytes.
	pub fn len(&self) -> usize {
		self.len as usize
	}

	/// Whether nothing is leased.
	pub fn is_empty(&self) -> bool {
		self.len == 0
	}

	/// Whether the peer finished the stream right after the leased bytes.
	pub fn is_fin(&self) -> bool {
		matches!(self.end, Some(End::Fin))
	}

	/// The leased bytes in order, one contiguous chunk at a time.
	pub fn chunks(&self) -> impl Iterator<Item = &[u8]> {
		self.chunks.iter().map(|chunk| &chunk[..])
	}

	/// Hand out up to `max` leading bytes without copying, stopping at a chunk boundary.
	///
	/// Their credit stays held until the next [`Self::release()`].
	fn take(&mut self, max: usize) -> Bytes {
		let Some(front) = self.chunks.front_mut() else {
			return Bytes::new();
		};
		let chunk = match front.len() > max {
			true => front.split_to(max),
			false => self.chunks.pop_front().expect("front exists"),
		};
		self.len -= chunk.len() as u64;
		chunk
	}

	/// Drop the first `n` leased bytes, keeping their credit until the next [`Self::release()`].
	fn advance(&mut self, n: usize) {
		let mut left = n;
		assert!(n as u64 <= self.len, "advanced {n} past {} leased bytes", self.len);
		self.len -= n as u64;
		while left > 0 {
			let front = self.chunks.front_mut().expect("counted above");
			if front.len() > left {
				front.advance(left);
				break;
			}
			left -= front.len();
			self.chunks.pop_front();
		}
	}

	/// Return the credit of every byte taken or advanced past.
	fn release(&mut self) -> u64 {
		let freed = self.held - self.len;
		self.held = self.len;
		freed
	}

	/// Lease `read`, copied out of the datagram buffers it arrived in.
	///
	/// Credit counts the leased bytes, but a slice keeps its whole receive buffer alive, so a
	/// peer splitting a unit into tiny frames would otherwise hold many buffers per byte of
	/// credit, whether the reader waits on them, parks, or takes them. Absorbing each smaller
	/// predecessor keeps a slowly arriving unit to few chunks while copying each byte only a
	/// logarithmic number of times.
	fn extend(&mut self, read: &[Bytes]) {
		let len: usize = read.iter().map(Bytes::len).sum();
		if len == 0 {
			return;
		}
		let (mut start, mut size) = (self.chunks.len(), len);
		while start > 0 && self.chunks[start - 1].len() <= size {
			start -= 1;
			size += self.chunks[start].len();
		}
		let mut buf = BytesMut::with_capacity(size);
		for chunk in self.chunks.drain(start..) {
			buf.extend_from_slice(&chunk);
		}
		for chunk in read {
			buf.extend_from_slice(chunk);
		}
		self.chunks.push_back(buf.freeze());
		self.len += len as u64;
		self.held += len as u64;
	}

	pub(super) fn rank(&self, id: StreamId) -> Rank {
		(self.priority, Reverse(id))
	}

	/// Note that data arrived, waking a waiting reader, and return the stream's rank if it was
	/// leased but is not ranked yet.
	pub(super) fn arrived(&mut self, id: StreamId) -> Option<Rank> {
		if self.want.take().is_some() {
			self.ready = true;
		}
		self.rank_once(id)
	}

	/// Mark a leased stream ranked, returning its rank if it was not already.
	///
	/// A stream never leased is never a candidate: data its reader has not looked at holds
	/// eviction off, and without any it holds no credit. So plain reads pay nothing here.
	fn rank_once(&mut self, id: StreamId) -> Option<Rank> {
		(self.filled && !std::mem::replace(&mut self.ranked, true)).then(|| self.rank(id))
	}

	/// Move what `assembler` holds in order into the lease, returning how many bytes.
	fn fill(&mut self, assembler: &mut Assembler) -> u64 {
		self.ready = false;
		self.want = None;
		self.filled = true;
		let mut read = Vec::new();
		while let Some(chunk) = assembler.read(usize::MAX) {
			read.push(chunk.bytes);
		}
		let len = self.len;
		self.extend(&read);
		self.len - len
	}

	/// Discard the leased bytes for an eviction, returning how many. Taken bytes keep their
	/// credit until the stop.
	fn evict(&mut self) -> u64 {
		let leased = std::mem::take(&mut self.len);
		self.held -= leased;
		self.chunks.clear();
		self.want = None;
		self.ready = false;
		self.evicted = true;
		leased
	}

	/// Discard everything, returning the credit held.
	pub(super) fn discard(&mut self) -> u64 {
		self.len = 0;
		self.chunks.clear();
		self.want = None;
		self.ready = false;
		std::mem::take(&mut self.held)
	}
}

/// Errors from [`RecvStream::lease()`]
#[derive(Debug, Error, Clone, Copy, Eq, PartialEq)]
pub enum LeaseError {
	/// Fewer bytes than wanted have arrived; the stream is readable again once more do.
	///
	/// Carries whether frames were queued meanwhile, for stream credit or an eviction.
	#[error("blocked")]
	Blocked(ShouldTransmit),
	/// The peer abandoned transmitting data on this stream, after the leased bytes.
	#[error("reset by peer: code {0}")]
	Reset(VarInt),
	/// The connection window was stuck and this stream was discarded to free it.
	///
	/// The peer has not been told: stop the stream with [`RecvStream::stop()`] and a code of the
	/// caller's choosing. Until then the peer may keep sending, which costs bandwidth but no
	/// window, since it is credited on arrival, and the bytes the reader took keep their credit,
	/// which holds further eviction off.
	#[error("evicted")]
	Evicted,
	/// More bytes were wanted than the connection receive window can ever hold.
	#[error("want exceeds the receive window")]
	TooLarge,
	/// The stream was stopped, or was never opened.
	#[error("closed stream")]
	ClosedStream,
}

impl RecvStream<'_> {
	/// Lease the stream's unreleased bytes once at least `want` have arrived in order, or the
	/// stream ended, then read them with [`Connection::leased()`](crate::connection::Connection::leased).
	///
	/// The bytes stay buffered, with their connection credit, until [`Self::advance()`] or
	/// [`Self::take()`] moves past them, and keep that credit until [`Self::release()`]; leasing
	/// again without moving returns them again. If [`ShouldTransmit::should_transmit()`]
	/// returns `true`, here or in [`LeaseError::Blocked`], transmit soon; the other errors queue
	/// nothing.
	///
	/// [`LeaseError::Blocked`] records the `want`, so
	/// the next [`StreamEvent::Readable`] for this stream means it may be met, and so that a
	/// connection window every reader is waiting on can be recovered by evicting the stream ranked
	/// lowest by [`Self::set_priority()`].
	///
	/// Nothing is evicted while the peer has connection credit left, so while the leased and
	/// unread bytes of every stream together stay below the connection receive window. A receiver
	/// whose window exceeds the most its incomplete units can hold never sees an eviction;
	/// [`CreditStats::evicted`](crate::CreditStats::evicted) counts any that happen.
	///
	/// Eviction also waits for every stream with data its reader has not leased yet, since that
	/// reader may be about to free the window, and for every reader waiting behind a gap, which
	/// a retransmission fills without new credit. A reader that gives up on a stream must stop it
	/// rather than leave it unpolled.
	///
	/// A leased stream keeps its state, and its stream ID, until it is stopped, even once it
	/// ended, so stop it when done.
	///
	/// Do not mix with [`Self::read()`] on one stream: a read skips the leased bytes.
	pub fn lease(&mut self, want: usize) -> Result<ShouldTransmit, LeaseError> {
		// Taken bytes do not count: a unit that outgrows the window with them is one the stuck
		// window evicts by rank, so it gives way like any other rather than failing alone.
		let want = want as u64;
		if want > self.state.receive_window {
			return Err(LeaseError::TooLarge);
		}
		let transmit = self.fill()?;
		let lease = self.state.lease_mut(self.id).expect("filled above");
		if lease.len >= want {
			return Ok(transmit);
		}
		match lease.end {
			Some(End::Fin) => Ok(transmit),
			Some(End::Reset(code)) => {
				debug_assert!(!transmit.should_transmit(), "a reset stream takes no credit");
				Err(LeaseError::Reset(code))
			}
			None => {
				lease.want = Some(want);
				// It may be waiting behind a gap, which holds eviction off until filled.
				self.state.unlooked.insert(self.id);
				// An eviction always has a reader to wake, even with no `MAX_DATA` due yet.
				let evicted = self.unstick();
				Err(LeaseError::Blocked(ShouldTransmit(
					transmit.should_transmit() || evicted,
				)))
			}
		}
	}

	/// Hand out up to `max` leading leased bytes without copying, stopping at a chunk boundary.
	///
	/// They leave the lease but keep their connection credit until the next [`Self::release()`],
	/// which is how a message reaches its consumer before the whole of it has arrived. Empty when
	/// nothing is leased.
	pub fn take(&mut self, max: usize) -> Bytes {
		match self.state.lease_mut(self.id) {
			Some(lease) => lease.take(max),
			None => Bytes::new(),
		}
	}

	/// Drop the first `n` leased bytes, which keep their connection credit until the next
	/// [`Self::release()`].
	///
	/// # Panics
	/// If `n` exceeds the leased bytes.
	pub fn advance(&mut self, n: usize) {
		match self.state.lease_mut(self.id) {
			Some(lease) => lease.advance(n),
			None => assert_eq!(n, 0, "advanced {n} past an empty lease"),
		}
	}

	/// Return the connection credit of every byte taken or advanced past, once the units they
	/// belong to are complete.
	///
	/// If [`ShouldTransmit::should_transmit()`] returns `true`, transmit soon so the peer learns
	/// of the larger window.
	pub fn release(&mut self) -> ShouldTransmit {
		let Some(lease) = self.state.lease_mut(self.id) else {
			return ShouldTransmit(false);
		};
		let freed = lease.release();
		if lease.evicted {
			self.state.evicted_held -= freed;
		}
		let transmit = self.return_credit(freed);
		ShouldTransmit(transmit.should_transmit() || self.unstick())
	}

	/// Rank this stream for eviction: when the connection window is stuck, the stream with the
	/// lowest priority goes first, and the newest of those that tie.
	pub fn set_priority(&mut self, priority: i64) {
		let Some(slot) = self.state.recv.get_mut(&self.id) else {
			return;
		};
		let lease = &mut get_or_insert_recv(slot, self.state.initial_stream_receive_window).lease;
		if lease.ranked {
			self.state.ranks.remove(&lease.rank(self.id));
			self.state.ranks.insert((priority, Reverse(self.id)));
		}
		lease.priority = priority;
	}

	/// Move what has arrived in order from the stream's receive buffer into its lease.
	fn fill(&mut self) -> Result<ShouldTransmit, LeaseError> {
		let Some(slot) = self.state.recv.get_mut(&self.id) else {
			return Err(LeaseError::ClosedStream);
		};
		let recv = get_or_insert_recv(slot, self.state.initial_stream_receive_window);
		if recv.lease.evicted {
			return Err(LeaseError::Evicted);
		}
		if recv.stopped {
			return Err(LeaseError::ClosedStream);
		}
		self.state.unlooked.remove(&self.id);
		self.state.held += recv.lease.fill(&mut recv.assembler);
		if recv.unread_credit() + recv.lease.held > 0 {
			self.state.ranks.extend(recv.lease.rank_once(self.id));
		}
		if recv.lease.end.is_none() {
			recv.lease.end = match recv.reset_code() {
				Some(code) => Some(End::Reset(code)),
				None if recv.is_read() => Some(End::Fin),
				None => None,
			};
		}
		let (_, transmit) = recv.max_stream_data(self.state.stream_receive_window);
		if transmit.should_transmit() {
			self.pending.max_stream_data.insert(self.id);
		}
		Ok(transmit)
	}

	/// Return `bytes` of withheld connection credit to the peer.
	fn return_credit(&mut self, bytes: u64) -> ShouldTransmit {
		if bytes == 0 {
			return ShouldTransmit(false);
		}
		let transmit = self.state.release(bytes);
		self.pending.max_data |= transmit.should_transmit();
		transmit
	}

	/// Rerun the stuck-window pass after returning credit, which may only have paid a shrunk
	/// window's debt: the peer stays blocked, and the readers that would rerun it may all be
	/// waiting already. Returns whether it evicted anything.
	pub(super) fn unstick(&mut self) -> bool {
		let Some(transmit) = self.state.evict_if_stuck() else {
			return false;
		};
		self.pending.max_data |= transmit.should_transmit();
		true
	}
}

impl StreamsState {
	/// The lease of a receive stream, if a reader leased it and has not stopped it since.
	pub(crate) fn leased(&self, id: StreamId) -> Option<&Leased> {
		let recv = self.recv.get(&id)?.as_ref()?.as_open_recv()?;
		let lease = &recv.lease;
		(lease.filled && (!recv.stopped || lease.evicted)).then_some(lease)
	}

	fn lease_mut(&mut self, id: StreamId) -> Option<&mut Leased> {
		let recv = self.recv.get_mut(&id)?.as_mut()?.as_open_recv_mut()?;
		Some(&mut recv.lease)
	}

	/// Return `bytes` of credit withheld by a lease to the peer.
	pub(super) fn release(&mut self, bytes: u64) -> ShouldTransmit {
		assert!(
			bytes <= self.held,
			"released {bytes} bytes of credit with only {} held",
			self.held
		);
		self.held -= bytes;
		self.add_read_credits(bytes)
	}

	/// Take the lease of `id`, stopped or freed, out of the eviction order and return the credit
	/// of the bytes it holds.
	pub(super) fn discard_lease(&mut self, id: StreamId, lease: &mut Leased) -> ShouldTransmit {
		if std::mem::take(&mut lease.ranked) {
			self.ranks.remove(&lease.rank(id));
		}
		let held = lease.discard();
		if std::mem::take(&mut lease.evicted) {
			self.evicted_held -= held;
		}
		self.release(held)
	}

	/// Evict the lowest-priority streams holding credit while no reader can make progress: the
	/// peer has no connection credit left, and no reader has data it has not looked at yet or
	/// waits behind a gap.
	///
	/// Waiting for a stream to be read cannot free the window then, and neither can the peer. A
	/// reader that leased and then parked, without waiting, cannot free it either, so it is a
	/// candidate too. A stream never leased is not: its reader may not have run yet, and a
	/// stream read without leases returns its credit as it reads.
	///
	/// Credit freed while a shrunk window is in debt pays that first and may advertise nothing,
	/// so eviction continues until the peer has credit again or no candidate is left. It pauses
	/// while an evicted reader still holds bytes it took, which keep their credit until it stops
	/// the stream, so a pass never evicts more than that stop would have freed.
	///
	/// Candidates come off [`Self::ranks`] in order, and a stream found holding nothing leaves
	/// it until data arrives again, so a pass costs a logarithm per stream it looks at, and each
	/// arrival pays for one look at most.
	///
	/// Returns `None` if nothing was evicted, or whether to transmit the `MAX_DATA` the evictions
	/// freed; each evicted stream's reader is woken by a [`StreamEvent::Readable`].
	pub(super) fn evict_if_stuck(&mut self) -> Option<ShouldTransmit> {
		if self.local_max_data > self.data_recvd {
			return None;
		}
		// An evicted reader has yet to stop its stream, which returns the credit of what it took.
		if self.evicted_held > 0 {
			return None;
		}
		// Checked before any eviction, so a reader about to free the window is waited for.
		if self.trim_unlooked() {
			return None;
		}

		let mut transmit = None;
		while self.local_max_data <= self.data_recvd
			&& self.evicted_held == 0
			&& let Some(rank) = self.ranks.pop_first()
		{
			#[cfg(test)]
			{
				self.scans += 1;
			}
			let id = rank.1.0;
			let recv = self
				.recv
				.get_mut(&id)
				.and_then(|slot| slot.as_mut()?.as_open_recv_mut())
				.expect("a freed stream leaves the ranks");
			recv.lease.ranked = false;
			if recv.stopped || recv.unread_credit() + recv.lease.held == 0 {
				continue;
			}
			debug_assert!(
				!unlooked(recv.unread_credit(), &recv.lease),
				"{id} has data its reader has not looked at"
			);
			transmit = Some(self.evict(id));
		}
		transmit
	}

	/// Whether any stream has data its reader has not looked at yet, which holds eviction off,
	/// dropping the listed streams that no longer do.
	fn trim_unlooked(&mut self) -> bool {
		let mut stale = Vec::new();
		let mut found = false;
		for &id in &self.unlooked {
			#[cfg(test)]
			{
				self.scans += 1;
			}
			let waiting = self
				.recv
				.get(&id)
				.and_then(|slot| slot.as_ref()?.as_open_recv())
				.is_some_and(|recv| !recv.stopped && unlooked(recv.unread_credit(), &recv.lease));
			if waiting {
				found = true;
				break;
			}
			stale.push(id);
		}
		for id in stale {
			self.unlooked.remove(&id);
		}
		found
	}

	/// Discard everything `id` buffers, returning its credit, and fail its next lease.
	///
	/// The stream stays until the application stops it, which tells the peer, and bytes the
	/// reader took live on outside the connection until then, so their credit stays held too.
	fn evict(&mut self, id: StreamId) -> ShouldTransmit {
		tracing::debug!(%id, "evicting a stream to free a stuck receive window");
		let recv = self
			.recv
			.get_mut(&id)
			.and_then(|slot| slot.as_mut()?.as_open_recv_mut())
			.expect("a candidate is open");
		let leased = recv.lease.evict();
		self.evicted_held += recv.lease.held;
		// The peer hears of it when the application stops the stream with its own code.
		let (unread, _) = recv.stop().expect("a candidate is not stopped");
		self.held -= leased;
		self.unlooked.remove(&id);
		self.evicted += 1;
		self.events.push_back(StreamEvent::Readable { id });
		self.add_read_credits(leased + unread)
	}

	/// Assert that the lease books balance: [`Self::held`] and [`Self::evicted_held`] sum the
	/// leases, and every leased stream holding credit is ranked.
	#[cfg(any(test, feature = "fuzz"))]
	pub(crate) fn check_leases(&self) {
		let (mut held, mut evicted_held, mut ranked) = (0, 0, 0);
		for (&id, slot) in &self.recv {
			let Some(recv) = slot.as_ref().and_then(|slot| slot.as_open_recv()) else {
				continue;
			};
			let lease = &recv.lease;
			held += lease.held;
			if lease.evicted {
				evicted_held += lease.held;
			}
			assert_eq!(lease.ranked, self.ranks.contains(&lease.rank(id)), "{id}");
			ranked += usize::from(lease.ranked);
			if lease.filled && !recv.stopped && recv.unread_credit() + lease.held > 0 {
				assert!(lease.ranked, "{id} holds credit unranked");
			}
		}
		assert_eq!(held, self.held);
		assert_eq!(evicted_held, self.evicted_held);
		assert_eq!(ranked, self.ranks.len(), "ranks outlived their streams");
	}
}
