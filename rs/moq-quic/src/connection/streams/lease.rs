//! Receive leases: bytes a reader has seen but not consumed, which keep their connection credit,
//! and the eviction that recovers a connection window nobody can make progress in.

use std::collections::VecDeque;

use bytes::{Buf, Bytes};
use thiserror::Error;

use super::{Chunks, ReadError, ReadableError, RecvStream, ShouldTransmit, StreamEvent, StreamId};
use super::{StreamsState, state::get_or_insert_recv};
use crate::{VarInt, frame};

/// The bytes of a receive stream read in order but not yet consumed.
///
/// They keep their connection flow control credit until consumed, so the connection receive
/// window bounds what every lease on the connection holds together. Borrowed with
/// [`RecvStream::lease()`] or [`Connection::leased()`](crate::Connection::leased).
#[derive(Debug, Default)]
pub struct Leased {
    chunks: VecDeque<Bytes>,
    /// Bytes in `chunks`.
    len: u64,
    /// Bytes whose connection credit is withheld: `len`, plus taken bytes not yet consumed.
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
    /// Their credit stays held until the next [`Self::consume()`].
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

    /// Drop the first `n` leased bytes, returning the credit they and every taken byte free.
    fn consume(&mut self, n: usize) -> u64 {
        let mut left = n;
        assert!(
            n as u64 <= self.len,
            "consumed {n} of {} leased bytes",
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
        let freed = self.held - self.len;
        self.held = self.len;
        freed
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
    /// The peer has not been told: [`RecvStream::stop()`] sends the code of the caller's choosing.
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
    /// Lease the stream's unconsumed bytes once at least `want` have arrived in order, or the
    /// stream ended, then read them with [`Connection::leased()`](crate::Connection::leased).
    ///
    /// The bytes stay buffered, with their connection credit, until [`Self::consume()`]; leasing
    /// again without consuming returns them again. If [`ShouldTransmit::should_transmit()`]
    /// returns `true`, here or in [`LeaseError::Blocked`], transmit soon.
    ///
    /// [`LeaseError::Blocked`] records the `want`, so
    /// the next [`StreamEvent::Readable`] for this stream means it may be met, and so that a
    /// connection window every reader is waiting on can be recovered by evicting the stream ranked
    /// lowest by [`Self::set_priority()`].
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
                    // An eviction always has a reader to wake, even with no `MAX_DATA` due yet.
                    let evicted = self.state.evict_if_stuck().inspect(|max_data| {
                        self.pending.max_data |= max_data.should_transmit();
                    });
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
    /// They leave the lease but keep their connection credit until the next [`Self::consume()`],
    /// which is how a message reaches its consumer before the whole of it has arrived. Empty when
    /// nothing is leased.
    pub fn take(&mut self, max: usize) -> Bytes {
        match self.state.leases.get_mut(&self.id) {
            Some(lease) => lease.take(max),
            None => Bytes::new(),
        }
    }

    /// Release the first `n` leased bytes, and the connection credit of every byte taken so far.
    ///
    /// If [`ShouldTransmit::should_transmit()`] returns `true`, transmit soon so the peer learns
    /// of the larger window.
    ///
    /// # Panics
    /// If `n` exceeds the leased bytes.
    pub fn consume(&mut self, n: usize) -> ShouldTransmit {
        let Some(lease) = self.state.leases.get_mut(&self.id) else {
            assert_eq!(n, 0, "consumed {n} bytes of an empty lease");
            return ShouldTransmit(false);
        };
        let freed = lease.consume(n);
        self.release(freed)
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
        lease.chunks.append(&mut read);
        lease.end = end;
        Ok(transmit)
    }

    /// Return `bytes` of withheld connection credit to the peer.
    fn release(&mut self, bytes: u64) -> ShouldTransmit {
        if bytes == 0 {
            return ShouldTransmit(false);
        }
        let transmit = self.state.release(bytes);
        self.pending.max_data |= transmit.should_transmit();
        transmit
    }

    /// Drop the stream's lease, returning its credit, ahead of a stop.
    ///
    /// Returns whether the stream had been evicted, which already discarded its receive buffer
    /// but left the peer to be told.
    pub(super) fn drop_lease(&mut self, error_code: VarInt) -> bool {
        let Some(lease) = self.state.leases.remove(&self.id) else {
            return false;
        };
        let _ = self.release(lease.held);
        if !lease.evicted {
            return false;
        }
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
        true
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

    /// Evict the lowest-priority stream holding credit when no reader can make progress: the
    /// peer has no connection credit left, and no reader has data it has not looked at yet.
    ///
    /// Waiting for a stream to be read cannot free the window then, and neither can the peer. A
    /// reader that leased and then parked, without waiting, cannot free it either, so it is a
    /// candidate too. A stream never leased is not: its reader may not have run yet, and a
    /// stream read without leases returns its credit as it reads.
    /// Returns `None` if nothing was evicted, or whether to transmit the `MAX_DATA` the eviction
    /// freed; the evicted stream's reader is woken by a [`StreamEvent::Readable`].
    pub(super) fn evict_if_stuck(&mut self) -> Option<ShouldTransmit> {
        if self.local_max_data > self.data_recvd {
            return None;
        }

        // Newest first among equals: the stream with the higher index arrived later.
        let rank = |priority: i64, id: StreamId| (priority, std::cmp::Reverse(id.index()));
        let mut victim: Option<((i64, std::cmp::Reverse<u64>), StreamId)> = None;
        let mut consider = |key, id| {
            if victim.is_none_or(|(best, _)| key < best) {
                victim = Some((key, id));
            }
        };
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
            let fresh = unread > 0 && lease.is_none_or(|lease| !lease.filled);
            if fresh || lease.is_some_and(|lease| lease.ready) {
                return None;
            }
            consider(rank(lease.map_or(0, |lease| lease.priority), id), id);
        }
        // A lease outlives its stream state once the last byte is read.
        for (&id, lease) in &self.leases {
            if lease.held > 0 && !lease.evicted && !self.recv.contains_key(&id) {
                consider(rank(lease.priority, id), id);
            }
        }

        let (_, id) = victim?;
        Some(self.evict(id))
    }

    /// Discard everything `id` holds, returning its credit, and fail its next lease.
    fn evict(&mut self, id: StreamId) -> ShouldTransmit {
        tracing::debug!(%id, "evicting a stream to free a stuck receive window");
        let lease = self.leases.entry(id).or_default();
        let held = std::mem::take(&mut lease.held);
        lease.chunks.clear();
        lease.len = 0;
        lease.want = None;
        lease.ready = false;
        lease.evicted = true;
        self.held -= held;

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
