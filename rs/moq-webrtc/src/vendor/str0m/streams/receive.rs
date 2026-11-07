use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config_mod::RtcpReportIntervals;
use crate::media::KeyframeRequestKind;
use crate::packet::MAX_RED_RECOVERY_DEPTH;
use crate::rtp_::MidRid;
use crate::rtp_::SeqNoWindow;
use crate::rtp_::{Bitrate, DlrrItem, ExtendedReport, extend_u32};
use crate::rtp_::{Fir, FirEntry, Frequency, MediaTime, Remb};
use crate::rtp_::{Mid, Pli, Pt, ReceiverReport};
use crate::rtp_::{ReportBlock, ReportList, Rid, Rrtr, Rtcp};
use crate::rtp_::{RtcpFb, RtpHeader, SenderInfo, SeqNo};
use crate::rtp_::{SdesType, Ssrc};
use crate::stats::{MediaIngressStats, RemoteEgressStats, StatsSnapshot};
use crate::util::{InstantExt, SystemTimeExt};
use crate::util::{already_happened, calculate_rtt};

use super::RtpPacket;
use super::StreamPaused;
use super::register::ReceiverRegister;

/// How many recent packets to remember for placing RED redundant blocks. Must cover the
/// recovery depth plus some reordering slack.
const RED_RECENT_PACKETS: usize = 32;

/// Incoming encoded stream.
///
/// A stream is a primary SSRC + optional RTX SSRC.
///
/// This is RTP level API. For frame level API see [`Rtc::writer`][crate::Rtc::writer].
#[derive(Debug)]
pub struct StreamRx {
    /// Unique idenfier of the remote stream.
    ///
    /// If the remote changes the SSRC, we will create a new stream, not change this id.
    ssrc: Ssrc,

    /// Identifier of a resend (RTX) stream. This can be set later, once we discover it.
    rtx: Option<Ssrc>,

    /// Previous main SSRC. This is to ensure we never go "backwards" in terms
    /// of changing SSRC (for FF).
    previous_ssrc: Option<Ssrc>,

    /// The Media mid/rid this stream belongs to.
    midrid: MidRid,

    /// Incoming CNAME in Sdes reports.
    cname: Option<String>,

    /// Whether we explicitly want to supress NACK sending. This is normally done by not
    /// setting an RTX, however this can be toggled off manually despite RTX being there.
    ///
    /// This is also set to true if the SDP negotiation disables RTX.
    ///
    /// Defaults to false.
    suppress_nack: bool,

    /// Timestamp when we got some indication of remote using this stream.
    last_used: Instant,

    /// Last seen pt and clock_rate in
    last_clock_rate: Option<(Pt, Frequency)>,

    /// Last received sender info.
    sender_info: Option<LastSenderInfo>,

    /// ROC to reset with on next incoming packet.
    reset_roc: Option<u64>,

    /// Register of received packets. For NACK handling.
    ///
    /// Set on first ever packet.
    register: Option<ReceiverRegister>,

    /// Sequence and replay tracking for RTX. NACKs and reception reports use the main register.
    ///
    /// Set on the first authenticated RTX packet or restored from remembered sequence history.
    register_rtx: Option<SeqNoWindow>,

    /// The most recent main-stream packets as `(seq_no, extended rtp time)`, used to place
    /// RFC 2198 RED redundant blocks (which carry a timestamp, not a seq no) at the right
    /// sequence number. Bounded by `RED_RECENT_PACKETS`.
    red_recent: VecDeque<(SeqNo, u64)>,

    /// Last observed media time in an RTP packet.
    last_time: Option<MediaTime>,

    /// If we have a pending keyframe request to send.
    pending_request_keyframe: Option<KeyframeRequestKind>,

    /// If we have a pending REMB request to send.
    pending_request_remb: Option<Bitrate>,

    /// Sequence number of the next FIR.
    fir_seq_no: u8,

    /// Last time we produced regular feedback RR.
    last_receiver_report: Instant,

    /// Statistics of incoming data.
    stats: StreamRxStats,

    /// When we need to evaluate the paused state.
    ///
    /// now + pause_threshold
    check_paused_at: Option<Instant>,

    /// Whether we consider this StreamRx paused.
    ///
    /// A stream is considered paused if it has received no packets for some (configurable) duration.
    /// This defaults to 1.5s.
    paused: bool,

    /// Whether we need to emit a paused event for the current paused state.
    need_paused_event: bool,

    /// The configured threshold before considering the lack of packets as going into paused.
    pause_threshold: Duration,
}

/// The last sender info we recieved.
#[derive(Debug)]
pub(crate) struct LastSenderInfo {
    /// When this SenderInfo was received.
    received_at: Instant,
    /// The sender info itself.
    info: SenderInfo,
    /// Whether we have emitted it yet via `poll_sender_info`
    emitted: bool,
}

/// Holder of stats.
#[derive(Debug, Default)]
pub(crate) struct StreamRxStats {
    /// count of bytes received, including retransmissions
    bytes: u64,
    /// count of packets received, including retransmissions
    packets: u64,
    /// count of FIR requests sent
    firs: u64,
    /// count of PLI requests sent
    plis: u64,
    /// count of NACKs sent
    nacks: u64,
    /// interarrival jitter (RTP timestamp units) from the last RR we sent
    jitter: u32,
    /// round trip time from the last DLRR, if any
    rtt: Option<Duration>,
    /// fraction of packets lost from the last RR, if any
    loss: Option<f32>,
}

impl StreamRx {
    pub(crate) fn new(
        ssrc: Ssrc,
        midrid: MidRid,
        suppress_nack: bool,
        pause_threshold: Duration,
    ) -> Self {
        debug!("Create StreamRx for SSRC: {}", ssrc);

        StreamRx {
            ssrc,
            rtx: None,
            previous_ssrc: None,
            midrid,
            cname: None,
            suppress_nack,
            last_used: already_happened(),
            last_clock_rate: None,
            sender_info: None,
            reset_roc: None,
            register: None,
            register_rtx: None,
            red_recent: VecDeque::new(),
            last_time: None,
            pending_request_keyframe: None,
            pending_request_remb: None,
            fir_seq_no: 0,
            last_receiver_report: already_happened(),
            stats: StreamRxStats::default(),
            check_paused_at: None,
            paused: true,
            need_paused_event: false,
            pause_threshold,
        }
    }

    /// The (primary) SSRC of this encoded stream.
    pub fn ssrc(&self) -> Ssrc {
        self.ssrc
    }

    /// The resend (RTX) SSRC of this encoded stream.
    pub fn rtx(&self) -> Option<Ssrc> {
        self.rtx
    }

    /// Mid for this stream.
    ///
    /// In SDP this corresponds to m-line and "Media".
    pub fn mid(&self) -> Mid {
        self.midrid.mid()
    }

    /// Rid for this stream.
    ///
    /// This is used to separate streams with the same [`Mid`] when using simulcast.
    pub fn rid(&self) -> Option<Rid> {
        self.midrid.rid()
    }

    /// CNAME as sent by remote peer in a Sdes.
    ///
    /// The value is None until we receive a first report with the value set.
    pub fn cname(&self) -> Option<&str> {
        self.cname.as_deref()
    }

    /// Set threshold duration for emitting the paused event.
    ///
    /// This event is emitted when no packet have received for this duration.
    pub fn set_pause_threshold(&mut self, t: Duration) {
        self.pause_threshold = t;
    }

    /// The last time we received a packet.
    ///
    /// Resets if the SSRC changes.
    pub fn last_time(&self) -> Option<MediaTime> {
        self.last_time
    }

    /// Request a keyframe for an incoming encoded stream.
    ///
    /// * SSRC the identifier of the remote encoded stream to request a keyframe for.
    /// * kind PLI or FIR.
    pub fn request_keyframe(&mut self, kind: KeyframeRequestKind) {
        self.pending_request_keyframe = Some(kind);
    }

    /// Request max recv bitrate for an incoming encoded stream.
    ///
    /// * bitrate Bitrate.
    pub fn request_remb(&mut self, bitrate: Bitrate) {
        self.pending_request_remb = Some(bitrate);
    }

    /// Suppress NACK sending.
    ///
    /// Normally NACK is disabled by not having an RTX SSRC set. In some situations it might be
    /// desirable to manually suppress NACK sending regardless of RTX setting.
    pub fn suppress_nack(&mut self, suppress: bool) {
        self.suppress_nack = suppress;
    }

    fn is_audio(&self) -> bool {
        self.rtx.is_none() // this is maybe not correct, but it's all we got.
    }

    pub(crate) fn receiver_report_at(&self, intervals: RtcpReportIntervals) -> Instant {
        self.last_receiver_report + intervals.for_audio(self.is_audio())
    }

    pub(crate) fn handle_rtcp(&mut self, now: Instant, fb: RtcpFb) {
        use RtcpFb::*;
        match fb {
            SenderInfo(v) => {
                self.set_sender_info(now, v);
            }
            SourceDescription(v) => {
                for (sdes, st) in v.values {
                    if sdes == SdesType::CNAME {
                        if st.is_empty() {
                            // In simulcast, chrome doesn't send the SSRC lines, but
                            // expects us to infer that from rtp headers. It does
                            // however send the SourceDescription RTCP with an empty
                            // string CNAME. ¯\_(ツ)_/¯
                            return;
                        }

                        // Here we _could_ check CNAME here matches something. But
                        // CNAMEs are a bit unfashionable.
                        self.cname = Some(st);
                        return;
                    }
                }
            }
            DlrrItem(v) => {
                self.set_dlrr_item(now, v);
            }
            Goodbye(_v) => {
                // We get Goodbye at weird times, like SDP renegotiation, which makes
                // pausing on the BYE not a good idea. Chrome also reuses the SSRC it
                // just sent BYE on. Very not helpful.
            }
            _ => {}
        }
    }

    fn set_sender_info(&mut self, now: Instant, mut info: SenderInfo) {
        // Extend the incoming time given our knowledge of last time.
        let extended = {
            let prev = self
                .sender_info
                .as_ref()
                .map(|last| last.info.rtp_time.numer());
            let r_u32 = info.rtp_time.numer() as u32;
            extend_u32(prev, r_u32)
        };

        // The MediaTime has a base 1 after being parsed. At this point
        // we know whether it's audio or video and set the base accordingly.
        let clock_rate = self
            .last_clock_rate
            .map(|(_, r)| r)
            .unwrap_or(Frequency::SECONDS);

        // Clock rate is that of the last received packet.
        info.rtp_time = MediaTime::new(extended, clock_rate);

        self.sender_info = Some(LastSenderInfo {
            received_at: now,
            info,
            emitted: false,
        });
    }

    fn set_dlrr_item(&mut self, now: Instant, dlrr: DlrrItem) {
        let ntp_time = now.to_ntp_duration();
        let rtt = calculate_rtt(ntp_time, dlrr.last_rr_delay, dlrr.last_rr_time);
        self.stats.rtt = rtt;
    }

    pub(crate) fn paused_at(&self) -> Option<Instant> {
        self.check_paused_at
    }

    pub(crate) fn has_pending_request(&self) -> bool {
        self.pending_request_keyframe.is_some() || self.pending_request_remb.is_some()
    }

    pub(crate) fn handle_timeout(&mut self, now: Instant) {
        // No scheduled paused check?
        if self.check_paused_at.is_none() {
            return;
        }

        // Not reached scheduled paused check?
        if Some(now) < self.check_paused_at {
            return;
        }

        // Every update() schedules a paused check in the future. If we have reached that
        // future we have implicitly also paused.
        self.check_paused_at = None;

        self.paused = true;
        self.need_paused_event = true;
    }

    pub(crate) fn extend_seq(
        &mut self,
        header: &RtpHeader,
        is_repair: bool,
        max_seq_lookup: impl Fn(Ssrc) -> Option<SeqNo>,
    ) -> SeqNo {
        // RTX has its own sequence series and only needs a replay window.
        if is_repair {
            if self.register_rtx.is_none() {
                self.register_rtx = max_seq_lookup(header.ssrc).map(SeqNoWindow::new_after);
            }
            return header.sequence_number(self.register_rtx.as_ref().map(|r| r.max_seq()));
        }

        let register = self
            .register
            .get_or_insert_with(|| ReceiverRegister::new(max_seq_lookup(header.ssrc)));

        // If the user has called `reset_seq_no`, this is the time to handle it, but only
        // if the incoming packet is for main (not repair).
        let mut reset_seq_no = None;
        if let Some(reset_roc) = self.reset_roc.take() {
            let s: SeqNo = (reset_roc << 16 | header.sequence_number as u64).into();
            reset_seq_no = Some(s);
        }

        if let Some(reset_seq_no) = reset_seq_no {
            reset_seq_no
        } else {
            header.sequence_number(register.max_seq())
        }
    }

    pub(crate) fn is_new_packet(&self, is_repair: bool, seq_no: SeqNo) -> bool {
        if is_repair {
            self.register_rtx.as_ref().is_none_or(|r| r.accepts(seq_no))
        } else {
            // extend_seq() initializes the main register before this check.
            self.register.as_ref().unwrap().accepts(seq_no)
        }
    }

    fn remember_recent(&mut self, seq_no: SeqNo, time: u64) {
        self.red_recent.push_back((seq_no, time));
        while self.red_recent.len() > RED_RECENT_PACKETS {
            self.red_recent.pop_front();
        }
    }

    /// Work out which (still missing) sequence number a RED redundant block belongs to.
    ///
    /// RFC 2198 blocks carry a timestamp offset, not a sequence number, and the frame duration
    /// is neither signalled nor constant (Opus 10/20/40/60 ms, DTX, CN interleaved on the same
    /// SSRC). Rather than guess a duration from the offsets, bracket the block's time between the
    /// nearest received packets `L` (latest with time before) and `U` (earliest with time after)
    /// and look at the missing sequence numbers between them:
    ///
    /// * exactly one missing: that is the block, whatever the durations were — this case is
    ///   exact;
    /// * several missing: only if `L..U` is uniformly spaced and the block time lands exactly on
    ///   one of the missing slots; otherwise give up. This case *assumes* a constant frame
    ///   duration across the gap, which holds for the fixed-ptime senders seen in practice
    ///   (WebRTC uses 20 ms). Under variable Opus ptime the derived slot can be a neighbouring
    ///   lost sequence number rather than the block's true one, so recovered audio may land one
    ///   or two frames off.
    ///
    /// The bound this always keeps is that a candidate must be a currently *missing* sequence
    /// number (`register.accepts`): recovery only ever fills a hole, and never overwrites a
    /// correctly received frame — the worst case above only reorders audio among frames that
    /// were all lost anyway. `carrier` is the packet the block arrived in; recovery is limited
    /// to `MAX_RED_RECOVERY_DEPTH` packets behind it.
    pub(crate) fn red_locate_seq(&self, carrier: SeqNo, block_time: u64) -> Option<SeqNo> {
        let register = self.register.as_ref()?;

        // A packet (received or already recovered) at exactly this time: nothing to recover.
        if self.red_recent.iter().any(|(_, t)| *t == block_time) {
            return None;
        }

        let below = self
            .red_recent
            .iter()
            .filter(|(_, t)| *t < block_time)
            .max_by_key(|(s, _)| *s)?;
        let above = self
            .red_recent
            .iter()
            .filter(|(_, t)| *t > block_time)
            .min_by_key(|(s, _)| *s)?;

        let (l_seq, l_time) = (*below.0, below.1);
        let (u_seq, u_time) = (*above.0, above.1);

        // Bound the scan: the block must be within recovery depth of the carrier, which also
        // keeps `l_seq..u_seq` small. `checked_sub` guards a stream whose timestamps don't
        // follow its sequence numbers.
        let depth_ok = (*carrier)
            .checked_sub(l_seq)
            .is_some_and(|d| d <= MAX_RED_RECOVERY_DEPTH + 1);
        if u_seq <= l_seq + 1 || !depth_ok {
            return None;
        }

        let mut missing = (l_seq + 1..u_seq)
            .map(SeqNo::from)
            .filter(|s| register.accepts(*s));

        let first = missing.next()?;
        let candidate = if missing.next().is_none() {
            // Exactly one hole between two received packets that bracket the block in time.
            first
        } else {
            // Several holes: only a uniformly spaced span lets us tell which one.
            let span_seq = u_seq - l_seq;
            let span_time = u_time - l_time;
            if span_time % span_seq != 0 {
                return None;
            }
            let d = span_time / span_seq;
            let back = block_time - l_time;
            if !back.is_multiple_of(d) {
                return None;
            }
            let seq: SeqNo = (l_seq + back / d).into();
            if !register.accepts(seq) {
                return None;
            }
            seq
        };

        (*carrier)
            .checked_sub(*candidate)
            .is_some_and(|d| d <= MAX_RED_RECOVERY_DEPTH)
            .then_some(candidate)
    }

    /// Record that `seq_no` was rebuilt from RED redundancy. It was never on the wire, so it is
    /// not counted as received (reception reports still show the loss), but it must neither be
    /// NACKed nor recovered again.
    pub(crate) fn mark_red_recovered(&mut self, seq_no: SeqNo, time: u64) {
        if let Some(register) = &mut self.register {
            register.mark_recovered(seq_no);
        }
        self.remember_recent(seq_no, time);
    }

    pub(crate) fn update_register(
        &mut self,
        now: Instant,
        header: &RtpHeader,
        clock_rate: Frequency,
        is_repair: bool,
        seq_no: SeqNo,
    ) -> RegisterUpdateReceipt {
        self.last_used = now;

        let was_paused = self.paused;
        if was_paused {
            self.paused = false;
            self.need_paused_event = true;
        }
        self.check_paused_at = Some(now + self.pause_threshold);

        let is_new_packet = if is_repair {
            let is_new = self.is_new_packet(true, seq_no);
            self.register_rtx
                .get_or_insert_with(|| SeqNoWindow::new(seq_no))
                .update(seq_no);
            is_new
        } else {
            // extend_seq() initializes the main register before this update.
            self.register
                .as_mut()
                .unwrap()
                .update(seq_no, now, header.timestamp, clock_rate.get())
        };

        // Get the previous time for comparison
        let previous_time = self.last_time.map(|t| t.numer());

        // Calculate the extended timestamp
        let mut time_u32 = extend_u32(previous_time, header.timestamp);

        if was_paused && Some(time_u32) < previous_time {
            // In 32-bit RTP timestamps, adding 2^31 (MAX/2) flips to the other half of timestamp space
            // This forces extend_u32 to produce a value in the next cycle
            const HALF_CYCLE: u32 = 1u32 << 31;
            let adjusted_ts = header.timestamp.wrapping_add(HALF_CYCLE);

            // Recalculate extended timestamp with adjusted value
            let adjusted_time_u32 = extend_u32(previous_time, adjusted_ts);

            // If this adjusted timestamp moves time forward, use it
            if adjusted_time_u32 > previous_time.unwrap() {
                time_u32 = adjusted_time_u32;
            } else {
                // Fallback
                time_u32 = header.timestamp as u64;
            }
        }

        let time = MediaTime::new(time_u32, clock_rate);

        if !is_repair {
            self.last_time = Some(time);
            if is_new_packet {
                self.remember_recent(seq_no, time.numer());
            }
        }

        RegisterUpdateReceipt {
            time,
            is_new_packet,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn handle_rtp(
        &mut self,
        now: Instant,
        header: RtpHeader,
        payload: Arc<[u8]>,
        seq_no: SeqNo,
        time: MediaTime,
        wire_payload_len: usize,
    ) -> RtpPacket {
        let packet = self.make_rtp_packet(now, header, payload, seq_no, time);

        // `payload` may be narrower than what was on the wire (RED unwrapped to its primary
        // block); count what was received, matching the sender's egress accounting.
        self.stats.bytes += wire_payload_len as u64;
        self.stats.packets += 1;

        packet
    }

    /// Build an [`RtpPacket`] WITHOUT counting it in receive stats. Used for RFC 2198
    /// FEC-recovered packets, which were never received on the wire (the carrying packet's
    /// bytes are already counted by `handle_rtp`).
    pub(crate) fn make_rtp_packet(
        &mut self,
        now: Instant,
        header: RtpHeader,
        payload: Arc<[u8]>,
        seq_no: SeqNo,
        time: MediaTime,
    ) -> RtpPacket {
        trace!("Handle RTP: {:?}", header);

        let need_clock_rate = self.last_clock_rate.map(|(pt, _)| pt) != Some(header.payload_type);
        if need_clock_rate {
            self.last_clock_rate = Some((header.payload_type, time.frequency()));

            // If we get an SR before the first packet, we update the potential clock rate.
            if let Some(i) = &mut self.sender_info {
                i.info.rtp_time = MediaTime::new(i.info.rtp_time.numer(), time.frequency());
            }
        }

        RtpPacket {
            seq_no,
            time,
            header,
            payload,
            vp8_patch: None,
            nackable: false,
            last_sender_info: self.sender_info.as_ref().map(|l| l.info),
            timestamp: now,
        }
    }

    /// Rewrites the header to be the un-RTX'd original packet, and returns
    /// `data` with the original sequence number prefix stripped.
    pub(crate) fn un_rtx<'a>(&self, header: &mut RtpHeader, data: &'a [u8], pt: Pt) -> &'a [u8] {
        let mut orig_seq_no_16 = 0;

        let n = RtpHeader::read_original_sequence_number(data, &mut orig_seq_no_16);

        trace!(
            "Repaired seq no {} -> {}",
            header.sequence_number, orig_seq_no_16
        );

        header.sequence_number = orig_seq_no_16;

        header.ssrc = self.ssrc;
        header.payload_type = pt;
        header.ext_vals.rid = header.ext_vals.rid_repair.take();

        &data[n..]
    }

    pub(crate) fn maybe_create_keyframe_request(
        &mut self,
        sender_ssrc: Ssrc,
        feedback: &mut VecDeque<Rtcp>,
    ) {
        let Some(kind) = self.pending_request_keyframe.take() else {
            return;
        };

        let ssrc = self.ssrc;

        match kind {
            KeyframeRequestKind::Pli => {
                self.stats.plis += 1;
                feedback.push_back(Rtcp::Pli(Pli { sender_ssrc, ssrc }))
            }
            KeyframeRequestKind::Fir => {
                self.stats.firs += 1;
                feedback.push_back(Rtcp::Fir(Fir {
                    sender_ssrc,
                    reports: FirEntry {
                        ssrc,
                        seq_no: self.next_fir_seq_no(),
                    }
                    .into(),
                }))
            }
        }
    }

    pub(crate) fn maybe_create_remb_request(
        &mut self,
        sender_ssrc: Ssrc,
        feedback: &mut VecDeque<Rtcp>,
    ) {
        let Some(bitrate) = self.pending_request_remb.take() else {
            return;
        };

        feedback.push_back(Rtcp::Remb(Remb {
            sender_ssrc,
            ssrc: 0.into(),
            bitrate: bitrate.as_f64() as f32,
            ssrcs: vec![*self.ssrc],
        }))
    }

    fn next_fir_seq_no(&mut self) -> u8 {
        let x = self.fir_seq_no;
        self.fir_seq_no = self.fir_seq_no.wrapping_add(1);
        x
    }

    pub(crate) fn need_rr(&self, now: Instant, intervals: RtcpReportIntervals) -> bool {
        if self.ssrc.is_probe() {
            return false;
        }

        now >= self.receiver_report_at(intervals)
    }

    pub(crate) fn create_rr_and_update(
        &mut self,
        now: Instant,
        sender_ssrc: Ssrc,
        feedback: &mut VecDeque<Rtcp>,
    ) {
        let mut rr = self.create_receiver_report(now);
        rr.sender_ssrc = sender_ssrc;

        if !rr.reports.is_empty() {
            let report = &rr.reports[rr.reports.len() - 1];
            self.stats.update_loss(report.fraction_lost);
            self.stats.jitter = report.jitter;
        }

        let xr = self.create_extended_receiver_report(now, sender_ssrc);

        trace!(
            "Created feedback RR/XR ({:?}): {:?} {:?}",
            self.midrid, rr, xr
        );
        feedback.push_back(Rtcp::ReceiverReport(rr));
        feedback.push_back(Rtcp::ExtendedReport(xr));

        self.last_receiver_report = now;
    }

    fn create_receiver_report(&mut self, now: Instant) -> ReceiverReport {
        let Some(mut report) = self.register.as_mut().and_then(|r| r.reception_report()) else {
            return ReceiverReport {
                sender_ssrc: 0.into(), // set one level up
                reports: ReportList::new(),
            };
        };
        report.ssrc = self.ssrc;

        // The middle 32 bits out of 64 in the NTP timestamp (as explained in
        // Section 4) received as part of the most recent RTCP sender report
        // (SR) packet from source SSRC_n.  If no SR has been received yet,
        // the field is set to zero.
        report.last_sr_time = {
            let t64 = self
                .sender_info
                .as_ref()
                .map_or(0u64, |l| l.info.ntp_time.as_ntp_64());

            (t64 >> 16) as u32
        };

        // The delay, expressed in units of 1/65_536 seconds, between
        // receiving the last SR packet from source SSRC_n and sending this
        // reception report block.  If no SR packet has been received yet
        // from SSRC_n, the DLSR field is set to zero.
        report.last_sr_delay = if let Some(l) = self.sender_info.as_ref() {
            let t = l.received_at;
            let delay = now - t;
            ((delay.as_micros() * 65_536) / 1_000_000) as u32
        } else {
            0
        };

        ReceiverReport {
            sender_ssrc: 0.into(), // set one level up
            reports: report.into(),
        }
    }

    fn create_extended_receiver_report(&self, now: Instant, sender_ssrc: Ssrc) -> ExtendedReport {
        // we only want to report our time to measure RTT,
        // the source will answer with Dlrr feedback, allowing us to calculate RTT
        let block = ReportBlock::Rrtr(Rrtr {
            ntp_time: now.to_system_time(),
        });
        ExtendedReport {
            ssrc: sender_ssrc,
            blocks: vec![block],
        }
    }

    pub(crate) fn recovery_deadline(
        &self,
        range: std::ops::Range<SeqNo>,
        received: Instant,
    ) -> Option<Instant> {
        if !self.suppress_nack {
            if let Some(register) = &self.register {
                return register.recovery_deadline(range, received, self.stats.rtt);
            }
        }
        let rtt = self.stats.rtt.unwrap_or(super::register_nack::DEFAULT_RTT);
        received.checked_add(rtt.saturating_mul(3))
    }

    /// Whether this stream should keep its NACK timer active.
    ///
    /// Paused streams finish pending retries before disabling NACK reports.
    pub(crate) fn nack_enabled(&self) -> bool {
        // Deliberately don't look at RTX is_some() here, since when using dynamic SSRC, we might need
        // to send NACK before discovering the remote RTX.
        if self.suppress_nack {
            return false;
        }

        if !self.paused {
            return true;
        }

        let Some(register) = &self.register else {
            return false;
        };

        register.has_pending_nacks()
    }

    pub(crate) fn maybe_create_nack(
        &mut self,
        now: Instant,
        sender_ssrc: Ssrc,
        feedback: &mut VecDeque<Rtcp>,
    ) -> Option<()> {
        if !self.nack_enabled() {
            return None;
        }

        let nacks = self
            .register
            .as_mut()
            .and_then(|r| r.nack_report(now, self.stats.rtt))?;

        for mut nack in nacks {
            nack.sender_ssrc = sender_ssrc;
            nack.ssrc = self.ssrc;

            trace!("Created feedback NACK: {:?}", nack);
            feedback.push_back(Rtcp::Nack(nack));
            self.stats.nacks += 1;
        }

        Some(())
    }

    pub(crate) fn visit_stats(&self, snapshot: &mut StatsSnapshot, now: Instant) {
        if self.ssrc.is_probe() {
            return;
        }

        self.stats
            .fill(snapshot, self.midrid, self.sender_info.as_ref(), now);
    }

    pub(crate) fn poll_paused(&mut self) -> Option<StreamPaused> {
        if self.ssrc.is_probe() {
            return None;
        }

        if !self.need_paused_event {
            return None;
        }

        self.need_paused_event = false;

        debug!(
            "{} StreamRx with {:?} and SSRC: {}",
            if self.paused { "Paused" } else { "Unpaused" },
            self.midrid,
            self.ssrc
        );

        Some(StreamPaused {
            ssrc: self.ssrc,
            mid: self.midrid.mid(),
            rid: self.midrid.rid(),
            paused: self.paused,
        })
    }

    /// Poll the most recent sender info and when it was received
    pub(crate) fn poll_sender_info(&mut self) -> Option<(SenderInfo, Instant)> {
        let i = self.sender_info.as_mut()?;
        if i.emitted {
            return None;
        }

        i.emitted = true;

        Some((i.info, i.received_at))
    }

    pub(crate) fn reset_buffers(&mut self, max_seq_lookup: impl Fn(Ssrc) -> Option<SeqNo>) {
        if let Some(r) = &mut self.register {
            r.clear(max_seq_lookup(self.ssrc));
        }

        if self.register_rtx.is_some() {
            self.register_rtx = self
                .rtx
                .and_then(max_seq_lookup)
                .map(SeqNoWindow::new_after);
        }
        self.pending_request_keyframe = None;
    }

    #[must_use]
    pub(crate) fn change_ssrc(&mut self, ssrc: Ssrc) -> bool {
        // Avoid flapping
        if ssrc == self.ssrc || Some(ssrc) == self.previous_ssrc {
            return false;
        }

        debug!(
            "Change main SSRC: {} -> {} {:?}",
            self.ssrc, ssrc, self.midrid
        );

        // Remember which was the previous in case a stray packet turns up
        // so do we don't go "backwards".
        self.previous_ssrc = Some(self.ssrc);
        self.ssrc = ssrc;

        // Reset all SSRC-specific state
        self.register = None;
        self.red_recent.clear();
        self.last_time = None;
        self.last_clock_rate = None;
        self.sender_info = None;
        self.last_receiver_report = already_happened();
        self.fir_seq_no = 0;
        self.pending_request_keyframe = None;
        self.pending_request_remb = None;
        self.reset_roc = None;

        // Note: We don't reset the RTX register here, as the RTX SSRC is managed separately
        // via maybe_reset_rtx() and is not directly tied to the main SSRC change.

        true
    }

    pub(crate) fn maybe_reset_rtx(&mut self, rtx: Ssrc) {
        if let Some(current) = self.rtx {
            if current == rtx {
                return;
            }

            debug!(
                "Change RTX SSRC {} -> {} for main SSRC: {} {:?}",
                current, rtx, self.ssrc, self.midrid
            );
        } else {
            debug!("SSRC {} associated with RTX: {}", self.ssrc, rtx);
        }

        self.rtx = Some(rtx);
        self.register_rtx = None;
    }

    /// Reset the current rollover counter (ROC).
    ///
    /// This is used in scenarios where we use a single sequence number across all
    /// receivers of the same stream (as opposed to a sequence number unique per peer).
    ///
    /// The ROC is not carried in the RTP header. Obtain it through out-of-band
    /// signaling and call this before handling the first SRTP packet for the stream.
    /// See [`RtpWrite::new`][crate::rtp::RtpWrite::new] for sender and receiver examples.
    ///
    /// [RFC3711](https://datatracker.ietf.org/doc/html/rfc3711#section-3.3.1):
    ///
    /// > Receivers joining an on-going session MUST be given the
    /// > current ROC value using out-of-band signaling such as key-management
    /// > signaling.  Furthermore, the receiver SHALL initialize s_l to the RTP
    /// > sequence number (SEQ) of the first observed SRTP packet (unless the
    /// > initial value is provided by out of band signaling such as key
    /// > management).
    pub fn reset_roc(&mut self, roc: u64) {
        self.register = None;
        self.register_rtx = None;
        self.red_recent.clear();
        self.reset_roc = Some(roc);
    }

    pub(crate) fn is_midrid(&self, midrid: MidRid) -> bool {
        midrid.special_equals(&self.midrid)
    }
}

impl StreamRxStats {
    fn update_loss(&mut self, fraction_lost: u8) {
        self.loss = Some(fraction_lost as f32 / u8::MAX as f32)
    }

    pub(crate) fn fill(
        &self,
        snapshot: &mut StatsSnapshot,
        midrid: MidRid,
        sender_info: Option<&LastSenderInfo>,
        now: Instant,
    ) {
        if self.bytes == 0 {
            return;
        }

        let stats = MediaIngressStats {
            mid: midrid.mid(),
            rid: midrid.rid(),
            bytes: self.bytes,
            packets: self.packets,
            firs: self.firs,
            plis: self.plis,
            nacks: self.nacks,
            jitter: self.jitter,
            rtt: self.rtt,
            loss: self.loss,
            timestamp: now,
            remote: sender_info.map(|l| RemoteEgressStats {
                bytes: l.info.sender_octet_count as u64,
                packets: l.info.sender_packet_count as u64,
            }),
        };

        // Several SSRCs can back a given (mid, rid) tuple. For example, Firefox creates new SSRCs
        // when a Transceiver transitions from send -> inactive -> send. In order to continue
        // correctly reporting stats for this (mid, rid) pair we need to merge the stats across all
        // the SSRCs that have been used.
        snapshot
            .ingress
            .entry(midrid)
            .and_modify(|s| s.merge_by_mid_rid(&stats))
            .or_insert(stats);
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RegisterUpdateReceipt {
    pub time: MediaTime,
    pub is_new_packet: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paused_timestamp_repair_moves_time_forward() {
        let now = already_happened();
        let mut stream = StreamRx::new(
            7.into(),
            MidRid("mid".into(), None),
            false,
            Duration::from_millis(1500),
        );
        let previous_time = 1;
        stream.last_time = Some(MediaTime::new(previous_time, Frequency::NINETY_KHZ));
        stream.paused = true;

        let header = RtpHeader {
            payload_type: Pt::new_with_value(96),
            sequence_number: 42,
            timestamp: 0,
            ssrc: 7.into(),
            ..Default::default()
        };

        let seq_no = stream.extend_seq(&header, false, |_| None);
        let receipt = stream.update_register(now, &header, Frequency::NINETY_KHZ, false, seq_no);

        assert!(
            receipt.time.numer() > previous_time,
            "expected repaired media time to move forward"
        );
        assert!(!stream.paused);
    }

    /// Feed packets `(seq, rtp_time)` into a fresh stream, as received on the wire.
    fn stream_with(packets: &[(u16, u32)]) -> StreamRx {
        let now = already_happened();
        let mut stream = StreamRx::new(
            7.into(),
            MidRid("mid".into(), None),
            false,
            Duration::from_millis(1500),
        );
        for (seq, ts) in packets {
            let header = RtpHeader {
                payload_type: Pt::new_with_value(111),
                sequence_number: *seq,
                timestamp: *ts,
                ssrc: 7.into(),
                ..Default::default()
            };
            let seq_no = stream.extend_seq(&header, false, |_| None);
            stream.update_register(now, &header, Frequency::FORTY_EIGHT_KHZ, false, seq_no);
        }
        stream
    }

    #[test]
    fn dlrr_for_local_ssrc_updates_receive_streams() {
        let mut streams = crate::streams::Streams::new(false, 1200, Duration::from_millis(1500));
        for (ssrc, mid) in [(7, "video"), (8, "audio")] {
            streams.expect_stream_rx(ssrc.into(), None, MidRid(mid.into(), None), false);
        }
        let default_ssrc = streams.first_ssrc_local();
        streams.declare_stream_tx(9.into(), None, MidRid("send".into(), None));
        let now = Instant::now();
        let last_rr_time = ((now - Duration::from_millis(250))
            .to_system_time()
            .as_ntp_64()
            >> 16) as u32;

        // Ignore a sub-block addressed to the remote media source instead of us.
        streams.handle_dlrr(
            now,
            DlrrItem {
                ssrc: 7.into(),
                last_rr_time,
                last_rr_delay: 3276,
            },
        );
        assert_eq!(streams.stream_rx(&7.into()).unwrap().stats.rtt, None);

        // Accept both an active sender and the fallback SSRC used before it existed.
        for local_ssrc in [9.into(), default_ssrc] {
            for ssrc in [7, 8] {
                streams.stream_rx(&ssrc.into()).unwrap().stats.rtt = None;
            }
            streams.handle_dlrr(
                now,
                DlrrItem {
                    ssrc: local_ssrc,
                    last_rr_time,
                    last_rr_delay: 3276,
                },
            );
            for ssrc in [7, 8] {
                let rtt = streams.stream_rx(&ssrc.into()).unwrap().stats.rtt.unwrap();
                assert!((Duration::from_millis(199)..=Duration::from_millis(201)).contains(&rtt));
            }
        }
    }

    #[test]
    fn nack_retries_use_receive_stream_rtt() {
        for rtt in [Duration::from_millis(20), Duration::from_millis(250)] {
            let mut stream = stream_with(&[(10, 0), (12, 1920)]);
            stream.stats.rtt = Some(rtt);
            let now = Instant::now();
            let mut feedback = VecDeque::new();
            assert!(
                stream
                    .maybe_create_nack(now, 1.into(), &mut feedback)
                    .is_some()
            );
            let due = now + rtt + Duration::from_millis(5);
            assert!(
                stream
                    .maybe_create_nack(due - Duration::from_nanos(1), 1.into(), &mut feedback)
                    .is_none()
            );
            assert_eq!(feedback.len(), 1);
            assert!(
                stream
                    .maybe_create_nack(due, 1.into(), &mut feedback)
                    .is_some()
            );
            assert_eq!(feedback.len(), 2);
        }
    }

    #[test]
    fn paused_stream_finishes_pending_nack_retries() {
        let mut stream = stream_with(&[(10, 0), (12, 1920)]);
        let mut now = stream.paused_at().unwrap();
        stream.handle_timeout(now);
        assert!(stream.paused);

        let mut feedback = VecDeque::new();
        for _ in 0..5 {
            assert!(stream.nack_enabled());
            assert!(
                stream
                    .maybe_create_nack(now, 1.into(), &mut feedback)
                    .is_some()
            );
            assert!(
                stream
                    .maybe_create_nack(now, 1.into(), &mut feedback)
                    .is_none()
            );
            now += Duration::from_millis(105);
        }
        assert_eq!(feedback.len(), 5);
        assert!(feedback.iter().all(|rtcp| {
            matches!(rtcp, Rtcp::Nack(nack) if nack.reports.len() == 1 && nack.reports[0].pid == 11)
        }));

        assert!(!stream.nack_enabled());
        assert!(
            stream
                .maybe_create_nack(now, 1.into(), &mut feedback)
                .is_none()
        );
        assert_eq!(feedback.len(), 5);
    }

    #[test]
    fn red_locate_seq_single_hole_regardless_of_frame_duration() {
        // 20 ms frames, then a 40 ms frame at seq 12, then 20 ms again. Seq 13 lost.
        let stream = stream_with(&[(10, 0), (11, 960), (12, 1920), (14, 4800)]);

        // Block at the time of seq 13 (1920 + 1920): the only hole between 12 and 14.
        assert_eq!(stream.red_locate_seq(14.into(), 3840), Some(13.into()));
    }

    #[test]
    fn red_locate_seq_several_holes_need_uniform_spacing() {
        // Seq 11, 12, 13 lost; 10 and 14 bracket them 4 frames apart: unambiguous.
        let stream = stream_with(&[(10, 0), (14, 3840)]);
        assert_eq!(stream.red_locate_seq(14.into(), 960), Some(11.into()));
        assert_eq!(stream.red_locate_seq(14.into(), 1920), Some(12.into()));
        assert_eq!(stream.red_locate_seq(14.into(), 2880), Some(13.into()));
        // Not on a slot: skip rather than guess.
        assert_eq!(stream.red_locate_seq(14.into(), 1000), None);

        // Same holes, but one of the lost frames was longer (span not a multiple): every block
        // is skipped, none is misplaced.
        let stream = stream_with(&[(10, 0), (14, 4800)]);
        for t in [960, 1920, 2880, 3840] {
            assert_eq!(stream.red_locate_seq(14.into(), t), None, "time {t}");
        }
    }

    #[test]
    fn red_locate_seq_rejects_known_and_recovered_frames() {
        let mut stream = stream_with(&[(10, 0), (14, 3840)]);
        let now = Instant::now();
        let mut feedback = VecDeque::new();
        assert!(
            stream
                .maybe_create_nack(now, 1.into(), &mut feedback)
                .is_some()
        );

        // A block for a frame we already have is not a recovery.
        assert_eq!(stream.red_locate_seq(14.into(), 0), None);

        // Once recovered, the seq is neither a hole nor NACKed, and a later block for the same
        // frame is rejected instead of being pushed into a neighbouring hole.
        stream.mark_red_recovered(12.into(), 1920);
        assert_eq!(stream.red_locate_seq(14.into(), 1920), None);
        assert!(!stream.is_new_packet(false, 12.into()));
        let nacked: Vec<SeqNo> = stream
            .register
            .as_mut()
            .unwrap()
            .nack_report(now + Duration::from_millis(105), None)
            .into_iter()
            .flatten()
            .flat_map(|n| {
                n.reports
                    .iter()
                    .flat_map(|r| r.into_iter(10.into()))
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(nacked, vec![11.into(), 13.into()]);

        // The remaining holes are still found (11 and 13 are single-hole brackets now).
        assert_eq!(stream.red_locate_seq(14.into(), 960), Some(11.into()));
        assert_eq!(stream.red_locate_seq(14.into(), 2880), Some(13.into()));
    }

    #[test]
    fn red_locate_seq_limits_recovery_depth() {
        let mut packets = vec![(10u16, 0u32)];
        // Seq 11 lost, then 12..=30 received at 20 ms.
        packets.extend((12..=30u16).map(|s| (s, (s as u32 - 10) * 960)));
        let stream = stream_with(&packets);
        assert_eq!(stream.red_locate_seq(30.into(), 960), None);
        assert_eq!(stream.red_locate_seq(19.into(), 960), Some(11.into()));
    }

    #[test]
    fn receiver_report_uses_supplied_intervals() {
        let mut stream = StreamRx::new(
            7.into(),
            MidRid("mid".into(), None),
            false,
            Duration::from_millis(1500),
        );
        let now = Instant::now();
        stream.last_receiver_report = now;

        assert_eq!(
            stream.receiver_report_at(RtcpReportIntervals {
                audio: Duration::from_millis(750),
                video: Duration::from_millis(500),
            }),
            now + Duration::from_millis(750)
        );
    }

    #[test]
    fn rtx_tracks_rollover_and_reordering_only_after_receipt() {
        let mut stream = rtx_stream();
        let header = rtx_header(8, 50000);
        stream.extend_seq(&header, true, |_| None);
        assert!(
            stream.register_rtx.is_none(),
            "unverified headers must not advance the window"
        );

        for seq in [65535, 65536, 65538, 65537] {
            record_rtx(&mut stream, seq);
            assert!(!stream.is_new_packet(true, seq.into()));
        }
        assert!(
            stream.register.is_none(),
            "RTX must not update primary reception reports"
        );
        let mut feedback = VecDeque::new();
        assert!(
            stream
                .maybe_create_nack(Instant::now(), 1.into(), &mut feedback)
                .is_none()
        );
        assert!(feedback.is_empty());
    }

    #[test]
    fn rtx_uses_the_full_128_position_window() {
        let mut stream = rtx_stream();
        record_rtx(&mut stream, 0);
        record_rtx(&mut stream, 200);
        assert!(!stream.is_new_packet(true, 72.into()));
        assert!(stream.is_new_packet(true, 73.into()));
        record_rtx(&mut stream, 73);
        assert!(!stream.is_new_packet(true, 73.into()));
    }

    #[test]
    fn rtx_buffer_and_roc_resets_preserve_remembered_replay_floor() {
        let mut stream = rtx_stream();
        record_rtx(&mut stream, 65535);
        record_rtx(&mut stream, 65537);
        assert!(stream.is_new_packet(true, 65536.into()));
        let history = |ssrc| (ssrc == 8.into()).then_some(65537.into());
        stream.reset_buffers(history);
        assert!(!stream.is_new_packet(true, 65536.into()));
        assert!(!stream.is_new_packet(true, 65537.into()));

        stream.reset_roc(5);
        let header = rtx_header(8, 2);
        assert_eq!(stream.extend_seq(&header, true, history), 65538.into());
        assert!(!stream.is_new_packet(true, 65536.into()));
        assert!(stream.is_new_packet(true, 65538.into()));
        // The requested ROC applies to the main stream, not the RTX sequence series.
        let main = RtpHeader {
            ssrc: 7.into(),
            sequence_number: 10,
            ..Default::default()
        };
        assert_eq!(
            stream.extend_seq(&main, false, history),
            ((5u64 << 16) + 10).into()
        );
    }

    #[test]
    fn rtx_ssrc_changes_restore_history_and_main_changes_preserve_it() {
        let mut stream = rtx_stream();
        record_rtx(&mut stream, 65535);
        assert!(stream.change_ssrc(9.into()));
        assert!(!stream.is_new_packet(true, 65535.into()));
        stream.maybe_reset_rtx(8.into());
        assert!(!stream.is_new_packet(true, 65535.into()));

        stream.maybe_reset_rtx(10.into());
        assert!(stream.register_rtx.is_none());
        record_rtx(&mut stream, 0);
        stream.maybe_reset_rtx(8.into());
        let header = rtx_header(8, 0);
        let seq = stream.extend_seq(&header, true, |ssrc| {
            (ssrc == 8.into()).then_some(65535.into())
        });
        assert_eq!(seq, 65536.into());
        assert!(!stream.is_new_packet(true, 65534.into()));
        assert!(!stream.is_new_packet(true, 65535.into()));
        assert!(stream.is_new_packet(true, seq));
        record_rtx(&mut stream, 65536);
    }

    fn rtx_stream() -> StreamRx {
        let mut stream = StreamRx::new(
            7.into(),
            MidRid("mid".into(), None),
            false,
            Duration::from_millis(1500),
        );
        stream.maybe_reset_rtx(8.into());
        stream
    }

    fn record_rtx(stream: &mut StreamRx, seq: u64) {
        let header = rtx_header(*stream.rtx().unwrap(), seq as u16);
        let extended = stream.extend_seq(&header, true, |_| None);
        assert_eq!(extended, seq.into());
        assert!(stream.is_new_packet(true, extended));
        let receipt = stream.update_register(
            already_happened(),
            &header,
            Frequency::NINETY_KHZ,
            true,
            extended,
        );
        assert!(receipt.is_new_packet);
    }

    fn rtx_header(ssrc: u32, seq: u16) -> RtpHeader {
        RtpHeader {
            ssrc: ssrc.into(),
            sequence_number: seq,
            payload_type: 97.into(),
            ..Default::default()
        }
    }
}
