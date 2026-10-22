//! Receive leases: bytes a reader has seen but not released, which keep their connection credit,
//! and the eviction that recovers a connection window nobody can make progress in.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};

use bytes::{Buf, Bytes, BytesMut};
use thiserror::Error;

use super::{Chunks, ReadError, ReadableError, RecvStream, ShouldTransmit, StreamEvent, StreamId};
use super::{StreamsState, state::get_or_insert_recv};
use crate::{VarInt, frame};

/// The bytes of a receive stream read in order but not yet released.
///
/// They keep their connection flow control credit until released, so the connection receive
/// window bounds what every lease on the connection holds together. Borrowed with
/// [`RecvStream::lease()`] or [`Connection::leased()`](crate::Connection::leased).
#[derive(Debug, Default)]
pub struct Leased {
    chunks: VecDeque<Bytes>,
    /// Bytes in `chunks`.
    len: u64,
    /// The trailing chunks that still slice the datagram buffers they arrived in, which a byte
    /// of them keeps whole.
    fresh: usize,
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
    pub(super) evicted: bool,
}

/// An eviction candidate, ordered so a max-heap pops the lowest priority, then the newest.
type Victim = Reverse<(i64, Reverse<StreamId>)>;

/// Whether a stream's reader may progress without new connection credit: it never leased what
/// arrived, data arrived while it waited, or it waits behind a gap. A waiting reader's unread
/// bytes all sit past a gap, since its lease took every byte in order, and retransmitting the
/// gap spends no credit. Nothing is evicted meanwhile.
fn unlooked(unread: u64, lease: Option<&Leased>) -> bool {
    (unread > 0 && lease.is_none_or(|lease| !lease.filled || lease.want.is_some()))
        || lease.is_some_and(|lease| lease.ready)
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
        self.fresh = self.fresh.min(self.chunks.len());
        chunk
    }

    /// Drop the first `n` leased bytes, keeping their credit until the next [`Self::release()`].
    fn advance(&mut self, n: usize) {
        let mut left = n;
        assert!(
            n as u64 <= self.len,
            "advanced {n} past {} leased bytes",
            self.len
        );
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
        self.fresh = self.fresh.min(self.chunks.len());
    }

    /// Return the credit of every byte taken or advanced past.
    fn release(&mut self) -> u64 {
        let freed = self.held - self.len;
        self.held = self.len;
        freed
    }

    /// Copy the fresh chunks out of their datagram buffers, for a reader about to wait on them.
    ///
    /// Credit counts the leased bytes, but a slice keeps its whole receive buffer alive, so a
    /// peer splitting a unit into tiny frames would otherwise hold many buffers per byte of
    /// credit. Bytes handed out without waiting never pay for the copy. Absorbing each smaller
    /// predecessor keeps a slowly arriving unit to few chunks while copying each byte only a
    /// logarithmic number of times.
    fn compact(&mut self) {
        if self.fresh == 0 {
            return;
        }
        let mut start = self.chunks.len() - self.fresh;
        let mut size: usize = self.chunks.range(start..).map(Bytes::len).sum();
        while start > 0 && self.chunks[start - 1].len() <= size {
            start -= 1;
            size += self.chunks[start].len();
        }
        let mut buf = BytesMut::with_capacity(size);
        for chunk in self.chunks.drain(start..) {
            buf.extend_from_slice(&chunk);
        }
        self.chunks.push_back(buf.freeze());
        self.fresh = 0;
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
    /// stream ended, then read them with [`Connection::leased()`](crate::Connection::leased).
    ///
    /// The bytes stay buffered, with their connection credit, until [`Self::advance()`] or
    /// [`Self::take()`] moves past them, and keep that credit until [`Self::release()`]; leasing
    /// again without moving returns them again. If [`ShouldTransmit::should_transmit()`]
    /// returns `true`, here or in [`LeaseError::Blocked`], transmit soon.
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
    /// Do not mix with [`Self::read()`] on one stream: a read skips the leased bytes.
    pub fn lease(&mut self, want: usize) -> Result<ShouldTransmit, LeaseError> {
        let transmit = self.fill()?;
        let receive_window = self.state.receive_window;
        let lease = self.state.leases.get_mut(&self.id).expect("filled above");
        // Taken bytes do not count: a unit that outgrows the window with them is one the stuck
        // window evicts by rank, so it gives way like any other rather than failing alone.
        let want = want as u64;
        if want > receive_window {
            return Err(LeaseError::TooLarge);
        }
        if lease.len < want {
            match lease.end {
                Some(End::Fin) => {}
                Some(End::Reset(code)) => return Err(LeaseError::Reset(code)),
                None => {
                    lease.want = Some(want);
                    lease.compact();
                    // It may be waiting behind a gap, which holds eviction off until filled.
                    self.state.unlooked.insert(self.id);
                    // An eviction always has a reader to wake, even with no `MAX_DATA` due yet.
                    let evicted = self.state.evict_if_stuck().inspect(|max_data| {
                        self.pending.max_data |= max_data.should_transmit();
                    });
                    // It may have freed a stream whose final size was known, as a stop does.
                    if evicted.is_some() {
                        self.state.queue_max_stream_id(self.pending);
                    }
                    return Err(LeaseError::Blocked(ShouldTransmit(
                        transmit.should_transmit() || evicted.is_some(),
                    )));
                }
            }
        }
        Ok(transmit)
    }

    /// Hand out up to `max` leading leased bytes without copying, stopping at a chunk boundary.
    ///
    /// They leave the lease but keep their connection credit until the next [`Self::release()`],
    /// which is how a message reaches its consumer before the whole of it has arrived. Empty when
    /// nothing is leased.
    pub fn take(&mut self, max: usize) -> Bytes {
        match self.state.leases.get_mut(&self.id) {
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
        match self.state.leases.get_mut(&self.id) {
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
        let Some(lease) = self.state.leases.get_mut(&self.id) else {
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
        self.state.leases.entry(self.id).or_default().priority = priority;
    }

    /// Move what has arrived in order from the stream's receive buffer into its lease.
    fn fill(&mut self) -> Result<ShouldTransmit, LeaseError> {
        let lease = self.state.leases.entry(self.id).or_default();
        if lease.evicted {
            return Err(LeaseError::Evicted);
        }
        lease.ready = false;
        lease.want = None;
        lease.filled = true;
        self.state.unlooked.remove(&self.id);
        if lease.end.is_some() {
            return Ok(ShouldTransmit(false));
        }

        let mut read = VecDeque::new();
        let (end, transmit) = {
            let mut chunks = match Chunks::new(self.id, true, self.state, self.pending) {
                Ok(chunks) => chunks,
                Err(ReadableError::ClosedStream) => return Err(LeaseError::ClosedStream),
                // Only an unordered read leaves a stream unorderable, and leases never read so.
                Err(ReadableError::IllegalOrderedRead) => return Err(LeaseError::ClosedStream),
            };
            let end = loop {
                match chunks.next(usize::MAX) {
                    Ok(Some(chunk)) => read.push_back(chunk.bytes),
                    Ok(None) => break Some(End::Fin),
                    Err(ReadError::Blocked) => break None,
                    Err(ReadError::Reset(code)) => break Some(End::Reset(code)),
                }
            };
            let _ = chunks.withhold();
            (end, chunks.finalize())
        };

        let lease = self.state.leases.get_mut(&self.id).expect("inserted above");
        let len: u64 = read.iter().map(|chunk| chunk.len() as u64).sum();
        lease.len += len;
        lease.held += len;
        lease.fresh += read.len();
        lease.chunks.append(&mut read);
        lease.end = end;
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
        self.state.queue_max_stream_id(self.pending);
        true
    }

    /// Drop the stream's lease, returning its credit, ahead of a stop.
    ///
    /// Returns `None` if the stream had no lease, or else whether it had been evicted, which
    /// already discarded its receive buffer but left the peer to be told.
    pub(super) fn drop_lease(&mut self, error_code: VarInt) -> Option<bool> {
        self.state.unlooked.remove(&self.id);
        let lease = self.state.leases.remove(&self.id)?;
        if lease.evicted {
            self.state.evicted_held -= lease.held;
        }
        let _ = self.return_credit(lease.held);
        if !lease.evicted {
            return Some(false);
        }
        // Eviction paused for this stop; what it returned may only have paid debt.
        self.unstick();
        let receiving = self
            .state
            .recv
            .get(&self.id)
            .and_then(|slot| slot.as_ref())
            .and_then(|slot| slot.as_open_recv())
            .is_some_and(|recv| recv.final_offset_unknown());
        if receiving {
            self.pending.stop_sending.push(frame::StopSending {
                id: self.id,
                error_code,
            });
        }
        Some(true)
    }
}

impl StreamsState {
    /// The lease of a receive stream, if it has one.
    pub(crate) fn leased(&self, id: StreamId) -> Option<&Leased> {
        self.leases.get(&id)
    }

    /// Note that data arrived on `id`, waking the reader waiting for it.
    pub(super) fn lease_readable(&mut self, id: StreamId) {
        if let Some(lease) = self.leases.get_mut(&id)
            && lease.want.take().is_some()
        {
            lease.ready = true;
        }
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
    /// Returns `None` if nothing was evicted, or whether to transmit the `MAX_DATA` the evictions
    /// freed; each evicted stream's reader is woken by a [`StreamEvent::Readable`].
    pub(super) fn evict_if_stuck(&mut self) -> Option<ShouldTransmit> {
        // Ranked once per pass, so a pass that evicts many streams to pay debt stays linear.
        let mut victims = self.stuck_victims()?;
        let mut transmit = None;
        while self.local_max_data <= self.data_recvd
            && let Some(Reverse((_, Reverse(id)))) = victims.pop()
        {
            transmit = Some(self.evict(id));
            if self.leases.get(&id).is_some_and(|lease| lease.held > 0) {
                break;
            }
        }
        transmit
    }

    /// The streams to evict, lowest rank on top, if the window is stuck; see
    /// [`Self::evict_if_stuck()`].
    fn stuck_victims(&mut self) -> Option<BinaryHeap<Victim>> {
        if self.local_max_data > self.data_recvd {
            return None;
        }
        // An evicted reader has yet to stop its stream, which returns the credit of what it took.
        if self.evicted_held > 0 {
            return None;
        }
        // Checked before the scan, so every waiting reader on a stuck window does not pay for one.
        if self.trim_unlooked() {
            return None;
        }

        #[cfg(test)]
        {
            self.scans += self.recv.len();
        }
        // Newest first among equals: the stream with the higher index arrived later.
        let rank = |priority: i64, id: StreamId| Reverse((priority, Reverse(id)));
        let mut victims = Vec::new();
        for (&id, slot) in &self.recv {
            let Some(recv) = slot.as_ref().and_then(|slot| slot.as_open_recv()) else {
                continue;
            };
            let lease = self.leases.get(&id);
            let unread = recv.end - recv.assembler.bytes_read();
            let held = unread + lease.map_or(0, |lease| lease.held);
            if recv.stopped || held == 0 || lease.is_some_and(|lease| lease.evicted) {
                continue;
            }
            debug_assert!(
                !unlooked(unread, lease),
                "{id} has data its reader has not looked at"
            );
            victims.push(rank(lease.map_or(0, |lease| lease.priority), id));
        }
        for (&id, lease) in &self.leases {
            // A lease outlives its stream state once the last byte is read.
            if lease.held > 0 && !lease.evicted && !self.recv.contains_key(&id) {
                victims.push(rank(lease.priority, id));
            }
        }

        Some(BinaryHeap::from(victims))
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
            let recv = self
                .recv
                .get(&id)
                .and_then(|slot| slot.as_ref()?.as_open_recv());
            let lease = self.leases.get(&id);
            let waiting = recv.is_some_and(|recv| {
                let unread = recv.end - recv.assembler.bytes_read();
                !recv.stopped
                    && !lease.is_some_and(|lease| lease.evicted)
                    && unlooked(unread, lease)
            });
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
    /// Bytes the reader took live on outside the connection until it stops the stream, so their
    /// credit stays held until then.
    fn evict(&mut self, id: StreamId) -> ShouldTransmit {
        tracing::debug!(%id, "evicting a stream to free a stuck receive window");
        let lease = self.leases.entry(id).or_default();
        let held = std::mem::take(&mut lease.len);
        lease.held -= held;
        lease.chunks.clear();
        lease.fresh = 0;
        lease.want = None;
        lease.ready = false;
        lease.evicted = true;
        self.evicted_held += lease.held;
        self.unlooked.remove(&id);
        self.held -= held;
        self.evicted += 1;

        let mut freed = held;
        if let Some(slot) = self.recv.get_mut(&id).filter(|slot| slot.is_some()) {
            let recv = get_or_insert_recv(slot, self.stream_receive_window);
            // The peer hears of it when the application stops the stream with its own code.
            if let Ok((credits, _)) = recv.stop() {
                freed += credits;
            }
            if !recv.final_offset_unknown() {
                let recv = self.recv.remove(&id).flatten().expect("checked above");
                self.stream_recv_freed(id, recv);
            }
        }
        self.events.push_back(StreamEvent::Readable { id });
        self.add_read_credits(freed)
    }
}
