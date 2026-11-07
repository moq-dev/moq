use std::collections::{HashMap, VecDeque};
use std::fmt::{self};
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use crate::config_mod::RtcpReportIntervals;
use crate::format::CodecConfig;
use crate::format::PayloadParams;
use crate::media::{KeyframeRequest, MID_PROBE, Media, SenderFeedback};
use crate::pacer::QueueState;
use crate::packet::Vp8Patch;
use crate::rtp_::MidRid;
use crate::rtp_::Ssrc;
use crate::rtp_::{Bitrate, Pt};
use crate::rtp_::{DlrrItem, Rtcp, RtcpFb, RtpHeader};
use crate::rtp_::{MediaTime, SenderInfo};
use crate::rtp_::{Mid, Rid, SeqNo};
use crate::util::already_happened;

pub use self::receive::StreamRx;
pub use self::send::{RtpWrite, StreamTx, StreamTxQueueInfo};

mod receive;
pub(crate) mod register;
pub(crate) mod register_nack;
mod rtx_cache;
pub(crate) mod rtx_cache_buf;
mod send;
mod send_queue;
mod send_stats;

pub(crate) use send::{DEFAULT_RTX_CACHE_DURATION, DEFAULT_RTX_RATIO_CAP};

pub(crate) struct StreamTimeoutConfig<'a> {
    pub(crate) codecs: &'a CodecConfig,
    pub(crate) intervals: RtcpReportIntervals,
}

/// Packet of RTP data.
///
/// As emitted by [`Event::RtpPacket`][crate::Event::RtpPacket] when using rtp mode.
#[derive(PartialEq, Eq)]
pub struct RtpPacket {
    /// Extended sequence number to avoid having to deal with ROC.
    pub seq_no: SeqNo,

    /// Extended RTP time in the clock frequency of the codec. To avoid dealing with ROC.
    ///
    /// For a newly scheduled outgoing packet, the clock_rate is not correctly set until
    /// we do the poll_output().
    pub time: MediaTime,

    /// Parsed RTP header.
    pub header: RtpHeader,

    /// RTP payload. This contains no header.
    pub payload: Arc<[u8]>,

    vp8_patch: Option<Vp8Patch>,

    /// str0m server timestamp.
    ///
    /// This timestamp has nothing to do with RTP itself. For outgoing packets, this is when
    /// the packet was first handed over to str0m and enqueued in the outgoing send buffers.
    /// For incoming packets it's the time we received the network packet.
    pub timestamp: Instant,

    /// Sender information from the most recent Sender Report(SR).
    ///
    /// If no Sender Report(SR) has been received or this packet is being sent by str0m this is [`None`].
    pub last_sender_info: Option<SenderInfo>,

    /// Whether this packet can be nacked.
    ///
    /// This is often false for audio, but might also be false for discardable frames when
    /// using temporal encoding as in a VP8 simulcast situation.
    pub(crate) nackable: bool,
}

/// Event when an encoded stream is considered paused/unpaused.
///
/// This means the stream has not received any data for some time (default 1.5 seconds).
#[derive(Debug)]
pub struct StreamPaused {
    /// The main SSRC of the encoded stream that paused.
    pub ssrc: Ssrc,

    /// The mid the encoded stream belongs to.
    pub mid: Mid,

    /// The rid, if the encoded stream has a rid.
    pub rid: Option<Rid>,

    /// Whether the stream is paused or not.
    pub paused: bool,
}

/// 255 is out of range for a real PT, which is 7 bit.
const BLANK_PACKET_DEFAULT_PT: Pt = Pt::new_with_value(255);

impl RtpPacket {
    fn blank() -> RtpPacket {
        RtpPacket {
            seq_no: 0.into(),
            time: MediaTime::from_90khz(0),
            header: RtpHeader {
                payload_type: BLANK_PACKET_DEFAULT_PT,
                ..Default::default()
            },
            payload: Arc::default(), // This payload is never used. See RtpHeader::create_padding_packet
            vp8_patch: None,
            nackable: false,
            last_sender_info: None,
            timestamp: already_happened(),
        }
    }
}

/// Holder of incoming/outgoing encoded streams.
///
/// Each encoded stream is uniquely identified by an SSRC. The concept of mid/rid sits on the Media
/// level together with the ability to translate a mid/rid to an encoded stream.
#[derive(Debug)]
pub(crate) struct Streams {
    /// All incoming encoded streams.
    streams_rx: HashMap<Ssrc, StreamRx>,

    /// Each incoming SSRC is mapped to a Mid/Ssrc. The Ssrc in the value is for the case
    /// where the incoming SSRC is for an RTX and we want the "main".
    rx_lookup: HashMap<Ssrc, RxLookup>,

    /// Time we last cleaned up unused entries from source_keys_rx.
    last_rx_lookup_cleanup: Instant,

    /// All outgoing encoded streams.
    streams_tx: HashMap<Ssrc, StreamTx>,

    /// Non-media padding source. Its sequence counter survives media changes.
    probe_tx: StreamTx,
    probe_media: Option<(Mid, Pt)>,

    /// Local SSRC used before we got any StreamTx. This is used for RTCP if we don't
    /// have any reasonable value to use.
    default_ssrc_tx: Ssrc,

    /// We need to report all RR/SR for a Mid together in one RTCP. This is a dynamic
    /// list that we don't want to allocate on every handle_timeout.
    mids_to_report: Vec<Mid>,

    /// Whether periodic statistics reports are expected to be generated. This informs us on
    /// whether we should be holding onto data needed for those reports or not.
    enable_stats: bool,

    /// Threshold above which an outgoing RTP packet triggers an MTU warning. Used as a
    /// hard cap when selecting RTX cache entries for spurious padding.
    mtu_warn: usize,
    pause_threshold: Duration,
}

/// Delay between cleaning up the RxLookup.
const RX_LOOKUP_CLEANUP_INTERVAL: Duration = Duration::from_millis(10_000);

/// How old an RxLookup entry may be.
const RX_LOOKUP_EXPIRY: Duration = Duration::from_millis(30_000);

#[derive(Debug)]
struct RxLookup {
    mid: Mid,
    main: Ssrc,
    last_used: Instant,
}

impl Streams {
    pub(crate) fn new(enable_stats: bool, mtu_warn: usize, pause_threshold: Duration) -> Self {
        Self {
            streams_rx: Default::default(),
            rx_lookup: Default::default(),
            last_rx_lookup_cleanup: already_happened(),
            streams_tx: Default::default(),
            probe_tx: StreamTx::new_probe(mtu_warn),
            probe_media: None,
            default_ssrc_tx: 0.into(), // this will be changed
            mids_to_report: Vec::with_capacity(10),
            enable_stats,
            mtu_warn,
            pause_threshold,
        }
    }

    pub(crate) fn map_dynamic_by_rid(
        &mut self,
        ssrc: Ssrc,
        midrid: MidRid,
        media: &mut Media,
        payload: PayloadParams,
        is_main: bool,
    ) {
        // This is the point of the function.
        let rid = midrid
            .rid()
            .expect("map_dynamic_by_rid to be called with Rid");

        // Check if the mid/rid combo is not expected
        if !media.rids_rx().contains(rid) {
            trace!("Mid does not expect rid: {} {}", midrid.mid(), rid);
            return;
        }

        let maybe_stream = self.stream_rx_by_midrid(midrid);

        let (ssrc_main, rtx) = if is_main {
            let maybe_rtx = maybe_stream.and_then(|s| s.rtx());
            (ssrc, maybe_rtx)
        } else {
            // This can bail if the main SSRC has not been discovered yet.
            let Some(stream) = maybe_stream else {
                return;
            };
            (stream.ssrc(), Some(ssrc))
        };

        self.map_dynamic_finish(midrid, ssrc_main, rtx, media, payload);
    }

    pub(crate) fn map_dynamic_by_pt(
        &mut self,
        ssrc: Ssrc,
        midrid: MidRid,
        media: &mut Media,
        payload: PayloadParams,
        is_main: bool,
    ) {
        if media.rids_rx().is_specific() {
            trace!(
                "Media expects rid and RTP packet has only mid: {:?}",
                media.rids_rx()
            );
            return;
        }

        let maybe_stream = self.stream_rx_by_midrid(midrid);

        let (ssrc_main, rtx) = if is_main {
            let maybe_rtx = maybe_stream.and_then(|s| s.rtx());
            (ssrc, maybe_rtx)
        } else {
            // This can bail if the main SSRC has not been discovered yet.
            let Some(stream) = maybe_stream else {
                return;
            };
            // The main is the SSRC in the stream already. The incoming is RTX.
            let ssrc_main = stream.ssrc();
            (ssrc_main, Some(ssrc))
        };

        self.map_dynamic_finish(midrid, ssrc_main, rtx, media, payload);
    }

    #[allow(clippy::too_many_arguments)]
    fn map_dynamic_finish(
        &mut self,
        midrid: MidRid,
        ssrc_main: Ssrc,
        rtx: Option<Ssrc>,
        media: &mut Media,
        payload: PayloadParams,
    ) {
        let maybe_stream = self.stream_rx_by_midrid(midrid);

        if let Some(stream) = maybe_stream {
            let ssrc_from = stream.ssrc();
            let rtx_from = stream.rtx();

            // Handle changes in SSRC.
            if ssrc_from != ssrc_main {
                // We got a change in main SSRC for this stream.
                let did_change = self.change_stream_rx_ssrc(ssrc_from, ssrc_main);

                // When the SSRC changes, any payload type on this RID can start with
                // a new sequence number. Reset all depayloaders for the RID.
                if did_change {
                    media.reset_depayloaders_for_rid(midrid.rid());
                }
            }

            // Handle changes in RTX
            if let (Some(rtx_from), Some(rtx_to)) = (rtx_from, rtx) {
                if rtx_from != rtx_to {
                    self.change_stream_rx_rtx(rtx_from, rtx_to);
                }
            }
        }

        // If we don't have an RTX PT configured, we don't want NACK.
        let suppress_nack = payload.resend.is_none();

        // If stream already exists, this might only "fill in" the RTX.
        self.expect_stream_rx(ssrc_main, rtx, midrid, suppress_nack);
    }

    pub fn expect_stream_rx(
        &mut self,
        ssrc: Ssrc,
        rtx: Option<Ssrc>,
        midrid: MidRid,
        suppress_nack: bool,
    ) -> &mut StreamRx {
        let stream = self
            .streams_rx
            .entry(ssrc)
            .or_insert_with(|| StreamRx::new(ssrc, midrid, suppress_nack, self.pause_threshold));

        if let Some(rtx) = rtx {
            stream.maybe_reset_rtx(rtx);
        }

        stream
    }

    pub fn remove_stream_rx(&mut self, ssrc: Ssrc) -> bool {
        let stream = self.streams_rx.remove(&ssrc);
        let existed = stream.is_some();

        self.rx_lookup.retain(|k, l| *k != ssrc && l.main != ssrc);

        existed
    }

    pub fn declare_stream_tx(
        &mut self,
        ssrc: Ssrc,
        rtx: Option<Ssrc>,
        midrid: MidRid,
    ) -> &mut StreamTx {
        self.streams_tx
            .entry(ssrc)
            .or_insert_with(|| StreamTx::new(ssrc, rtx, midrid, self.enable_stats, self.mtu_warn))
    }

    pub fn remove_stream_tx(&mut self, ssrc: Ssrc) -> bool {
        self.streams_tx.remove(&ssrc).is_some()
    }

    pub(crate) fn local_sender_ssrcs(&self) -> Vec<Ssrc> {
        self.streams_tx
            .values()
            .flat_map(|s| std::iter::once(s.ssrc()).chain(s.rtx()))
            .collect()
    }

    pub(crate) fn reset_send_buffers(&mut self) {
        for stream in self.streams_tx.values_mut() {
            stream.reset_buffers();
        }
    }

    pub fn stream_rx(&mut self, ssrc: &Ssrc) -> Option<&mut StreamRx> {
        self.streams_rx.get_mut(ssrc)
    }

    pub fn stream_tx(&mut self, ssrc: &Ssrc) -> Option<&mut StreamTx> {
        self.streams_tx.get_mut(ssrc)
    }

    /// Lookup the "main" SSRC and mid for a given SSRC(main or RTX).
    pub(crate) fn mid_ssrc_rx_by_ssrc_or_rtx(
        &mut self,
        now: Instant,
        ssrc: Ssrc,
    ) -> Option<(Mid, Ssrc)> {
        // A direct hit on SSRC is to prefer. The idea is that mid/rid are only sent
        // for the initial x seconds and then we start using SSRC only instead.
        if let Some(r) = self.rx_lookup.get_mut(&ssrc) {
            r.last_used = now;
            return Some((r.mid, r.main));
        }

        let maybe_stream = self.stream_rx_by_ssrc_or_rtx(ssrc);
        if let Some(stream) = maybe_stream {
            let mid = stream.mid();
            let ssrc_main = stream.ssrc();

            self.rx_lookup.insert(
                ssrc,
                RxLookup {
                    mid,
                    main: ssrc_main,
                    last_used: now,
                },
            );

            return Some((mid, ssrc_main));
        }

        None
    }

    pub(crate) fn regular_feedback_at(&self, i: RtcpReportIntervals) -> Option<Instant> {
        let r = self
            .streams_rx
            .values()
            .filter(|s| !s.ssrc().is_probe())
            .map(|s| s.receiver_report_at(i));
        let s = self.streams_tx.values().map(|s| s.sender_report_at(i));
        r.chain(s).min()
    }

    pub(crate) fn paused_at(&self) -> Option<Instant> {
        self.streams_rx.values().find_map(|s| s.paused_at())
    }

    pub(crate) fn send_stream(&self) -> Option<Instant> {
        if self.streams_tx.values().any(|s| s.need_timeout()) {
            Some(already_happened())
        } else {
            None
        }
    }

    pub(crate) fn pending_request(&self) -> Option<Instant> {
        if self.streams_rx.values().any(|s| s.has_pending_request()) {
            Some(already_happened())
        } else {
            None
        }
    }

    pub(crate) fn handle_timeout(
        &mut self,
        now: Instant,
        sender_ssrc: Ssrc,
        do_nack: bool,
        medias: &[Media],
        config: StreamTimeoutConfig<'_>,
        feedback: &mut VecDeque<Rtcp>,
    ) {
        let StreamTimeoutConfig { codecs, intervals } = config;

        self.mids_to_report.clear(); // Clear for checking StreamRx.
        for stream in self.streams_rx.values() {
            if stream.need_rr(now, intervals) {
                self.mids_to_report.push(stream.mid());
            }
        }

        for stream in self.streams_rx.values_mut() {
            stream.maybe_create_keyframe_request(sender_ssrc, feedback);
            stream.maybe_create_remb_request(sender_ssrc, feedback);

            // All StreamRx belonging to the same Mid are reported together.
            if self.mids_to_report.contains(&stream.mid()) {
                stream.create_rr_and_update(now, sender_ssrc, feedback);
            }

            if do_nack {
                stream.maybe_create_nack(now, sender_ssrc, feedback);
            }

            stream.handle_timeout(now);
        }

        self.mids_to_report.clear(); // start over for StreamTx.
        for stream in self.streams_tx.values() {
            if stream.need_sr(now, intervals) {
                self.mids_to_report.push(stream.mid());
            }
        }

        for stream in self.streams_tx.values_mut() {
            let mid = stream.mid();

            // All StreamTx belonging to the same Mid are reported together.
            if self.mids_to_report.contains(&mid) {
                stream.create_sr_and_update(now, feedback);
            }

            // Finding the first (main) PT that also has RTX for the Media is expensive,
            // this closure is run only when needed.
            // The unwrap is okay because we cannot have StreamTx with a Mid without the corresponding Media.
            let get_media = move || (medias.iter().find(|m| m.mid() == mid).unwrap(), codecs);

            stream.handle_timeout(now, get_media);
        }

        if now > self.rx_lookup_at() {
            self.rx_lookup
                .retain(|_, l| now - l.last_used <= RX_LOOKUP_EXPIRY);
            self.last_rx_lookup_cleanup = now;
        }
    }

    pub(crate) fn poll_keyframe_request(&mut self) -> Option<KeyframeRequest> {
        self.streams_tx.values_mut().find_map(|s| {
            let kind = s.poll_keyframe_request()?;
            Some(KeyframeRequest {
                mid: s.mid(),
                rid: s.rid(),
                kind,
            })
        })
    }

    pub(crate) fn poll_sender_feedback(&mut self) -> Option<SenderFeedback> {
        self.streams_rx.values_mut().find_map(|s| {
            let (sender_info, at) = s.poll_sender_info()?;

            Some(SenderFeedback {
                mid: s.mid(),
                rid: s.rid(),
                received_at: at,
                sender_info,
            })
        })
    }

    pub(crate) fn poll_remb_request(&mut self) -> Option<(Mid, Bitrate)> {
        self.streams_tx
            .values_mut()
            .find_map(|s| s.poll_remb_request().map(|b| (s.mid(), b)))
    }

    pub(crate) fn poll_stream_paused(&mut self) -> Option<StreamPaused> {
        self.streams_rx.values_mut().find_map(|s| s.poll_paused())
    }

    pub(crate) fn has_stream_rx(&self, ssrc: Ssrc) -> bool {
        self.streams_rx.contains_key(&ssrc)
    }

    pub(crate) fn has_stream_tx(&self, ssrc: Ssrc) -> bool {
        self.streams_tx.contains_key(&ssrc)
    }

    pub(crate) fn handle_dlrr(&mut self, now: Instant, dlrr: DlrrItem) {
        let is_default_ssrc = !self.default_ssrc_tx.is_probe() && dlrr.ssrc == self.default_ssrc_tx;
        if !is_default_ssrc && !self.has_stream_tx(dlrr.ssrc) {
            trace!("Ignoring DLRR for unknown local SSRC: {}", dlrr.ssrc);
            return;
        }

        // DLRR identifies our RRTR sender, not a remote media SSRC. The measured
        // round trip is shared by receive streams on this peer's transport.
        for stream in self.streams_rx.values_mut() {
            stream.handle_rtcp(now, RtcpFb::DlrrItem(dlrr));
        }
    }

    pub(crate) fn streams_rx(&mut self) -> impl Iterator<Item = &mut StreamRx> {
        self.streams_rx.values_mut()
    }

    pub(crate) fn streams_tx(&mut self) -> impl Iterator<Item = &mut StreamTx> {
        self.streams_tx.values_mut()
    }

    pub(crate) fn ssrcs_tx(&self, mid: Mid) -> Vec<(Ssrc, Option<Ssrc>)> {
        self.streams_tx
            .values()
            .filter(|s| s.mid() == mid)
            .map(|s| (s.ssrc(), s.rtx()))
            .collect()
    }

    pub(crate) fn new_ssrc(&self) -> Ssrc {
        loop {
            // This new guarantees we never get SSRC 0 (reserved for BWE probes)
            let ssrc = Ssrc::new();

            let has_ssrc = self.has_stream_rx(ssrc) || self.has_stream_tx(ssrc);

            if has_ssrc {
                continue;
            }

            // Need to check RTX as well.
            let has_rtx_rx = self.streams_rx.values().any(|s| s.rtx() == Some(ssrc));
            if has_rtx_rx {
                continue;
            }

            let has_rtx_tx = self.streams_tx.values().any(|s| s.rtx() == Some(ssrc));
            if has_rtx_tx {
                continue;
            }

            // Not used
            break ssrc;
        }
    }

    pub fn new_ssrc_pair(&self) -> (Ssrc, Ssrc) {
        let ssrc = self.new_ssrc();

        let rtx = loop {
            let proposed = self.new_ssrc();
            // Avoid clashing with just allocated main SSRC.
            if proposed != ssrc {
                break proposed;
            }
        };

        (ssrc, rtx)
    }

    pub(crate) fn first_ssrc_remote(&self) -> Ssrc {
        *self.streams_rx.keys().next().unwrap_or(&0.into())
    }

    pub(crate) fn first_ssrc_local(&mut self) -> Ssrc {
        if let Some(ssrc) = self.streams_tx.keys().next() {
            // If there is some local Tx SSRC, use that.
            *ssrc
        } else {
            // Fallback in case we don't have any Tx SSRC.
            if *self.default_ssrc_tx == 0 {
                // Do not use 0, allocate one that is not in session already.
                self.default_ssrc_tx = self.new_ssrc();
            }
            self.default_ssrc_tx
        }
    }

    /// Bind the probe source only while a cluster is authorized. Changing the
    /// binding drops queued padding but preserves the SSRC's SRTP sequence.
    pub(crate) fn set_probe_media(&mut self, media: Option<(Mid, Pt)>) {
        if media == self.probe_media {
            return;
        }
        self.probe_tx.reset_buffers();
        if let Some((mid, pt)) = media {
            self.probe_tx.set_probe_media(mid, pt);
        }
        self.probe_media = media;
    }

    pub(crate) fn send_queue_states(
        &mut self,
        now: Instant,
        mut can_probe: impl FnMut(Mid, Pt) -> bool,
    ) -> impl Iterator<Item = QueueState> {
        // Snapshot each stream once, preferring media/RTX padding over the
        // fallback. No extra stream or retransmission-queue scan is needed.
        let mut media_padding = false;
        let mut probe_queue = self.probe_media.map(|_| {
            let mut queue = self.probe_tx.queue_state(now);
            queue.midrid = MidRid(MID_PROBE, None);
            queue
        });
        let mut streams = self.streams_tx.values_mut();
        std::iter::from_fn(move || {
            if let Some(stream) = streams.next() {
                let mut queue = stream.queue_state(now);
                if probe_queue.is_some() && queue.use_for_padding {
                    // Only negotiated feedback sources can provide probe padding.
                    // Ordinary padding keeps its existing eligibility.
                    queue.use_for_padding = stream
                        .padding_pt()
                        .is_some_and(|pt| can_probe(stream.mid(), pt));
                }
                media_padding |= queue.use_for_padding;
                return Some(queue);
            }
            let mut queue = probe_queue.take()?;
            queue.use_for_padding = !media_padding;
            Some(queue)
        })
    }

    pub(crate) fn stream_tx_by_midrid(&mut self, midrid: MidRid) -> Option<&mut StreamTx> {
        if midrid == MidRid(MID_PROBE, None) {
            self.probe_media?;
            return Some(&mut self.probe_tx);
        }
        self.streams_tx.values_mut().find(|s| s.is_midrid(midrid))
    }

    pub(crate) fn stream_rx_by_midrid(&mut self, midrid: MidRid) -> Option<&mut StreamRx> {
        self.streams_rx.values_mut().find(|s| s.is_midrid(midrid))
    }

    pub(crate) fn remove_streams_by_mid(&mut self, mid: Mid) {
        self.streams_tx.retain(|_, s| s.mid() != mid);
        self.streams_rx.retain(|_, s| s.mid() != mid);
        self.rx_lookup.retain(|_, v| v.mid != mid);
    }

    /// An iterator over all the tx streams for a given mid.
    pub(crate) fn streams_tx_by_mid(&mut self, mid: Mid) -> impl Iterator<Item = &mut StreamTx> {
        self.streams_tx.values_mut().filter(move |s| s.mid() == mid)
    }

    /// An iterator over all the rx streams for a given mid.
    pub(crate) fn streams_rx_by_mid(&mut self, mid: Mid) -> impl Iterator<Item = &mut StreamRx> {
        self.streams_rx.values_mut().filter(move |s| s.mid() == mid)
    }

    pub(crate) fn reset_buffers_tx(&mut self, mid: Mid) {
        for s in self.streams_tx_by_mid(mid) {
            s.reset_buffers();
        }
    }

    pub(crate) fn reset_buffers_rx(
        &mut self,
        mid: Mid,
        max_seq_lookup: impl Fn(Ssrc) -> Option<SeqNo>,
    ) {
        for s in self.streams_rx_by_mid(mid) {
            s.reset_buffers(&max_seq_lookup);
        }
    }

    pub(crate) fn change_stream_rx_ssrc(&mut self, ssrc_from: Ssrc, ssrc_to: Ssrc) -> bool {
        // This unwrap is OK, because we can't call change_stream_rx_ssrc without first
        // knowing there is such a StreamRx.
        let maybe_change = self.streams_rx.get_mut(&ssrc_from).unwrap();

        // The StreamRx is allowed to not change the SSRC in case it is switching back
        // to the previous SSRC. This is to avoid flapping in case RTP packets arrive
        // out of order.
        let did_change = maybe_change.change_ssrc(ssrc_to);

        if did_change {
            // Unwrap is OK, see above.
            let to_change = self.streams_rx.remove(&ssrc_from).unwrap();

            // Reinsert under new SSRC key.
            self.streams_rx.insert(ssrc_to, to_change);

            // Remove previous mappings for the SSRC
            self.rx_lookup
                .retain(|k, l| *k != ssrc_from && l.main != ssrc_from);
        }

        did_change
    }

    fn change_stream_rx_rtx(&mut self, rtx_from: Ssrc, rtx_to: Ssrc) {
        // Remove the SSRC mapping
        self.rx_lookup.remove(&rtx_from);

        let Some(to_change) = self
            .streams_rx
            .values_mut()
            .find(|s| s.rtx() == Some(rtx_from))
        else {
            // If there's no main stream associated with the RTX our job is done.
            return;
        };

        to_change.maybe_reset_rtx(rtx_to);
    }

    fn stream_rx_by_ssrc_or_rtx(&self, ssrc: Ssrc) -> Option<&StreamRx> {
        self.streams_rx
            .values()
            .find(|s| s.ssrc() == ssrc || s.rtx() == Some(ssrc))
    }

    /// Whether any receive stream currently needs NACK reports (see [`StreamRx::nack_enabled`]).
    ///
    /// Not cached: the answer changes with pause state and pending NACK retries.
    pub(crate) fn any_nack_enabled(&self) -> bool {
        self.streams_rx.values().any(|s| s.nack_enabled())
    }

    fn rx_lookup_at(&self) -> Instant {
        self.last_rx_lookup_cleanup + RX_LOOKUP_CLEANUP_INTERVAL
    }
}

impl fmt::Debug for RtpPacket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RtpPacket")
            .field("seq_no", &self.seq_no)
            .field("time", &self.time)
            .field("header", &self.header)
            .field("payload", &self.payload.len())
            .field("nackable", &self.nackable)
            .field("timestamp", &self.timestamp)
            .finish()
    }
}
