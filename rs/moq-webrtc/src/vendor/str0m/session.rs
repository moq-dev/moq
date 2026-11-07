use std::collections::{HashMap, VecDeque};
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::Event;
use crate::bwe::BweKind;
use crate::bwe_::Bwe;
use crate::config::KeyingMaterial;
use crate::config_mod::RtcpReportIntervals;
use crate::crypto::CryptoProvider;
use crate::crypto::dtls::SrtpProfile;
use crate::format::CodecConfig;
use crate::format::PayloadParams;
use crate::format::Vp9PacketizerMode;
use crate::io::DatagramSend;
use crate::media::AppSpecificFeedback;
use crate::media::Media;
use crate::media::{KeyframeRequestKind, MID_PROBE};
use crate::media::{MediaAdded, MediaChanged};
use crate::pacer::PacerControl;
use crate::pacer::{Pacer, PacerImpl};
use crate::packet::{RedBlock, RedDecoder, red_same_pt_blocks};
use crate::poll::Wake;
use crate::rtp::{Extension, RawPacket};
use crate::rtp_::Direction;
use crate::rtp_::ExtensionValues;
use crate::rtp_::MediaTime;
use crate::rtp_::MidRid;
use crate::rtp_::Pli;
use crate::rtp_::Pt;
use crate::rtp_::SRTCP_OVERHEAD;
use crate::rtp_::SeqNo;
use crate::rtp_::SeqNoWindow;
use crate::rtp_::{Bitrate, ExtensionMap, Goodbye, Mid, ReportList, Rtcp, RtcpFb};
use crate::rtp_::{Dlrr, DlrrItem, ExtendedReport, ReportBlock};
use crate::rtp_::{RtpHeader, SessionId, TwccPacketId, extend_u16};
use crate::rtp_::{SrtpContext, Ssrc};
use crate::rtp_::{TwccRecvRegister, TwccSendRegister};
use crate::stats::StatsSnapshot;
use crate::streams::{RtpPacket, StreamTimeoutConfig, Streams};
use crate::util::{Soonest, SystemTimeExt, already_happened, not_happening};
use crate::{Reason, net};
use crate::{RtcConfig, RtcError};

/// Minimum interval between NACK polls. Per-packet retries also wait for RTT.
/// Keep this high enough to avoid additional problems in very bad network conditions.
const NACK_MIN_INTERVAL: Duration = Duration::from_millis(33);

/// Delay between reports of TWCC. This is deliberately very low.
const TWCC_INTERVAL: Duration = Duration::from_millis(50);

/// Maximum number of pending RRTR entries before oldest are dropped.
const MAX_PENDING_RRTRS: usize = 300;

/// Maximum number of DLRR items to include in a single ExtendedReport.
const MAX_DLRR_PER_REPORT: usize = 50;

pub(crate) struct Session {
    id: SessionId,

    // These fields are pub to allow session_sdp.rs modify them.
    // Notice the fields are maybe not in m-line index order since the app
    // might be spliced in somewhere.
    pub medias: Vec<Media>,

    // The actual RTP encoded streams.
    pub streams: Streams,

    /// The app m-line. Spliced into medias above.
    app: Option<(Mid, usize)>,

    reordering_size_audio: usize,
    reordering_size_video: usize,
    reordering_timeout_audio: Option<Duration>,
    intervals: RtcpReportIntervals,
    pub send_buffer_audio: usize,
    pub send_buffer_video: usize,

    /// Extension mappings are _per BUNDLE_, but we can only have one a=group BUNDLE
    /// in WebRTC (one ice connection), so they are effectively per session.
    pub exts: ExtensionMap,

    // Configuration of how we are sending/receiving media.
    pub codec_config: CodecConfig,

    // Payload params for SSRC 0 BWE probes.
    // Lazy init when we see the PT.
    probe_payload_params: Option<PayloadParams>,

    srtp_rx: Option<SrtpContext>,
    srtp_tx: Option<SrtpContext>,
    last_nack: Instant,
    last_twcc: Instant,
    twcc: u64,
    twcc_rx_register: TwccRecvRegister,
    twcc_tx_register: TwccSendRegister,
    max_rx_seq_lookup: HashMap<Ssrc, SeqNoWindow>,

    bwe: Option<Bwe>,
    bwe_last_event: Option<(Bitrate, bool)>,

    enable_twcc_feedback: bool,

    /// A pacer for sending RTP at specific rate.
    pacer: PacerImpl,
    pacer_control: PacerControl,

    // temporary buffer when getting the next (unencrypted) RTP packet from Media line.
    poll_packet_buf: Vec<u8>,

    // Next packet for RtpPacket event.
    pending_packet: Option<RtpPacket>,

    // Total RTP payload bytes for media, including retransmissions.
    media_bytes_rx: u64,
    media_bytes_tx: u64,

    pub ice_lite: bool,

    /// Whether we are running in RTP-mode.
    pub rtp_mode: bool,

    /// VP9 packetizer mode.
    vp9_packetizer_mode: Vp9PacketizerMode,

    feedback_tx: VecDeque<Rtcp>,
    feedback_rx: VecDeque<Rtcp>,
    rtp_closing: bool,

    /// Pending RRTRs from remote SSRCs in FIFO order, deduplicated by SSRC.
    /// A new RRTR for an already-pending SSRC updates its value in place.
    /// Capped at 300 entries; at most 50 are consumed per DLRR report.
    pending_rrtrs: VecDeque<(Ssrc, LastRrtr)>,

    raw_packets: Option<VecDeque<Box<RawPacket>>>,

    // Pending application-specific feedback (PSFB FMT=15) messages to emit as events.
    pending_app_feedback: Option<AppSpecificFeedback>,

    /// Target MTU (start) and warn threshold (end). Buffer sizing uses the
    /// target; oversized outgoing datagrams above the warn threshold log a warning.
    mtu: RangeInclusive<usize>,

    #[cfg(feature = "_internal_test_exports")]
    pending_probe: Option<crate::bwe_::ProbeClusterConfig>,
}

/// Last RRTR (Receiver Reference Time Report, RFC 3611 §4.4) received from a remote SSRC.
/// Stored at session level so we can respond with a DLRR regardless of which local stream
/// the remote is observing.
#[derive(Debug, Clone, Copy)]
struct LastRrtr {
    /// Middle 32 bits of the RRTR NTP timestamp (compact NTP / LRR).
    lrr: u32,
    /// When the RRTR arrived locally.
    received_at: Instant,
}

impl Session {
    pub fn new(config: &RtcConfig) -> Self {
        let mut id = SessionId::new();
        // Max 2^62 - 1: https://bugzilla.mozilla.org/show_bug.cgi?id=861895
        const MAX_ID: u64 = 2_u64.pow(62) - 1;
        while *id > MAX_ID {
            id = (*id >> 1).into();
        }
        let (pacer, bwe) = if let Some(config) = &config.bwe_config {
            let rate = config.initial_bitrate;
            let pacer = PacerImpl::leaky_bucket(rate * 2.0);
            let bwe = Bwe::new(rate);
            (pacer, Some(bwe))
        } else {
            (PacerImpl::null(), None)
        };

        let enable_stats = config.stats_interval.is_some();

        Session {
            id,
            medias: vec![],
            streams: Streams::new(enable_stats, *config.mtu.end(), config.pause_threshold),
            app: None,
            reordering_size_audio: config.reordering_size_audio,
            reordering_size_video: config.reordering_size_video,
            reordering_timeout_audio: config.reordering_timeout_audio,
            intervals: config.intervals,
            send_buffer_audio: config.send_buffer_audio,
            send_buffer_video: config.send_buffer_video,
            exts: config.exts.clone(),

            // Both sending and receiving starts from the configured codecs.
            // These can then be changed in the SDP OFFER/ANSWER dance.
            codec_config: config.codec_config.clone(),
            probe_payload_params: None,

            srtp_rx: None,
            srtp_tx: None,
            last_nack: already_happened(),
            last_twcc: already_happened(),
            twcc: 0,
            twcc_rx_register: TwccRecvRegister::new(100),
            twcc_tx_register: TwccSendRegister::new(1000),
            max_rx_seq_lookup: HashMap::new(),
            bwe,
            bwe_last_event: None,
            enable_twcc_feedback: false,
            pacer,
            pacer_control: PacerControl::new(),
            poll_packet_buf: vec![0; 2000],
            pending_packet: None,
            media_bytes_rx: 0,
            media_bytes_tx: 0,
            ice_lite: config.ice_lite,
            rtp_mode: config.rtp_mode,
            vp9_packetizer_mode: config.vp9_packetizer_mode,
            feedback_tx: VecDeque::new(),
            feedback_rx: VecDeque::new(),
            rtp_closing: false,
            pending_rrtrs: VecDeque::new(),
            raw_packets: if config.enable_raw_packets {
                Some(VecDeque::new())
            } else {
                None
            },
            pending_app_feedback: None,
            mtu: config.mtu.clone(),
            #[cfg(feature = "_internal_test_exports")]
            pending_probe: None,
        }
    }

    fn mtu(&self) -> usize {
        *self.mtu.start()
    }

    fn mtu_warn(&self) -> usize {
        *self.mtu.end()
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    pub fn set_app(&mut self, mid: Mid, index: usize) -> Result<(), String> {
        if let Some((mid_existing, index_existing)) = self.app {
            if mid_existing != mid {
                return Err(format!("App mid changed {} != {}", mid, mid_existing,));
            }
            if index_existing != index {
                return Err(format!("App index changed {} != {}", index, index_existing,));
            }
        } else {
            self.app = Some((mid, index));
        }
        Ok(())
    }

    pub fn app(&self) -> &Option<(Mid, usize)> {
        &self.app
    }

    pub fn set_keying_material(
        &mut self,
        mat: KeyingMaterial,
        crypto: &CryptoProvider,
        srtp_profile: SrtpProfile,
        active: bool,
    ) {
        // TODO: rename this to `initialise_srtp_context`?
        // Whether we're active or passive determines if we use the left or right
        // hand side of the key material to derive input/output.
        let left = active;

        self.srtp_rx = Some(SrtpContext::new(crypto, srtp_profile, &mat, !left));
        self.srtp_tx = Some(SrtpContext::new(crypto, srtp_profile, &mat, left));
    }

    pub fn handle_timeout(&mut self, now: Instant, wake: &mut Wake) -> Result<(), RtcError> {
        if self.rtp_closing {
            // Closing only drains previously queued BYEs; this timeout changes nothing.
            wake.no_events();
            wake.no_timeout();
            return Ok(());
        }

        // Payload ready media.
        self.do_payload(now)?;

        let sender_ssrc = self.streams.first_ssrc_local();

        let do_nack = now >= self.nack_at().unwrap_or(not_happening());

        self.streams.handle_timeout(
            now,
            sender_ssrc,
            do_nack,
            &self.medias,
            StreamTimeoutConfig {
                codecs: &self.codec_config,
                intervals: self.intervals,
            },
            &mut self.feedback_tx,
        );

        // Flush any pending RRTRs as a DLRR alongside the Sender Report compound packet.
        // We piggyback on the SR cadence so the DLRR doesn't generate extra standalone
        // RTCP traffic that could perturb bandwidth estimation.
        // Consume at most 50 entries per report; the rest remain for subsequent SRs.
        if !self.pending_rrtrs.is_empty()
            && self
                .feedback_tx
                .iter()
                .any(|r| matches!(r, Rtcp::SenderReport(_)))
        {
            let n = self.pending_rrtrs.len().min(MAX_DLRR_PER_REPORT);
            let items: Vec<DlrrItem> = self
                .pending_rrtrs
                .drain(..n)
                .map(|(ssrc, rrtr)| {
                    let delay = now.saturating_duration_since(rrtr.received_at);
                    let last_rr_delay = ((delay.as_micros() * 65_536) / 1_000_000) as u32;
                    DlrrItem {
                        ssrc,
                        last_rr_time: rrtr.lrr,
                        last_rr_delay,
                    }
                })
                .collect();

            self.feedback_tx
                .push_back(Rtcp::ExtendedReport(ExtendedReport {
                    ssrc: sender_ssrc,
                    blocks: vec![ReportBlock::Dlrr(Dlrr { items })],
                }));
        }

        if do_nack {
            self.last_nack = now;
        }

        let probe_media = self.handle_timeout_bwe(now);
        self.update_queue_state(now, probe_media);

        if let Some(twcc_at) = self.twcc_at() {
            if now >= twcc_at {
                self.create_twcc_feedback(sender_ssrc, now);
            }
        }

        Ok(())
    }

    fn handle_timeout_bwe(&mut self, now: Instant) -> Option<(Mid, Pt)> {
        self.bwe.as_ref()?;

        // We can request probes once SRTP keys are available and a non-stopped,
        // sending media section has both a transport sequence extension and a
        // payload type supporting TWCC feedback. No prior media packet or RTX
        // stream is required. The Direct API uses its configured capabilities.
        let probe_media = self.srtp_tx.as_ref().and_then(|_| self.probe_media());
        let do_probe = probe_media.is_some();
        let bwe = self.bwe.as_mut()?;

        if let Some(probe_config) = bwe.handle_timeout(now, do_probe) {
            // Only start the probe in the pacer if the estimator accepted it.
            if bwe.start_probe(probe_config, now) {
                #[cfg(feature = "_internal_test_exports")]
                {
                    self.pending_probe = Some(probe_config);
                }
                self.pacer.start_probe(probe_config);
            }
        }

        // Check if active probe just completed
        if let Some(cluster_id) = self.pacer.check_probe_complete(now) {
            bwe.end_probe(now, cluster_id);
            self.streams.set_probe_media(None);
        }
        probe_media
    }

    fn update_queue_state(&mut self, now: Instant, probe_media: Option<(Mid, Pt)>) {
        // Do not make unsendable media/padding ready before DTLS supplies keys.
        // Otherwise an immediate pacer deadline can starve the application's I/O.
        if self.srtp_tx.is_none() {
            return;
        }

        let probe_media = probe_media.filter(|_| self.pacer.active_cluster().is_some());
        self.streams.set_probe_media(probe_media);
        let medias = &self.medias;
        let codecs = &self.codec_config;
        let iter = self.streams.send_queue_states(now, |mid, pt| {
            // Reuse the already-validated fallback binding when it matches.
            probe_media == Some((mid, pt))
                || medias
                    .iter()
                    .find(|m| m.mid() == mid)
                    .is_some_and(|media| Self::probe_pt(media, codecs, Some(pt)).is_some())
        });

        let Some(padding_request) = self.pacer.handle_timeout(now, iter) else {
            return;
        };

        let stream = self
            .streams
            .stream_tx_by_midrid(padding_request.midrid)
            .expect("pacer to use an existing stream");

        stream.generate_padding(padding_request.padding);
    }

    // Use a negotiated PT and MID so browser transports can route the packet to
    // their receive-side congestion controller before discarding the empty payload.
    fn probe_media(&self) -> Option<(Mid, Pt)> {
        self.medias.iter().find_map(|media| {
            Self::probe_pt(media, &self.codec_config, None).map(|pt| (media.mid(), pt))
        })
    }

    // A regular padding source needs feedback for its actual payload type;
    // the SSRC 0 fallback can use any eligible payload type on this media.
    fn probe_pt(media: &Media, codecs: &CodecConfig, padding_pt: Option<Pt>) -> Option<Pt> {
        if media.stopped() || !media.direction().is_sending() {
            return None;
        }
        media
            .remote_extmap()
            .id_of(Extension::TransportSequenceNumber)?;
        codecs
            .params()
            .iter()
            .find(|p| {
                padding_pt.is_none_or(|pt| p.pt() == pt)
                    && p.fb_transport_cc()
                    && p.spec().codec.is_audio() == media.kind().is_audio()
                    && (media.remote_pts().is_empty() || media.remote_pts().contains(&p.pt()))
            })
            .map(|p| p.pt())
    }

    fn create_twcc_feedback(&mut self, sender_ssrc: Ssrc, now: Instant) -> Option<()> {
        self.last_twcc = now;
        let mut twcc = self.twcc_rx_register.build_report(self.mtu() - 100)?;

        // These SSRC are on media level, but twcc is on session level,
        // we fill in the first discovered media SSRC in each direction.
        twcc.sender_ssrc = sender_ssrc;
        twcc.ssrc = self.streams.first_ssrc_remote();

        trace!("Created feedback TWCC: {:?}", twcc);
        self.feedback_tx.push_front(Rtcp::Twcc(twcc));
        Some(())
    }

    /// Enqueue an application-specific feedback message (PSFB FMT=15, PT=206) for
    /// transmission as part of the regular RTCP compound packet.
    pub(crate) fn send_app_specific_feedback(
        &mut self,
        sender_ssrc: Ssrc,
        media_ssrc: Ssrc,
        payload: impl Into<Arc<[u8]>>,
    ) {
        use crate::rtp_::AppSpecificFeedback as RtcpAppFeedback;
        let feedback = RtcpAppFeedback {
            sender_ssrc,
            media_ssrc,
            payload: payload.into(),
        };
        self.feedback_tx
            .push_back(Rtcp::AppSpecificFeedback(feedback));
    }

    /// Enqueue a Picture Loss Indication (PLI, PT=206 FMT=1) for `media_ssrc`, to be
    /// sent as part of the regular RTCP compound packet. The feedback packet's sender
    /// SSRC is set to `sender_ssrc`.
    pub(crate) fn send_pli_feedback(&mut self, sender_ssrc: Ssrc, media_ssrc: Ssrc) {
        self.feedback_tx.push_back(Rtcp::Pli(Pli {
            sender_ssrc,
            ssrc: media_ssrc,
        }));
    }

    pub fn handle_rtp_receive(&mut self, now: Instant, message: &[u8], wake: &mut Wake) {
        let Some(header) = RtpHeader::parse(message, &self.exts) else {
            // Parsing has not touched stream state. Later rejection paths can have.
            wake.no_events();
            wake.no_timeout();
            trace!("Failed to parse RTP header");
            return;
        };

        self.handle_rtp(now, header, message);
    }

    pub fn handle_rtcp_receive(&mut self, now: Instant, message: &[u8], wake: &mut Wake) {
        // According to spec, the outer enclosing SRTCP packet should always be a SR or RR,
        // even if it's irrelevant and empty.
        // In practice I'm not sure that is happening, because libWebRTC hates empty packets.
        if self.handle_rtcp(now, message, wake).is_none() {
            // Missing keys or failed authentication: no reports reached the session.
            wake.no_events();
            wake.no_timeout();
        }
    }

    fn mid_and_ssrc_for_header(&mut self, now: Instant, header: &RtpHeader) -> Option<(Mid, Ssrc)> {
        let ssrc_header = header.ssrc;

        // SSRC 0 is used for non-media BWE probes from libwebrtc.
        // These probes need TWCC feedback but don't carry actual media.
        if ssrc_header.is_probe() {
            return self.ensure_probe_stream(header.payload_type);
        }

        if let Some(r) = self.streams.mid_ssrc_rx_by_ssrc_or_rtx(now, ssrc_header) {
            return Some(r);
        }

        // Attempt to dynamically map this header to some Media/ReceiveStream.
        self.map_dynamic(header);

        // The dynamic mapping might have added an entry by now.
        if let Some(r) = self.streams.mid_ssrc_rx_by_ssrc_or_rtx(now, ssrc_header) {
            return Some(r);
        }

        None
    }

    /// Creates the probe stream on-demand for handling SSRC 0 BWE probes.
    ///
    /// No Media is created since probes don't carry real media - they only need
    /// SRTP decryption and TWCC feedback.
    fn ensure_probe_stream(&mut self, pt: Pt) -> Option<(Mid, Ssrc)> {
        let ssrc: Ssrc = 0.into();
        let midrid = MidRid(MID_PROBE, None);

        // Add PayloadParams for this PT if not already configured.
        // Probes may use different PTs, so we add them as we see them.
        if self
            .probe_payload_params
            .as_ref()
            .is_none_or(|p| p.pt() != pt)
        {
            self.probe_payload_params = Some(PayloadParams::new_probe(pt));
        }

        // Create the stream with NACK suppressed (probes don't need retransmission).
        // Reusing it must not invalidate the cached NACK state on every packet.
        if !self.streams.has_stream_rx(ssrc) {
            self.streams.expect_stream_rx(ssrc, None, midrid, true);
        }

        Some((MID_PROBE, ssrc))
    }

    fn map_dynamic(&mut self, header: &RtpHeader) {
        // There are two strategies for dynamically mapping SSRC. Both use the RTP "mid"
        // header extension.
        // A) Mid+Rid - used when doing simulcast. Rid points out which
        //              simulcast layer is in use. There is a separate header
        //              to indicate repair (RTX) stream.
        // B) Mid+PT - when not doing simulcast, the PT identifies whether
        //             this is a repair stream.

        let Some(mid) = header.ext_vals.mid else {
            return;
        };
        let rid = header.ext_vals.rid.or(header.ext_vals.rid_repair);

        // The media the mid points out. Bail if the mid points to something
        // we don't know about.
        let Some(media) = self.medias.iter_mut().find(|m| m.mid() == mid) else {
            return;
        };

        // Figure out which payload the PT maps to. Either main or RTX.
        let maybe_payload = self.codec_config.iter().find(|p| {
            p.pt() == header.payload_type
                || p.resend() == Some(header.payload_type)
                || p.red() == Some(header.payload_type)
        });

        // If we don't find it, bail out.
        let Some(payload) = maybe_payload else {
            return;
        };

        if let Some(rid) = rid {
            // Case A - use the rid_repair header to identify RTX.
            let is_main = header.ext_vals.rid.is_some();

            let midrid = MidRid(mid, Some(rid));

            self.streams
                .map_dynamic_by_rid(header.ssrc, midrid, media, *payload, is_main);
        } else {
            // Case B - the payload type identifies RTX. RED rides the main SSRC, so a RED
            // PT is also "main".
            let is_main =
                payload.pt() == header.payload_type || payload.red() == Some(header.payload_type);

            let midrid = MidRid(mid, None);

            self.streams
                .map_dynamic_by_pt(header.ssrc, midrid, media, *payload, is_main);
        }
    }

    pub(crate) fn handle_rtp(&mut self, now: Instant, mut header: RtpHeader, buf: &[u8]) {
        // Rewrite absolute-send-time (if present) to be relative to now.
        header.ext_vals.update_absolute_send_time(now);

        trace!("Handle RTP: {:?}", header);

        // Early repair padding has no main stream yet, but still needs transport feedback.
        let mapped = self.mid_and_ssrc_for_header(now, &header);
        let can_receive = mapped.is_some() || self.is_early_rtx_padding(&header);
        if !can_receive {
            trace!("No mid/SSRC for header: {:?}", header);
            return;
        }

        let srtp = match self.srtp_rx.as_mut() {
            Some(v) => v,
            None => {
                trace!("Rejecting SRTP while missing SrtpContext");
                return;
            }
        };

        let mut stream = mapped.and_then(|(_, ssrc)| self.streams.stream_rx(&ssrc));
        let maybe_params = if header.ssrc.is_probe() {
            self.probe_payload_params.as_ref()
        } else {
            main_payload_params(&self.codec_config, header.payload_type)
        };

        let params = match maybe_params {
            Some(p) => p,
            None => {
                trace!(
                    "No payload params could be found (main or RTX) for {:?}",
                    header.payload_type
                );
                return;
            }
        };
        let clock_rate = params.spec().rtp_clock_rate();
        let pt = params.pt();
        let is_red = params.red() == Some(header.payload_type);
        // RTX padding may use the main payload type, so an associated RTX SSRC takes precedence.
        let repair_ssrc = stream.as_ref().and_then(|s| s.rtx());
        let is_repair = repair_ssrc == Some(header.ssrc) || (!is_red && pt != header.payload_type);
        let history = self.max_rx_seq_lookup.get(&header.ssrc);
        let mut seq_no = if let Some(stream) = &mut stream {
            let max_seq_lookup = make_max_seq_lookup(&self.max_rx_seq_lookup);
            stream.extend_seq(&header, is_repair, max_seq_lookup)
        } else {
            let max_seq = history.map(|s| s.max_seq());
            header.sequence_number(max_seq)
        };
        let is_repair_padding = is_repair && header.has_padding;
        let use_session_history = is_repair_padding || stream.is_none();
        // A newly discovered stream restores only the RTX maximum and blocks older packets.
        // Keep using the session bitmap for padding so unseen reordered packets still count,
        // while padding already received before discovery remains protected against replay.
        let is_new = if use_session_history {
            history.is_none_or(|history| history.accepts(seq_no))
        } else {
            let stream = stream.as_ref().unwrap();
            stream.is_new_packet(is_repair, seq_no)
        };
        if !is_new {
            trace!(
                "Ignoring dupe packet ssrc: {} seq_no: {}",
                header.ssrc, seq_no
            );
            return;
        }

        // The decrypted plaintext borrows from the SRTP scratch buffer. We
        // narrow it down to the actual payload via slicing only, then build
        // the final `Arc<[u8]>` in a single allocation at the end.
        let data = match srtp.unprotect_rtp(buf, &header, *seq_no) {
            Some(d) => d,
            None => {
                trace!(
                    "Failed to unprotect SRTP for SSRC: {} pt: {}  mid: {:?} \
                    rid: {:?} seq_no: {} is_repair: {}",
                    header.ssrc,
                    pt,
                    header.ext_vals.mid,
                    header.ext_vals.rid.or(header.ext_vals.rid_repair),
                    seq_no,
                    is_repair
                );
                return;
            }
        };

        let data = if header.has_padding {
            match RtpHeader::unpad_payload(data) {
                Some(d) => d,
                None => {
                    trace!("unpadding of unprotected payload failed");
                    return;
                }
            }
        } else {
            data
        };

        // The header padding flag alone does not mean the packet contains only padding.
        // Authenticate and unpad first; media payload needs a mapped stream to be processed.
        if stream.is_none() && !data.is_empty() {
            return;
        }

        if let Some(raw_packets) = &mut self.raw_packets {
            raw_packets.push_back(Box::new(RawPacket::RtpRx(header.clone(), data.to_vec())));
        }

        // Mark as received for TWCC purposes
        if let Some(transport_cc) = header.ext_vals.transport_cc {
            let prev = self.twcc_rx_register.max_seq();
            let extended = extend_u16(prev.map(|s| *s), transport_cc);
            self.twcc_rx_register.update_seq(extended.into(), now);
        }

        // Store largest seen seq_no for the SSRC. This is used in case we get SSRC changes
        // like A -> B -> A. When we go back to A, we must keep the ROC.
        update_max_seq(&mut self.max_rx_seq_lookup, header.ssrc, seq_no);

        let Some(stream) = stream else {
            return;
        };
        // A receive stream exists only when MID/SSRC mapping succeeded.
        let (mid, ssrc) = mapped.unwrap();

        // Register reception in nack registers.
        let receipt_outer = stream.update_register(now, &header, clock_rate, is_repair, seq_no);

        // RED-recovered packets (frame mode) are collected here and delivered after the
        // primary, filling single-packet losses from the next packet's redundancy.
        let mut red_recovered: Vec<(RtpHeader, Arc<[u8]>, SeqNo, MediaTime)> = Vec::new();

        // Capture the full wire payload length before RED unwrapping narrows `data` to the primary
        // block. The sender counts the whole RED-wrapped payload, so receive stats must too.
        let wire_payload_len = data.len();

        // RTX packets must be rewritten to be a normal packet. This only changes the
        // the seq_no, however MediaTime might be different when interpreted against the
        // the "main" register.
        let (receipt, data) = if is_repair {
            // Drop RTX packets that are just empty padding. The payload here
            // is empty because we would have done RtpHeader::unpad_payload above.
            // For unpausing, it's enough with the stream.update() already done above.
            if data.is_empty() {
                return;
            }

            // Rewrite the header, and strip the resent seq_no prefix from the body.
            let data = stream.un_rtx(&mut header, data, pt);

            let max_seq_lookup = make_max_seq_lookup(&self.max_rx_seq_lookup);

            // Header has changed, which means we extend a new seq_no. This time
            // without is_repair since this is the wrapped resend. This is the
            // extended number of the main stream.
            seq_no = stream.extend_seq(&header, false, max_seq_lookup);

            // header.ssrc is changed by un_rtx() above to be main SSRC.
            update_max_seq(&mut self.max_rx_seq_lookup, header.ssrc, seq_no);

            // Now update the "main" register with the repaired packet info.
            let receipt = stream.update_register(now, &header, clock_rate, false, seq_no);
            (receipt, data)
        } else if is_red && !self.rtp_mode {
            // Frame mode: RED rides the main SSRC/seq. Decode to the primary (Opus) payload
            // and rewrite the header PT; drop the packet if the RED structure is malformed.
            let Some(un) = un_red(&mut header, data, pt) else {
                return;
            };
            // Each redundant block carries an earlier frame. Place it at the sequence number it
            // belongs to (derived from its timestamp and the packets received around it, see
            // `StreamRx::red_locate_seq`) and, if that is still missing, deliver it as a
            // recovered packet so a loss is filled without a retransmission. Only same-PT blocks
            // are considered: RFC 2198 permits a redundant block of another codec, which must
            // never be fed to this depayloader.
            for b in red_same_pt_blocks(&un.redundant, *pt) {
                // Skip recovery (rather than inject a bad timestamp) if the offset somehow
                // exceeds the primary's extended media time.
                let Some(numer) = receipt_outer
                    .time
                    .numer()
                    .checked_sub(b.timestamp_offset as u64)
                else {
                    continue;
                };
                // `None` when the block can't be tied to a currently-missing seq no (see
                // `red_locate_seq` for the exact vs. best-effort cases), or when that seq no
                // was already received or recovered. Recovery only ever fills a hole.
                let Some(rec_seq) = stream.red_locate_seq(seq_no, numer) else {
                    continue;
                };
                // Mark it so the seq is neither NACKed nor recovered again from a later packet.
                // It is not counted as received (it was lost on the wire), so reception reports
                // still carry the real loss. `red_locate_seq` now rejects this seq, which also
                // dedups a malformed packet repeating the same offset.
                stream.mark_red_recovered(rec_seq, numer);

                let rec_time = MediaTime::new(numer, clock_rate);
                let mut rec_header = header.clone();
                rec_header.sequence_number = *rec_seq as u16;
                // RFC 2198: the marker bit is not carried for redundant blocks. Clearing it also
                // stops a recovered (historical) audio frame being seen as a start-of-talkspurt.
                rec_header.marker = false;
                // Redundant blocks carry no header extensions, so a recovered frame cannot carry
                // its own (e.g. audio level). Clear the carrier's extensions rather than let the
                // recovered frame inherit the current packet's metadata.
                rec_header.ext_vals = ExtensionValues::default();
                red_recovered.push((rec_header, Arc::from(b.payload), rec_seq, rec_time));
            }
            (receipt_outer, un.primary)
        } else {
            // This is not RTX, the outer seq and time is what we use. The first
            // stream.update will have updated the main register.
            (receipt_outer, data)
        };

        // Probe packets (SSRC 0) contain only padding, no real media to process
        if ssrc.is_probe() {
            return;
        }

        // For RED (frame mode) `data` is now just the unwrapped primary; count the full RED wire
        // payload instead so receive accounting matches the sender's byte count.
        let rx_bytes = if is_red && !self.rtp_mode {
            wire_payload_len
        } else {
            data.len()
        };
        self.media_bytes_rx += rx_bytes as u64;

        // One-shot conversion to Arc<[u8]>: a single allocation that copies
        // only the trimmed payload bytes out of the SRTP scratch buffer.
        let payload: Arc<[u8]> = Arc::from(data);

        let packet = stream.handle_rtp(now, header, payload, seq_no, receipt.time, rx_bytes);

        if self.rtp_mode {
            // In RTP mode, we store the packet temporarily here for the next poll_output().
            // However only if this is a packet not seen before. This filters out spurious
            // resends for padding.
            if receipt.is_new_packet {
                self.pending_packet = Some(packet);
            }
        } else {
            // In non-RTP mode, we let the Media use a Depayloader.
            // unwrap is fine because mid_and_ssrc_for_header guarantees it.
            let media = self.medias.iter_mut().find(|m| m.mid() == mid).unwrap();
            media.depayload(
                stream.rid(),
                packet,
                self.reordering_size_audio,
                self.reordering_size_video,
                &self.codec_config,
            );
        }

        // Deliver any RED-recovered packets (frame mode only) through the same depayload
        // path. The jitter buffer orders them and dedups against any later real arrival.
        for (rec_header, rec_payload, rec_seq, rec_time) in red_recovered {
            // make_rtp_packet (not handle_rtp): recovered packets must not inflate RX stats.
            let rec_packet =
                stream.make_rtp_packet(now, rec_header, rec_payload, rec_seq, rec_time);
            let media = self.medias.iter_mut().find(|m| m.mid() == mid).unwrap();
            media.depayload(
                stream.rid(),
                rec_packet,
                self.reordering_size_audio,
                self.reordering_size_video,
                &self.codec_config,
            );
        }
    }

    /// Identifies candidates for padding receipt before the main SSRC is discovered.
    ///
    /// MID and repair RID must identify a negotiated simulcast layer, and the codec must
    /// support RTX. This limits the fallback to expected repair traffic instead of accepting
    /// arbitrary unmapped SSRCs. These header checks do not authenticate the packet or prove
    /// it contains only padding; the receive path verifies both before recording a receipt.
    fn is_early_rtx_padding(&self, header: &RtpHeader) -> bool {
        let Some(mid) = header.ext_vals.mid else {
            return false;
        };
        let Some(media) = self.medias.iter().find(|m| m.mid() == mid) else {
            return false;
        };
        let params = main_payload_params(&self.codec_config, header.payload_type);
        let has_rtx = params.is_some_and(|p| p.resend().is_some());
        let repair_rid = header.ext_vals.rid_repair;
        let is_expected_rid = repair_rid.is_some_and(|rid| media.rids_rx().contains(rid));
        // A regular RID identifies primary traffic, which must go through normal stream discovery.
        header.has_padding && header.ext_vals.rid.is_none() && has_rtx && is_expected_rid
    }

    fn handle_rtcp(&mut self, now: Instant, buf: &[u8], wake: &mut Wake) -> Option<()> {
        let srtp: &mut SrtpContext = self.srtp_rx.as_mut()?;
        let unprotected = srtp.unprotect_rtcp(buf)?;

        Rtcp::read_packet(&unprotected, &mut self.feedback_rx);
        let mut need_configure_pacer = false;
        let mut may_move_timeout = false;

        if let Some(raw_packets) = &mut self.raw_packets {
            for fb in &self.feedback_rx {
                raw_packets.push_back(Box::new(RawPacket::RtcpRx(fb.clone())));
            }
        }

        for fb in RtcpFb::from_rtcp(self.feedback_rx.drain(..)) {
            if let RtcpFb::AppSpecificFeedback(asf) = fb {
                self.pending_app_feedback = Some(AppSpecificFeedback {
                    sender_ssrc: asf.sender_ssrc,
                    media_ssrc: asf.media_ssrc,
                    payload: asf.payload,
                });
                continue;
            }

            // Application feedback above only queues an event. Keep the conservative
            // timer wake for every other report, including reports in a compound packet.
            may_move_timeout = true;

            if let RtcpFb::Twcc(twcc) = fb {
                trace!("Handle TWCC: {:?}", twcc);
                let maybe_records = self.twcc_tx_register.apply_report(twcc, now);

                if let (Some(maybe_records), Some(bwe)) = (maybe_records, &mut self.bwe) {
                    bwe.update(maybe_records, now);
                }
                need_configure_pacer = true;

                // The funky thing about TWCC reports is that they are never stapled
                // together with other RTCP packet. If they were though, we want to
                // handle more packets.
                continue;
            }

            if let RtcpFb::Rrtr((rrtr, ssrc)) = fb {
                // DLRR responder (RFC 3611 §4.5): store the remote's RRTR so we can
                // reply with a DLRR, letting the remote compute RTT.
                let lrr = (rrtr.ntp_time.as_ntp_64() >> 16) as u32;
                let entry = LastRrtr {
                    lrr,
                    received_at: now,
                };

                // Update in place if this SSRC is already pending (preserves queue order).
                if let Some(existing) = self.pending_rrtrs.iter_mut().find(|(s, _)| *s == ssrc) {
                    existing.1 = entry;
                } else {
                    // Cap at 300 entries; drop the oldest if full.
                    if self.pending_rrtrs.len() >= MAX_PENDING_RRTRS {
                        self.pending_rrtrs.pop_front();
                    }
                    self.pending_rrtrs.push_back((ssrc, entry));
                }
                continue;
            }

            if let RtcpFb::DlrrItem(dlrr) = fb {
                self.streams.handle_dlrr(now, dlrr);
                continue;
            }

            if fb.is_for_rx() {
                let Some(stream) = self.streams.stream_rx(&fb.ssrc()) else {
                    continue;
                };
                stream.handle_rtcp(now, fb);
            } else {
                let Some(stream) = self.streams.stream_tx(&fb.ssrc()) else {
                    continue;
                };
                stream.handle_rtcp(now, fb);
            }
        }

        // Not in the above if due to lifetime issues, still okay because the method
        // doesn't do anything when BWE isn't configured.
        if need_configure_pacer {
            self.configure_pacer();
        }

        if !may_move_timeout {
            // Also covers empty reports; raw-packet capture only queues events.
            wake.no_timeout();
        }

        Some(())
    }

    pub fn poll_event(&mut self) -> Option<Event> {
        #[cfg(feature = "_internal_test_exports")]
        {
            if let Some(probe) = self.pending_probe.take() {
                return Some(Event::Probe(probe));
            }
        }

        let can_probe =
            self.bwe.is_some() && self.srtp_tx.is_some() && self.probe_media().is_some();
        if let Some(bwe) = &mut self.bwe {
            // Drain estimates even while unavailable so they cannot be delivered later.
            let estimate = bwe.poll_estimate();
            let estimate = match (can_probe, self.bwe_last_event) {
                (false, Some((last, true))) => Some(last),
                (false, _) => None,
                (true, Some((_, true))) => estimate,
                (true, _) => bwe.last_estimate(),
            };
            if let Some(estimate) = estimate {
                self.bwe_last_event = Some((estimate, can_probe));
                let kind = BweKind::Twcc {
                    estimate,
                    can_probe,
                };
                return Some(Event::EgressBitrateEstimate(kind));
            }
        }

        // If we're not ready to flow media, don't send any events.
        if !self.ready_for_srtp() {
            return None;
        }

        if let Some(raw_packets) = &mut self.raw_packets {
            if let Some(p) = raw_packets.pop_front() {
                return Some(Event::RawPacket(p));
            }
        }

        if let Some(feedback) = self.pending_app_feedback.take() {
            return Some(Event::AppSpecificFeedback(feedback));
        }

        // This must be before pending_packet.take() since we need to emit the unpaused event
        // before the first packet causing the unpause.
        if let Some(paused) = self.streams.poll_stream_paused() {
            return Some(Event::StreamPaused(paused));
        }

        if self.rtp_mode {
            if let Some(packet) = self.pending_packet.take() {
                return Some(Event::RtpPacket(packet));
            }
        }

        if let Some(req) = self.streams.poll_keyframe_request() {
            return Some(Event::KeyframeRequest(req));
        }

        if let Some(report) = self.streams.poll_sender_feedback() {
            return Some(Event::SenderFeedback(report));
        }

        if let Some((mid, bitrate)) = self.streams.poll_remb_request() {
            return Some(Event::EgressBitrateEstimate(BweKind::Remb {
                estimate: bitrate,
                mid,
            }));
        }

        for media in &mut self.medias {
            if media.need_open_event {
                media.need_open_event = false;

                return Some(Event::MediaAdded(MediaAdded {
                    mid: media.mid(),
                    kind: media.kind(),
                    direction: media.direction(),
                    simulcast: media.simulcast().map(|s| s.clone().into()),
                }));
            }

            if media.need_changed_event {
                media.need_changed_event = false;
                return Some(Event::MediaChanged(MediaChanged {
                    mid: media.mid(),
                    direction: media.direction(),
                }));
            }
        }

        None
    }

    fn update_receive_recovery(&mut self) {
        for media in self.medias.iter_mut().filter(|m| m.kind().is_video()) {
            let mid = media.mid();
            for (rid, buf) in media.receive_buffers() {
                let deadline = buf.recovery_candidate().and_then(|(range, received)| {
                    match self.streams.stream_rx_by_midrid(MidRid(mid, rid)) {
                        Some(stream) => stream.recovery_deadline(range, received),
                        None => received.checked_add(Duration::from_millis(300)),
                    }
                });
                buf.set_recovery_deadline(deadline);
            }
        }
    }

    pub fn poll_event_fallible(&mut self, now: Instant) -> Result<Option<Event>, RtcError> {
        // Not relevant in rtp_mode, where the packets are picked up by poll_event().
        if self.rtp_mode {
            return Ok(None);
        }

        self.update_receive_recovery();
        for media in &mut self.medias {
            if let Some(e) =
                media.poll_sample(&self.codec_config, now, self.reordering_timeout_audio)?
            {
                return Ok(Some(Event::MediaData(e)));
            }
        }

        Ok(None)
    }

    fn ready_for_srtp(&self) -> bool {
        self.srtp_rx.is_some() && self.srtp_tx.is_some()
    }

    pub fn poll_datagram(&mut self, now: Instant) -> Option<net::DatagramSend> {
        // Time must have progressed forward from start value.
        if now == already_happened() {
            return None;
        }

        let x = if self.rtp_closing {
            self.poll_feedback()
        } else {
            None.or_else(|| self.poll_feedback())
                .or_else(|| self.poll_packet(now))
        };

        if let Some(x) = &x {
            // In RTP mode we trust the API user feeds the RTP packet sizes they
            // need for the MTU they are targeting. This warning is only for when
            // str0m does the RTP packetization.
            if !self.rtp_mode && x.len() > self.mtu_warn() {
                warn!("RTP above MTU {}: {}", self.mtu_warn(), x.len());
            }
        }

        x
    }

    /// To be called in lieu of [`Self::poll_datagram`] when the owner is not in a position to transmit any
    /// generated feedback, and thus such feedback should be dropped.
    pub fn clear_feedback(&mut self) {
        self.feedback_rx.clear();
        self.feedback_tx.clear();
    }

    pub fn close_rtp(&mut self) {
        if self.rtp_closing {
            return;
        }

        self.rtp_closing = true;
        self.clear_feedback();
        self.streams.reset_send_buffers();

        for reports in ReportList::lists_from_iter(self.streams.local_sender_ssrcs()) {
            self.feedback_tx
                .push_back(Rtcp::Goodbye(Goodbye { reports }));
        }
    }

    pub fn is_rtp_closed(&self) -> bool {
        self.rtp_closing && self.feedback_tx.is_empty()
    }

    fn poll_feedback(&mut self) -> Option<net::DatagramSend> {
        if self.feedback_tx.is_empty() {
            return None;
        }

        // Round to nearest multiple of 4 bytes.
        let encryptable_mtu: usize = (self.mtu() - SRTCP_OVERHEAD) & !3;
        assert!(encryptable_mtu.is_multiple_of(4));

        let mut data = vec![0_u8; encryptable_mtu];

        let mut raw_packets = self.raw_packets.as_mut();
        let output = move |fb| {
            if let Some(raw_packets) = &mut raw_packets {
                raw_packets.push_back(Box::new(RawPacket::RtcpTx(fb)));
            }
        };

        let len = Rtcp::write_packet(&mut self.feedback_tx, &mut data, output);

        if len == 0 {
            return None;
        }

        data.truncate(len);

        let srtp = self.srtp_tx.as_mut()?;
        let protected = srtp.protect_rtcp(&data);

        assert!(
            protected.len() < self.mtu(),
            "Encrypted SRTCP should be less than MTU"
        );

        Some(protected.into())
    }

    fn poll_packet(&mut self, now: Instant) -> Option<DatagramSend> {
        let srtp_tx = self.srtp_tx.as_mut()?;

        // Figure out which, if any, queue to poll
        // The cluster_id is captured by the pacer at poll time, before register_send() might clear it
        let (midrid, cluster_id) = self.pacer.poll_queue()?;
        let stream = self.streams.stream_tx_by_midrid(midrid)?;
        let Some(media) = self.medias.iter().find(|m| m.mid() == stream.mid()) else {
            trace!("Pacer pointed to mid {} which has no media", midrid.mid());
            return None;
        };
        let buf = &mut self.poll_packet_buf;
        let twcc_seq = self.twcc;

        let params = &self.codec_config;
        let exts = media.remote_extmap();

        // TWCC might not be enabled for this m-line. Firefox do use TWCC, but not
        // for audio. This is indiciated via the SDP.
        let twcc_enabled = exts.id_of(Extension::TransportSequenceNumber).is_some();
        let twcc = twcc_enabled.then_some(&mut self.twcc);

        let receipt = stream.poll_packet(now, exts, twcc, params, buf)?;

        let PacketReceipt {
            header,
            seq_no,
            is_padding,
            payload_size,
        } = receipt;

        trace!(payload_size, is_padding, "Poll RTP: {:?}", header);

        #[cfg(feature = "_internal_dont_use_log_stats")]
        {
            let kind = if is_padding { "padding" } else { "media" };

            crate::log_stat!("PACKET_SENT", header.ssrc, payload_size, kind);
        }

        self.pacer.register_send(now, payload_size.into(), midrid);

        if let Some(raw_packets) = &mut self.raw_packets {
            raw_packets.push_back(Box::new(RawPacket::RtpTx(header.clone(), buf.clone())));
        }

        let protected = srtp_tx.protect_rtp(buf, &header, *seq_no);

        if twcc_enabled {
            let packet_id = if let Some(cluster) = cluster_id {
                TwccPacketId::with_cluster(twcc_seq, cluster)
            } else {
                TwccPacketId::new(twcc_seq)
            };
            self.twcc_tx_register
                .register_seq(packet_id, now, payload_size);
        }

        // Update BWE subsystem
        if let Some(bwe) = self.bwe.as_mut() {
            bwe.on_media_sent(payload_size.into(), is_padding, now);
        }

        if !is_padding && !header.ssrc.is_probe() {
            self.media_bytes_tx += payload_size as u64;
        }

        Some(protected.into())
    }

    pub fn poll_timeout(&mut self) -> (Option<Instant>, Reason) {
        if self.rtp_closing {
            return if self.feedback_tx.is_empty() {
                (None, Reason::NotHappening)
            } else {
                (Some(already_happened()), Reason::Feedback)
            };
        }

        self.update_receive_recovery();
        let feedback_at = self.regular_feedback_at();
        let nack_at = self.nack_at();
        let twcc_at = self.twcc_at();
        let pacing_at = self.pacer.poll_timeout();
        let packetize_at = self.medias.iter().filter_map(|m| m.poll_timeout()).min();
        let receive_at = self
            .medias
            .iter_mut()
            .filter_map(|media| media.poll_receive_timeout(self.reordering_timeout_audio))
            .min();
        let paused_at = self.paused_at();
        let send_stream_at = self.streams.send_stream();
        let request_at = self.streams.pending_request();

        // Gives us built-in reason
        let bwe_at = self
            .bwe
            .as_ref()
            .map(|bwe| bwe.poll_timeout())
            // We should never see this reason.
            .unwrap_or((None, Reason::BweDelayControl));

        (feedback_at, Reason::Feedback)
            .soonest((request_at, Reason::Feedback))
            .soonest((nack_at, Reason::Nack))
            .soonest((twcc_at, Reason::Twcc))
            .soonest(pacing_at)
            .soonest((receive_at, Reason::ReceiveReorder))
            .soonest((packetize_at, Reason::Packetize))
            .soonest((paused_at, Reason::PauseCheck))
            .soonest((send_stream_at, Reason::SendStream))
            .soonest(bwe_at)
    }

    pub fn has_mid(&self, mid: Mid) -> bool {
        self.medias.iter().any(|m| m.mid() == mid)
            || self.app.is_some_and(|(app_mid, _)| app_mid == mid)
    }

    fn regular_feedback_at(&self) -> Option<Instant> {
        self.streams.regular_feedback_at(self.intervals)
    }

    fn paused_at(&self) -> Option<Instant> {
        self.streams.paused_at()
    }

    fn nack_at(&self) -> Option<Instant> {
        // Paused streams stop counting as NACK-enabled once their pending retries finish,
        // so an idle session doesn't wake every NACK_MIN_INTERVAL for as long as it lives.
        if !self.streams.any_nack_enabled() {
            return None;
        }

        Some(self.last_nack + NACK_MIN_INTERVAL)
    }

    fn twcc_at(&self) -> Option<Instant> {
        // Early RTX padding can create transport receipts before any receive stream exists.
        // Scheduling by pending receipts ensures that padding still receives timely feedback.
        if self.enable_twcc_feedback && self.twcc_rx_register.has_unreported() {
            Some(self.last_twcc + TWCC_INTERVAL)
        } else {
            None
        }
    }

    pub fn enable_twcc_feedback(&mut self) {
        if !self.enable_twcc_feedback {
            debug!("Enable TWCC feedback");
            self.enable_twcc_feedback = true;
        }
    }

    pub fn visit_stats(&mut self, now: Instant, snapshot: &mut StatsSnapshot) {
        for stream in self.streams.streams_tx() {
            stream.visit_stats(snapshot, now);
        }

        for stream in self.streams.streams_rx() {
            stream.visit_stats(snapshot, now);
        }

        snapshot.tx = self.media_bytes_tx;
        snapshot.rx = self.media_bytes_rx;
        snapshot.bwe_tx = self.bwe.as_ref().and_then(|bwe| bwe.last_estimate());

        snapshot.egress_loss_fraction = self.twcc_tx_register.loss(Duration::from_secs(1), now);
        snapshot.rtt = self.twcc_tx_register.rtt();
        snapshot.ingress_loss_fraction = self.twcc_rx_register.loss();
    }

    pub fn set_bwe_bitrate(&mut self, minimum: Bitrate, desired: Bitrate) {
        if let Some(bwe) = self.bwe.as_mut() {
            bwe.set_bitrate(minimum, desired);
            self.configure_pacer();
        }
    }

    pub fn reset_bwe(&mut self, init_bitrate: Bitrate) {
        if let Some(bwe) = self.bwe.as_mut() {
            bwe.reset(init_bitrate);
            self.configure_pacer();
        }
    }

    #[allow(dead_code)]
    pub fn line_count(&self) -> usize {
        self.medias.len() + if self.app.is_some() { 1 } else { 0 }
    }

    pub fn add_media(&mut self, media: Media) {
        self.medias.push(media);
    }

    pub fn medias(&self) -> &[Media] {
        &self.medias
    }

    pub fn remove_media(&mut self, mid: Mid) {
        self.medias.retain(|media| media.mid() != mid);
        self.streams.remove_streams_by_mid(mid);
    }

    fn configure_pacer(&mut self) {
        let Some(bwe) = self.bwe.as_mut() else {
            return;
        };

        let Some(current_estimate) = bwe.last_estimate() else {
            // No estimate yet, no padding
            return;
        };
        let is_overuse = bwe.is_overusing();
        let minimum = bwe.minimum_bitrate();

        let has_active_media = self.has_active_outgoing_media();

        // Calculate pacing and padding rates
        let result =
            self.pacer_control
                .calculate(has_active_media, current_estimate, is_overuse, minimum);

        self.pacer.set_padding_rate(result.padding_rate);
        self.pacer.set_pacing_rate(result.pacing_rate);
    }

    fn has_active_outgoing_media(&self) -> bool {
        // Direct API media has no SDP remote payload list, so Media::disabled()
        // is not an availability check here. Use the same negotiated/configured
        // sending capabilities as probing, including TWCC feedback.
        self.probe_media().is_some()
    }

    pub fn media_by_mid(&self, mid: Mid) -> Option<&Media> {
        self.medias.iter().find(|m| m.mid() == mid)
    }

    pub fn media_by_mid_mut(&mut self, mid: Mid) -> Option<&mut Media> {
        self.medias.iter_mut().find(|m| m.mid() == mid)
    }

    fn do_payload(&mut self, now: Instant) -> Result<(), RtcError> {
        let mtu = self.mtu();
        for m in &mut self.medias {
            m.do_payload(
                now,
                &mut self.streams,
                &self.codec_config,
                self.vp9_packetizer_mode,
                mtu,
                self.codec_config.red_distances(),
            )?;
        }

        Ok(())
    }

    /// Toggle RFC 2198 RED wrapping on the send side at runtime. No renegotiation: RED availability
    /// is fixed at negotiation (or via DirectAPI); this only decides whether we wrap right now.
    /// Returns whether the setting changed.
    pub fn set_red_send(&mut self, mid: Mid, enabled: bool) -> bool {
        let Some(media) = self.media_by_mid_mut(mid) else {
            return false;
        };
        media.set_red_send(enabled)
    }

    pub fn set_direction(&mut self, mid: Mid, direction: Direction) -> bool {
        let Some(media) = self.media_by_mid_mut(mid) else {
            return false;
        };
        let old_dir = media.direction();
        if old_dir == direction {
            return false;
        }

        media.set_direction(direction);

        if old_dir.is_sending() && !direction.is_sending() {
            self.streams.reset_buffers_tx(mid);
        }

        let max_seq_lookup = make_max_seq_lookup(&self.max_rx_seq_lookup);
        if old_dir.is_receiving() && !direction.is_receiving() {
            self.streams.reset_buffers_rx(mid, max_seq_lookup);
        }

        true
    }

    pub fn stop_media(&mut self, mid: Mid) -> bool {
        let Some(media) = self.media_by_mid_mut(mid) else {
            return false;
        };
        if media.disabled() {
            return false;
        }

        media.mark_stopped();
        self.set_direction(mid, Direction::Inactive);

        true
    }

    pub fn is_request_keyframe_possible(&self, kind: KeyframeRequestKind) -> bool {
        // TODO: It's possible to have different set of feedback enabled for different
        // payload types. I.e. we could have FIR enabled for H264, but not for VP8.
        // We might want to make this check more fine grained by testing which PT is
        // in "active use" right now.
        self.codec_config.iter().any(|r| match kind {
            KeyframeRequestKind::Pli => r.fb_pli,
            KeyframeRequestKind::Fir => r.fb_fir,
        })
    }

    /// Checks whether the SRTP contexts are up.
    pub fn is_connected(&self) -> bool {
        self.srtp_rx.is_some() && self.srtp_tx.is_some()
    }
}

pub struct PacketReceipt {
    pub header: RtpHeader,
    pub seq_no: SeqNo,
    pub is_padding: bool,
    pub payload_size: usize,
}

/// Find the PayloadParams for the given Pt, either when the Pt is the main Pt for the Codec or
/// when it's the RTX Pt.
fn main_payload_params(c: &CodecConfig, pt: Pt) -> Option<&PayloadParams> {
    c.iter()
        .find(|p| p.pt == pt || p.resend == Some(pt) || p.red == Some(pt))
}

/// A decoded RED (RFC 2198) packet: the primary block plus any redundant (older) blocks.
struct UnRed<'a> {
    primary: &'a [u8],
    redundant: Vec<RedBlock<'a>>,
}

/// Decode a RED packet, rewrite the header to the primary PT, and return the primary payload
/// plus the redundant blocks. Returns `None` (drop) when the RED structure is malformed.
fn un_red<'a>(header: &mut RtpHeader, data: &'a [u8], pt: Pt) -> Option<UnRed<'a>> {
    let blocks = RedDecoder::decode(data).ok()?;
    let mut primary = None;
    let mut redundant = Vec::new();
    for b in blocks {
        if b.is_primary {
            // RFC 2198 lets redundant blocks use a different PT, but the primary block must carry
            // the codec this RED stream negotiated (`pt`, e.g. Opus). A mismatch means this is not
            // the primary we expect, so drop the packet rather than mislabel a foreign codec (e.g.
            // reinterpret a PCMU primary as Opus).
            if b.pt != *pt {
                return None;
            }
            primary = Some(b.payload);
        } else {
            redundant.push(b);
        }
    }
    header.payload_type = pt;
    Some(UnRed {
        primary: primary?,
        redundant,
    })
}

fn make_max_seq_lookup(map: &HashMap<Ssrc, SeqNoWindow>) -> impl Fn(Ssrc) -> Option<SeqNo> + '_ {
    |ssrc| map.get(&ssrc).map(|s| s.max_seq())
}

fn update_max_seq(map: &mut HashMap<Ssrc, SeqNoWindow>, ssrc: Ssrc, seq_no: SeqNo) {
    map.entry(ssrc)
        .and_modify(|window| window.update(seq_no))
        .or_insert_with(|| SeqNoWindow::new(seq_no));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RtcConfig;
    use crate::io::DATAGRAM_MTU_TARGET;
    use crate::packet::RedEncoder;

    use crate::poll::Readiness;
    use crate::rtp_::RtcpPacket;

    fn keyed_session() -> Session {
        let mut session = Session::new(&RtcConfig::default());
        let crypto = crate::crypto::test_default_provider();
        let profile = SrtpProfile::AEAD_AES_128_GCM;
        let mat = KeyingMaterial::new(&vec![0; profile.keying_material_len()]);
        session.set_keying_material(mat, crypto, profile, true);
        session
    }

    #[test]
    fn readiness_closed_session_timeout_preserves_pending_bye() {
        let now = Instant::now();
        let mut session = keyed_session();
        session
            .streams
            .declare_stream_tx(42.into(), None, MidRid("vid".into(), None));
        session.close_rtp();
        let before = session.poll_timeout();
        assert_eq!(before.1, Reason::Feedback);
        let mut readiness = Readiness::idle();
        session.handle_timeout(now, &mut readiness.wake()).unwrap();
        assert!(!readiness.wants_events());
        assert!(!readiness.wants_timeout());
        assert_eq!(session.poll_timeout(), before);
        assert!(
            session.poll_datagram(now).is_some(),
            "existing BYE must still drain"
        );
    }

    #[test]
    fn readiness_malformed_rtp_does_not_wake() {
        let mut session = keyed_session();
        let before = session.poll_timeout();
        let mut readiness = Readiness::idle();
        session.handle_rtp_receive(Instant::now(), &[0x80], &mut readiness.wake());
        assert!(!readiness.wants_events());
        assert!(!readiness.wants_timeout());
        assert_eq!(session.poll_timeout(), before);
        assert!(session.poll_event().is_none());
    }

    #[test]
    fn readiness_valid_rtp_header_keeps_wake_even_without_keys() {
        let mut session = Session::new(&RtcConfig::default());
        let mut readiness = Readiness::idle();
        // SSRC 0 creates a probe receive stream before the missing-key check.
        let packet = [0x80, 96, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        session.handle_rtp_receive(Instant::now(), &packet, &mut readiness.wake());
        assert!(session.streams.has_stream_rx(0.into()));
        assert!(readiness.wants_events());
        assert!(readiness.wants_timeout());
    }

    #[test]
    fn readiness_rejected_rtcp_does_not_wake() {
        for mut session in [Session::new(&RtcConfig::default()), keyed_session()] {
            let before = session.poll_timeout();
            let mut readiness = Readiness::idle();
            session.handle_rtcp_receive(Instant::now(), &[0; 32], &mut readiness.wake());
            assert!(!readiness.wants_events());
            assert!(!readiness.wants_timeout());
            assert_eq!(session.poll_timeout(), before);
            assert!(session.poll_event().is_none());
        }
    }

    #[test]
    fn readiness_rtcp_app_feedback_only_wakes_events() {
        for mixed in [false, true] {
            let now = Instant::now();
            let mut session = keyed_session();
            let before = session.poll_timeout();
            let feedback = crate::rtp_::AppSpecificFeedback {
                sender_ssrc: 1.into(),
                media_ssrc: 2.into(),
                payload: vec![1, 2, 3, 4].into(),
            };
            // Empty RR followed by application feedback, as sent by DirectApi.
            let mut buf = vec![0x80, 201, 0, 1, 0, 0, 0, 1];
            let mut packet = [0; 64];
            let len = feedback.write_to(&mut packet);
            buf.extend_from_slice(&packet[..len]);
            if mixed {
                let pli = Pli {
                    sender_ssrc: 1.into(),
                    ssrc: 2.into(),
                };
                let len = pli.write_to(&mut packet);
                buf.extend_from_slice(&packet[..len]);
            }
            // Equal key material lets the local transmit context act as the peer.
            let packet = session.srtp_tx.as_mut().unwrap().protect_rtcp(&buf);
            let mut readiness = Readiness::idle();
            session.handle_rtcp_receive(now, &packet, &mut readiness.wake());
            assert!(readiness.wants_events());
            assert_eq!(
                readiness.wants_timeout(),
                mixed,
                "other compound reports keep timer invalidation"
            );
            let Some(Event::AppSpecificFeedback(received)) = session.poll_event() else {
                panic!("feedback must remain observable immediately");
            };
            assert_eq!(received.payload, feedback.payload);
            assert_eq!(session.poll_timeout(), before);
        }
    }

    #[test]
    fn readiness_empty_rtcp_report_keeps_raw_packet_events() {
        let mut session = keyed_session();
        session.raw_packets = Some(VecDeque::new());
        let before = session.poll_timeout();
        let packet = session
            .srtp_tx
            .as_mut()
            .unwrap()
            .protect_rtcp(&[0x80, 201, 0, 1, 0, 0, 0, 1]);
        let mut readiness = Readiness::idle();
        session.handle_rtcp_receive(Instant::now(), &packet, &mut readiness.wake());
        assert!(readiness.wants_events());
        assert!(!readiness.wants_timeout());
        assert!(matches!(session.poll_event(), Some(Event::RawPacket(_))));
        assert_eq!(session.poll_timeout(), before);
    }

    #[test]
    fn session_mtu_matches_config() {
        let cfg = RtcConfig::default();
        let s = Session::new(&cfg);
        assert_eq!(s.mtu(), DATAGRAM_MTU_TARGET);

        let cfg = RtcConfig::default().set_mtu(900..=1280);
        let s = Session::new(&cfg);
        assert_eq!(s.mtu(), 900);
    }

    #[test]
    fn un_red_rejects_a_primary_payload_type_mismatch() {
        let expected_primary_pt = Pt::new_with_value(111);
        let data = RedEncoder::encode(0, &[1, 2, 3], &[]);
        let mut header = RtpHeader::default();

        assert!(
            un_red(&mut header, &data, expected_primary_pt).is_none(),
            "RED associated with Opus must not reinterpret a PCMU primary block as Opus"
        );
    }
}
