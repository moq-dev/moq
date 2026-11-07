//! Data channel related types.

use std::{fmt, str, time::Instant};

use crate::poll::{RtcMut, Wake};
use crate::sctp::RtcSctp;
use crate::util::already_happened;
use crate::{Rtc, RtcError};

pub use crate::sctp::ChannelConfig;
pub use crate::sctp::Reliability;
pub use crate::sctp::SctpInitData;

/// Identifier of a data channel.
///
/// This is NOT the SCTP stream id.
// Deliberately not Deref or From to avoid this Id being created outside of this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelId(usize);

/// Data channel data from remote peer.
///
/// This is obtained via [`Event::ChannelData`][crate::Event::ChannelData].
#[derive(PartialEq, Eq)]
pub struct ChannelData {
    /// Identifier of the channel this data was sent on.
    ///
    /// The channel would have been previously announced via
    /// [`Event::ChannelOpen`][crate::Event::ChannelOpen].
    pub id: ChannelId,

    /// Tells whether the sender sent this data as binary or text.
    pub binary: bool,

    /// The actual data sent. If `binary` is false, this can be converted to text.
    pub data: Vec<u8>,
}

/// Channel for sending data to the remote peer.
///
/// Get this handle from [`Rtc::channel()`][crate::Rtc::channel()].
pub struct Channel<'a> {
    sctp_stream_id: u16,
    // `RtcMut` arms the readiness on mutable deref only; see [`crate::poll`].
    rtc: RtcMut<'a>,
}

impl<'a> Channel<'a> {
    pub(crate) fn new(sctp_stream_id: u16, rtc: &'a mut Rtc) -> Self {
        Channel {
            rtc: RtcMut::new(rtc),
            sctp_stream_id,
        }
    }

    /// Write data to the remote peer and indicate whether it's text or binary.
    ///
    /// Returns true or false whether the buffer was accepted or not.
    #[must_use = "Whether the buffer was accepted by the write()"]
    pub fn write(&mut self, binary: bool, buf: &[u8]) -> Result<bool, RtcError> {
        // If it's not available, don't accept.
        let available = self.rtc.sctp.available();
        if buf.len() > available {
            return Ok(false);
        }

        // Try write.
        let written = self.rtc.sctp.write(self.sctp_stream_id, binary, buf)?;

        // Invariant: if available calculation is correct, we should have accepted.
        assert_eq!(
            written,
            buf.len(),
            "Data channel write() less than entire buffer"
        );

        Ok(true)
    }

    /// Get the amount of buffered data.
    ///
    /// Returns 0 if the channel is closed or encountered some error. This is to
    /// be similar to the [RTCPeerConnection equivalent][buff]
    ///
    /// [buff]: https://developer.mozilla.org/en-US/docs/Web/API/RTCDataChannel/bufferedAmount
    pub fn buffered_amount(&mut self) -> usize {
        self.rtc.sctp.buffered_amount(self.sctp_stream_id)
    }

    /// Set the threshold to emit an
    /// [`Event::ChannelBufferedAmountLow`][crate::Event::ChannelBufferedAmountLow]
    ///
    /// Setting this on a closed or broken channel does not show an error. This is
    /// be similar to the [RTCPeerConnection equivalent][buff]
    ///
    /// [buff]: https://developer.mozilla.org/en-US/docs/Web/API/RTCDataChannel/bufferedAmountLowThreshold
    pub fn set_buffered_amount_low_threshold(&mut self, threshold: usize) {
        self.rtc
            .sctp
            .set_buffered_amount_low_threshold(self.sctp_stream_id, threshold);
    }

    /// Get the channel config.
    ///
    /// The config is not available in every case depending on whether the channel was
    /// negotiated in- or out of band.
    ///
    /// # In-band negotiation (the usual case)
    ///
    /// For (regular) in-band negotiation (DCEP, Data Channel Establishment Protocol), this
    /// returns `None` until the DCEP handshake completes. The config is guaranteed to be
    /// available when [`Event::ChannelOpen`][crate::Event::ChannelOpen] is emitted.
    ///
    /// # Out-of-band negotiation
    ///
    /// Returns `None` when the remote side created the data channel connection without using
    /// DCEP. This is called out-of-band negotiation, where the remote peer opens a stream
    /// but doesn't send the channel configuration through the DCEP protocol messages.
    ///
    /// For locally created out-of-band channels, the config is always available since it
    /// was provided during channel creation.
    ///
    /// In str0m, DCEP is disabled by setting the `negotiated` field to `Some(stream_id)` in
    /// [`ChannelConfig`]. This corresponds to the `negotiated: true` property in the
    /// browser's [`createDataChannel()`][n] dictionary.
    ///
    /// [n]: https://developer.mozilla.org/en-US/docs/Web/API/RTCPeerConnection/createDataChannel#negotiated
    pub fn config(&self) -> Option<&ChannelConfig> {
        self.rtc.sctp.config(self.sctp_stream_id)
    }
}

impl fmt::Debug for ChannelData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut ds = f.debug_struct("ChannelData");

        ds.field("id", &self.id);
        ds.field("binary", &self.binary);

        let len = &self.data.len();
        if self.binary {
            ds.field("data", len);
        } else {
            match str::from_utf8(&self.data) {
                Ok(s) => {
                    const MAX_LINE_WIDTH: usize = 79;
                    const REST_OF_LINE_WIDTH: usize =
                        "ChannelData { id: ChannelId(0), binary: false, data: \"\" }".len();
                    const TUPLE_WIDTH: usize = "(xxx, ..)".len();
                    const DATA_WIDTH: usize = MAX_LINE_WIDTH - REST_OF_LINE_WIDTH;
                    const PREFIX_WIDTH: usize = DATA_WIDTH - TUPLE_WIDTH;
                    if s.is_ascii() {
                        if len > &DATA_WIDTH {
                            let trunc: String = s.chars().take(PREFIX_WIDTH).collect();
                            ds.field("data", &format_args!("({}, \"{}\"..)", len, trunc));
                        } else {
                            ds.field("data", &s);
                        }
                    } else {
                        ds.field("data", len);
                    }
                }
                Err(e) => {
                    ds.field("data", &format_args!("{:?}", (len, &e)));
                }
            }
        }

        ds.finish()
    }
}

#[derive(Debug, Default)]
pub(crate) struct ChannelHandler {
    allocations: Vec<ChannelAllocation>,
    next_channel_id: usize,
    /// Stream IDs of closed channels whose reset handshake is still
    /// outstanding, excluded from allocation until it completes.
    closed_stream_ids: Vec<u16>,
}

#[derive(Debug)]
struct ChannelAllocation {
    id: ChannelId,

    /// Stream id, when it is known. This might be delayed awaiting sctp initialization to
    /// know if we are client or server, or awaiting the reset handshake of a previous
    /// incarnation of a negotiated id.
    sctp_stream_id: Option<u16>,

    /// The out-of-band negotiated stream id the user asked for. Promoted to
    /// `sctp_stream_id` once reset which was holding it completes.
    negotiated_stream_id: Option<u16>,

    /// Holds the config until it is used in handle_timeout.
    config: Option<ChannelConfig>,
}

impl ChannelHandler {
    pub fn new_channel(&mut self, config: &ChannelConfig) -> ChannelId {
        let id = self.next_channel_id();

        // Out-of-band negotiated means the user names the stream id
        // instead of us allocating one. We record it as a request in
        // and leave `sctp_stream_id` unset.
        let negotiated_stream_id = config.negotiated;
        if let Some(sctp_stream_id) = negotiated_stream_id {
            let exists = self.allocations.iter().any(|a| {
                a.sctp_stream_id == Some(sctp_stream_id)
                    || a.negotiated_stream_id == Some(sctp_stream_id)
            });
            assert!(
                !exists,
                "sctp_stream_id ({}) exists already",
                sctp_stream_id
            );
        }

        let alloc = ChannelAllocation {
            id,
            sctp_stream_id: None,
            negotiated_stream_id,
            // The config is none until we confirm we definitely want this channel.
            config: None,
        };

        debug!("Allocate channel id: {:?}", id);
        self.allocations.push(alloc);

        id
    }

    pub fn confirm(&mut self, id: ChannelId, config: ChannelConfig) {
        let a = self
            .allocations
            .iter_mut()
            .find(|a| a.id == id)
            .expect("Entry for issued channel id");
        a.config = Some(config);
    }

    /// For translating sctp stream id to ChannelId. Any event out of sctp goes via this.
    pub fn channel_id_by_stream_id(&self, sctp_stream_id: u16) -> Option<ChannelId> {
        self.allocations
            .iter()
            .find(|a| a.sctp_stream_id == Some(sctp_stream_id))
            .map(|a| a.id)
    }

    /// Look up sctp stream id for channel id.
    pub fn stream_id_by_channel_id(&self, id: ChannelId) -> Option<u16> {
        self.allocations
            .iter()
            .find(|a| a.id == id)
            .and_then(|a| a.sctp_stream_id)
    }

    pub(crate) fn handle_timeout(&mut self, _now: Instant, sctp: &mut RtcSctp, wake: &mut Wake) {
        if !sctp.is_inited() || (!self.need_allocation() && !self.need_open()) {
            // No allocation or open can progress, including ids held for a reset.
            wake.no_events();
            wake.no_timeout();
            return;
        }

        // Allocate sctp channel ids for ones that are missing.
        self.do_allocations(sctp);

        // After do_allocations so we get a channel for any confirmed.
        if self.need_open() {
            // Opening channels drives SCTP and can affect both signals.
            self.open_channels(sctp);
        } else {
            // Allocating ids cannot produce output, but can remove our immediate timeout.
            wake.no_events();
        }
    }

    /// Allocate next available `ChannelId`.
    fn next_channel_id(&mut self) -> ChannelId {
        let id = self.next_channel_id;
        self.next_channel_id += 1;

        ChannelId(id)
    }

    /// Whether a stream id is held back awaiting a reset completion.
    fn is_held(&self, sctp_stream_id: u16) -> bool {
        self.closed_stream_ids.contains(&sctp_stream_id)
    }

    fn need_allocation(&self) -> bool {
        self.allocations.iter().any(|a| {
            a.sctp_stream_id.is_none()
                // A negotiated id that is still held cannot be allocated yet.
                && a.negotiated_stream_id.is_none_or(|want| !self.is_held(want))
        })
    }

    fn need_open(&self) -> bool {
        self.allocations
            .iter()
            .any(|a| a.config.is_some() && a.sctp_stream_id.is_some())
    }

    // Do automatic allocations of sctp stream id.
    fn do_allocations(&mut self, sctp: &RtcSctp) {
        if !self.need_allocation() {
            return;
        }

        // RFC 8831
        // Unless otherwise defined or negotiated, the
        // streams are picked based on the DTLS role (the client picks even
        // stream identifiers, and the server picks odd stream identifiers).
        let base = if sctp.is_client() { 0 } else { 1 };

        let mut taken: Vec<u16> = self
            .allocations
            .iter()
            .filter_map(|a| a.sctp_stream_id)
            .chain(self.closed_stream_ids.iter().copied())
            .collect();

        for a in &mut self.allocations {
            if a.sctp_stream_id.is_some() {
                continue;
            }

            // Out-of-band negotiated. The user picked the id, all we do is wait until
            // no closed generation of it is still awaiting its reset handshake.
            if let Some(want) = a.negotiated_stream_id {
                if taken.contains(&want) {
                    debug!("Negotiated stream id {} still held, retry later", want);
                    continue;
                }

                debug!("Associate negotiated stream id {:?} => {}", a.id, want);
                a.sctp_stream_id = Some(want);
                taken.push(want);
                continue;
            }

            // We need to allocate. Walk this parity's ids until we find a free one.
            //
            // A remote peer can fill every id (sctp-proto enforces no stream-count),
            // the next allocation could overflow, instead fail the allocation gracefully.
            let mut proposed = base;
            while taken.contains(&proposed) {
                match proposed.checked_add(2) {
                    Some(next) => proposed = next,
                    None => break, // id space for this parity is exhausted
                }
            }

            if taken.contains(&proposed) {
                // Exhausted, the loop broke on overflow, leave the channel
                // unallocated, it is retried on the next timeout.
                warn!("SCTP stream id space exhausted, cannot allocate {:?}", a.id);
                continue;
            }

            // Found the next free.
            debug!("Associate stream id {:?} => {}", a.id, proposed);
            a.sctp_stream_id = Some(proposed);
            taken.push(proposed);
        }
    }

    // Actually open channels.
    fn open_channels(&mut self, sctp: &mut RtcSctp) {
        for a in &mut self.allocations {
            // The stream id must be known before the config is taken. A negotiated
            // channel waiting out a previous incarnation's reset has no id yet, and
            // consuming its config here would leave nothing to open it with once the
            // id is released.
            let Some(sctp_stream_id) = a.sctp_stream_id else {
                continue;
            };
            let Some(config) = a.config.take() else {
                continue;
            };

            debug!("Open stream for: {:?}", a.id);
            sctp.open_stream(sctp_stream_id, config);
        }
    }

    pub fn poll_timeout(&self, sctp: &RtcSctp) -> Option<Instant> {
        if sctp.is_inited() && (self.need_allocation() || self.need_open()) {
            Some(already_happened())
        } else {
            None
        }
    }

    pub fn ensure_channel_id_for(&mut self, sctp_stream_id: u16) {
        let exists = self
            .allocations
            .iter()
            .any(|a| a.sctp_stream_id == Some(sctp_stream_id));

        if !exists {
            let id = self.next_channel_id();
            let alloc = ChannelAllocation {
                id,
                sctp_stream_id: Some(sctp_stream_id),
                negotiated_stream_id: None,
                config: None,
            };
            self.allocations.push(alloc);
        }
    }

    // NB: Maybe this should still be &mut self or even `self` to prove singular ownership
    pub fn close_channel(&self, id: ChannelId, sctp: &mut RtcSctp) {
        if let Some(sctp_stream_id) = self
            .allocations
            .iter()
            .find(|a| a.id == id)
            .and_then(|s| s.sctp_stream_id)
        {
            sctp.close_stream(sctp_stream_id);
        }
    }

    /// The reset handshake for a held stream ID completed, release it for
    /// reallocation immediately.
    ///
    /// Completions are reported per stream generation, so the same id can be held
    /// more than once when resets overlap. Release a single generation, the id only
    /// becomes reusable once every one of them has completed.
    pub fn stream_reset_complete(&mut self, stream_id: u16) {
        if let Some(pos) = self.closed_stream_ids.iter().position(|c| *c == stream_id) {
            self.closed_stream_ids.remove(pos);
        }
    }

    /// The association is gone, so no reset can ever complete on it.
    /// The held IDs belong to an association nobody will send on again.
    pub fn association_lost(&mut self) {
        self.closed_stream_ids.clear();
    }

    /// Remove a closed channel.
    pub fn remove_channel(&mut self, id: ChannelId, reset_pending: bool) {
        let stream_id = self
            .allocations
            .iter()
            .find(|a| a.id == id)
            .and_then(|a| a.sctp_stream_id);

        if let (true, Some(stream_id)) = (reset_pending, stream_id) {
            self.closed_stream_ids.push(stream_id);
        }

        self.allocations.retain(|a| a.id != id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::poll::Readiness;

    #[test]
    fn readiness_channel_timeout_without_work_does_not_wake() {
        let now = Instant::now();
        for initialized in [false, true] {
            let mut sctp = RtcSctp::new(crate::io::DATAGRAM_MTU_TARGET, 65536);
            if initialized {
                sctp.init(false, now, None, None).unwrap();
            }
            let mut handler = ChannelHandler::default();
            let before = handler.poll_timeout(&sctp);
            let mut readiness = Readiness::idle();
            handler.handle_timeout(now, &mut sctp, &mut readiness.wake());
            assert!(!readiness.wants_events());
            assert!(!readiness.wants_timeout());
            assert_eq!(handler.poll_timeout(&sctp), before);
            assert!(sctp.poll().is_none());
        }
    }

    #[test]
    fn readiness_channel_allocation_only_moves_timeout() {
        let now = Instant::now();
        let mut sctp = RtcSctp::new(crate::io::DATAGRAM_MTU_TARGET, 65536);
        sctp.init(false, now, None, None).unwrap();
        let mut handler = ChannelHandler::default();
        let id = handler.new_channel(&ChannelConfig::default());
        assert!(handler.poll_timeout(&sctp).is_some());
        let mut readiness = Readiness::idle();
        handler.handle_timeout(now, &mut sctp, &mut readiness.wake());
        assert!(!readiness.wants_events());
        assert!(readiness.wants_timeout());
        assert!(handler.stream_id_by_channel_id(id).is_some());
        assert!(handler.poll_timeout(&sctp).is_none());
        assert!(sctp.poll().is_none());

        // Confirming opens the stream and must retain both signals.
        handler.confirm(id, ChannelConfig::default());
        let mut readiness = Readiness::idle();
        handler.handle_timeout(now, &mut sctp, &mut readiness.wake());
        assert!(readiness.wants_events());
        assert!(readiness.wants_timeout());
        assert!(
            sctp.config(handler.stream_id_by_channel_id(id).unwrap())
                .is_some()
        );
    }

    #[test]
    fn channel_id_allocation() {
        let mut handler = ChannelHandler::default();

        // allocate first channel, get unique id
        assert_eq!(handler.new_channel(&Default::default()), ChannelId(0));

        // allocate second channel, get unique id
        assert_eq!(handler.new_channel(&Default::default()), ChannelId(1));

        // free channel 0, allocate two more channels and verify that the
        // new channels have unique IDs.
        handler.remove_channel(ChannelId(0), true);
        assert_eq!(handler.new_channel(&Default::default()), ChannelId(2));
        assert_eq!(handler.new_channel(&Default::default()), ChannelId(3));
    }

    #[test]
    fn stream_id_held_until_reset_complete() {
        let mut handler = ChannelHandler::default();

        // Simulate two channels with known stream IDs (as if do_allocations ran
        // for a client: even IDs 0, 2).
        let id0 = handler.new_channel(&Default::default());
        let _id1 = handler.new_channel(&Default::default());
        // Manually set stream IDs as do_allocations would.
        handler.allocations[0].sctp_stream_id = Some(0);
        handler.allocations[1].sctp_stream_id = Some(2);

        // Close channel 0 (stream ID 0) with its reset outstanding. It should be held.
        handler.remove_channel(id0, true);
        assert_eq!(handler.closed_stream_ids, vec![0]);

        // Build the taken list as do_allocations does — stream 0 is held.
        let _id2 = handler.new_channel(&Default::default());
        let taken: Vec<u16> = handler
            .allocations
            .iter()
            .filter_map(|a| a.sctp_stream_id)
            .chain(handler.closed_stream_ids.iter().copied())
            .collect();
        assert!(taken.contains(&0), "stream 0 should be held");
        assert!(taken.contains(&2), "stream 2 should be active");

        // The completion signal is the only thing that releases it.
        handler.stream_reset_complete(0);
        assert!(handler.closed_stream_ids.is_empty());

        let taken_after: Vec<u16> = handler
            .allocations
            .iter()
            .filter_map(|a| a.sctp_stream_id)
            .chain(handler.closed_stream_ids.iter().copied())
            .collect();
        assert!(
            !taken_after.contains(&0),
            "stream 0 should be available after reset completion"
        );
    }

    #[test]
    fn negotiated_stream_id_cannot_overlap_held_generation() {
        let sctp = RtcSctp::new(1200, crate::sctp::LOCAL_MAX_MESSAGE_SIZE);
        let mut handler = ChannelHandler::default();
        let config = ChannelConfig {
            negotiated: Some(4),
            ..Default::default()
        };

        // The old generation was granted the id it asked for.
        let old = handler.new_channel(&config);
        handler.do_allocations(&sctp);
        assert_eq!(handler.stream_id_by_channel_id(old), Some(4));

        handler.remove_channel(old, true);
        assert_eq!(handler.closed_stream_ids, vec![4]);

        // A replacement asking for the same id may be declared, but must stall
        // without the id until the old reset completes.
        let _replacement = handler.new_channel(&config);
        handler.do_allocations(&sctp);

        let overlaps_held_generation = handler.allocations.iter().any(|allocation| {
            handler
                .closed_stream_ids
                .iter()
                .any(|closed| allocation.sctp_stream_id == Some(*closed))
        });

        assert!(
            !overlaps_held_generation,
            "a negotiated ID must remain unavailable until its old reset completes"
        );
    }

    #[test]
    fn negotiated_stream_keeps_config_while_waiting_for_reset() {
        let mut sctp = RtcSctp::new(1200, crate::sctp::LOCAL_MAX_MESSAGE_SIZE);
        let mut handler = ChannelHandler::default();
        let config = ChannelConfig {
            label: "replacement".into(),
            negotiated: Some(4),
            ..Default::default()
        };

        handler.closed_stream_ids.push(4);
        let replacement = handler.new_channel(&config);
        handler.confirm(replacement, config);

        handler.do_allocations(&sctp);
        handler.open_channels(&mut sctp);

        let allocation = handler
            .allocations
            .iter()
            .find(|allocation| allocation.id == replacement)
            .unwrap();
        assert!(
            allocation.config.is_some(),
            "a negotiated channel must retain its config while its stream id is held"
        );

        handler.stream_reset_complete(4);
        handler.do_allocations(&sctp);
        handler.open_channels(&mut sctp);

        assert_eq!(
            sctp.config(4).map(|config| config.label.as_str()),
            Some("replacement"),
            "the negotiated channel must open once the old reset completes"
        );
    }

    #[test]
    fn negotiated_stream_waiting_for_reset_does_not_spin_timeout() {
        let now = Instant::now();
        let mut sctp = RtcSctp::new(1200, crate::sctp::LOCAL_MAX_MESSAGE_SIZE);
        sctp.init(true, now, None, None).unwrap();

        let mut handler = ChannelHandler::default();
        let config = ChannelConfig {
            negotiated: Some(4),
            ..Default::default()
        };

        handler.closed_stream_ids.push(4);
        let replacement = handler.new_channel(&config);
        handler.confirm(replacement, config);
        let mut readiness = Readiness::idle();
        handler.handle_timeout(now, &mut sctp, &mut readiness.wake());
        assert!(!readiness.wants_events());
        assert!(!readiness.wants_timeout());

        assert_eq!(
            handler.poll_timeout(&sctp),
            None,
            "a channel that cannot open until network input arrives must not request an immediate timeout"
        );
    }

    #[test]
    fn one_reset_completion_releases_only_one_stream_generation() {
        let mut handler = ChannelHandler::default();

        // sctp-proto 0.10.3 reports reset completion per stream generation.
        // Two outstanding generations of the same stream id therefore need two
        // completion events before the id is reusable.
        handler.closed_stream_ids.extend([4, 4]);

        handler.stream_reset_complete(4);

        assert_eq!(
            handler.closed_stream_ids,
            vec![4],
            "the first completion must not release a newer pending generation"
        );
    }

    #[test]
    fn stream_id_remains_held_without_reset_completion() {
        let sctp = RtcSctp::new(1200, crate::sctp::LOCAL_MAX_MESSAGE_SIZE);
        let mut handler = ChannelHandler::default();

        let id = handler.new_channel(&Default::default());
        handler.allocations[0].sctp_stream_id = Some(0);
        handler.remove_channel(id, true);

        // There is no timer that can release the id.
        for _ in 0..100 {
            handler.do_allocations(&sctp);
        }

        assert_eq!(
            handler.closed_stream_ids,
            vec![0],
            "elapsed time alone cannot make an SCTP stream ID safe to reuse"
        );
    }

    #[test]
    fn stream_id_not_held_when_no_reset_is_pending() {
        let mut handler = ChannelHandler::default();

        let id0 = handler.new_channel(&Default::default());
        handler.allocations[0].sctp_stream_id = Some(0);

        // The stream never made it into the association, so no reset was started and
        // no completion will ever arrive. Holding the id would leak it forever.
        handler.remove_channel(id0, false);
        assert!(handler.closed_stream_ids.is_empty());
    }

    #[test]
    fn association_lost_releases_held_stream_ids() {
        let mut handler = ChannelHandler::default();

        let id0 = handler.new_channel(&Default::default());
        handler.allocations[0].sctp_stream_id = Some(0);
        handler.remove_channel(id0, true);
        assert_eq!(handler.closed_stream_ids, vec![0]);

        // No reset can complete on a dead association, nothing holds the id back.
        handler.association_lost();
        assert!(handler.closed_stream_ids.is_empty());
    }

    /// A remote peer decides which SCTP stream ids exist on the association, and
    /// `ensure_channel_id_for()` gives every one of them an allocation. A peer that
    /// opens every id of our parity therefore leaves `do_allocations()` with no free
    /// id to hand out.
    ///
    /// The allocator walks `proposed += 2` with no upper bound. Past 65535 that is a
    /// debug-build overflow panic, and in release it wraps back to `base` and spins
    /// forever inside `do_allocations()`, hanging the whole event loop.
    ///
    /// Failing to allocate is fine, str0m retries on the next timeout. Panicking or
    /// hanging is not.
    #[test]
    fn stream_id_space_exhausted_by_remote() {
        let sctp = RtcSctp::new(1200, crate::sctp::LOCAL_MAX_MESSAGE_SIZE);
        let base: u16 = if sctp.is_client() { 0 } else { 1 };

        let mut handler = ChannelHandler {
            closed_stream_ids: (base..=u16::MAX).step_by(2).collect(),
            ..Default::default()
        };

        // What the peer did: claim every stream id of our parity. Going through
        // `ensure_channel_id_for()` for each is the realistic route but is quadratic,
        // so seed the equivalent state directly.
        assert_eq!(handler.closed_stream_ids.len(), 32768);

        // Now the local application asks for a data channel.
        let id = handler.new_channel(&Default::default());

        handler.do_allocations(&sctp);

        let alloc = handler
            .allocations
            .iter()
            .find(|a| a.id == id)
            .expect("the allocation entry to still be there");

        assert!(
            alloc.sctp_stream_id.is_none(),
            "no id can be allocated when the peer holds the whole parity"
        );
    }
}
