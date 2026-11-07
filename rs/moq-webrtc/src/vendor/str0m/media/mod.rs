//! Media (audio/video) related content.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::RtcError;
use crate::change::AddMedia;
use crate::format::CodecConfig;

use crate::packet::{CodecDepacketizer, DepacketizingBuffer, Payloader};
use crate::packet::{RedSender, RedSink, RtpMeta};
use crate::rtp_::ExtensionMap;
use crate::rtp_::MidRid;
use crate::rtp_::SRTP_BLOCK_SIZE;
use crate::rtp_::SRTP_OVERHEAD;
use str0m_proto::Id;

use crate::format::PayloadParams;
use crate::format::Vp9PacketizerMode;
use crate::sdp::Simulcast as SdpSimulcast;
use crate::sdp::{MediaLine, Msid, is_no_stream};
use crate::streams::{RtpPacket, Streams};
use crate::util::already_happened;

mod event;
pub use event::*;

mod tele;

mod writer;
pub use writer::Writer;

pub use crate::packet::MediaKind;
pub use crate::packet::TelephoneEvent;
pub use crate::rtp_::{Direction, ExtensionValues, Frequency, MediaTime, Mid, Pt, Rid};

/// Mid used for SSRC 0 non-media BWE probes.
///
/// Outgoing probes use a negotiated media Mid on the wire and do not require
/// an initialized video/RTX stream.
///
/// These probes carry `transport_cc` for TWCC feedback but no real media.
pub(crate) const MID_PROBE: Mid = Mid::from_array(*b"~]probe\0\0\0\0\0\0\0\0\0");
// A 6-second telephone event at 48 kHz needs about 306 packets.
const MAX_PENDING_PAYLOADS: usize = 512;

#[derive(Debug)]
/// Information about some configured media.
pub struct Media {
    // ========================================= RTP level =========================================
    //
    /// Identifier of this media.
    ///
    /// RTP level.
    mid: Mid,

    /// Canonical name.
    ///
    /// RTP level.
    cname: String,

    /// Rid that we are expecting to see on incoming RTP packets that map to this mid.
    /// Once discovered, we make an entry in `stream_rx`.
    ///
    /// RTP level.
    rids_rx: Rids,

    /// Rid that we can send using the [`Writer`].
    ///
    /// RTP level.
    rids_tx: Rids,

    // ========================================= SDP level =========================================
    //
    /// The index of this media line in the Session::media Vec.
    ///
    /// SDP property.
    index: usize,

    /// "Stream and track" identifiers.
    ///
    /// This is for _outgoing_ SDP.
    ///
    /// SDP property.
    msid: Msid,

    /// Audio or video.
    kind: MediaKind,

    /// Current media direction.
    ///
    /// Can be altered via negotiation.
    ///
    /// SDP property.
    dir: Direction,

    /// Remote PTs negotiated for this media.
    ///
    /// This tells us both the desired priority order of payload types
    /// as well as which PT the remote side wants (in case they are narrowed).
    ///
    /// These must have corresponding entries in Session::codec_config.
    ///
    /// SDP property.
    ///
    /// If this is empty, the m-line is disabled/rejected (port=0 in SDP).
    remote_pts: Vec<Pt>,

    /// Set when this m-line has been stopped via
    /// [`SdpApi::stop_media`](crate::change::SdpApi::stop_media) or
    /// rejected by the remote peer. Independent of `remote_pts` so that
    /// an explicit stop preserves the negotiated PT list in SDP output
    /// (the SDP grammar requires at least one fmt on a port=0 m-line).
    stopped: bool,

    /// Remote extmaps negotiated for this media.
    ///
    /// The corresponding entries must exist in Session::codec_config.
    ///
    /// These are 1-indexed to be exactly like in the SDP.
    remote_exts: ExtensionMap,

    /// [`true`] if this media was created by the remote peer, [`false`] if it was created by us.
    remote_created: bool,

    /// Simulcast configuration, if set.
    ///
    /// SDP property.
    simulcast: Option<SdpSimulcast>,

    // ========================================= Payloaders, etc =========================================
    //
    /// Buffers of incoming RTP packets. These do reordering/jitter buffer and also
    /// depayload from RTP to frames.
    depayloaders: HashMap<(Pt, Option<Rid>), DepacketizingBuffer>,

    /// Payloaders for outoing RTP packets.
    payloaders: HashMap<(Pt, Option<Rid>), PayloaderEntry>,

    /// Whether outgoing packets are wrapped in RFC 2198 RED right now. RED availability is fixed at
    /// negotiation (or set up via DirectAPI); this is the runtime send-side lever, toggled with no
    /// renegotiation. Defaults to `true`, so a negotiated RED PT wraps unless it is turned off.
    red_send_enabled: bool,

    /// Outgoing payloads ordered by their send deadlines or media wallclock times.
    to_payload: VecDeque<ToPayload>,

    /// End time of the last telephone event, for the minimum pause between tones.
    /// The payload queue cannot supply this after the event's packets have been sent.
    last_tele_end: Option<Instant>,

    pub(crate) need_open_event: bool,
    pub(crate) need_changed_event: bool,

    /// When converting media lines to SDP, it's easier to represent the app m-line
    /// as a Media. This field is true when we do that. No Session::medias will have
    /// this set to true – they only exist temporarily.
    pub(crate) app_tmp: bool,
}

#[derive(Debug)]
/// Config value for [`Media::rids_rx()`] and [`Media::rids_tx()`]
pub enum Rids {
    /// No rid is allowed.
    None,
    /// Any Rid is allowed.
    ///
    /// This is the default value for direct API.
    Any,
    /// These specific [`Rid`] are allowed.
    ///
    /// This is the default value for Simulcast configured via SDP.
    Specific(Vec<Rid>),
}

impl Rids {
    pub(crate) fn contains(&self, rid: Rid) -> bool {
        match self {
            Rids::None => false,
            Rids::Any => true,
            Rids::Specific(v) => v.contains(&rid),
        }
    }

    pub(crate) fn is_specific(&self) -> bool {
        matches!(self, Rids::Specific(_))
    }

    fn add(&mut self, rid: Rid) {
        match self {
            Rids::None | Rids::Any => {
                *self = Rids::Specific(vec![rid]);
            }
            Rids::Specific(vec) if !vec.contains(&rid) => vec.push(rid),
            Rids::Specific(_) => {}
        }
    }
}

#[derive(Debug)]
pub(crate) struct ToPayload {
    pub pt: Pt,
    pub rid: Option<Rid>,
    pub wallclock: Instant,
    pub rtp_time: MediaTime,
    /// Optional send deadline, acting as an informal pacer. Telephone packets use it to
    /// spread updates over the event duration; regular media has no deadline.
    pub not_before: Option<Instant>,
    pub start_of_talk_spurt: bool,
    pub data: Arc<[u8]>,
    pub ext_vals: ExtensionValues,
}

/// Per-(pt, rid) outgoing payloader entry stored in [`Media::payloaders`]: the codec-agnostic
/// [`Payloader`] together with its optional RFC 2198 RED send state. Bundling them keeps a single
/// map keyed by (pt, rid) rather than parallel maps on the same key.
#[derive(Debug)]
struct PayloaderEntry {
    payloader: Payloader,
    red: Option<RedSender>,
}

impl Media {
    /// Identifier of the media.
    ///
    /// RTP level.
    pub fn mid(&self) -> Mid {
        self.mid
    }

    /// Canonical name.
    ///
    /// Persistent transport-level identifier for an RTP source.
    ///
    /// RTP level property. The value is sent in RTCP reports for `StreamTx`. Incoming
    /// cnames can be found in [`StreamRx::cname`][crate::rtp::StreamRx::cname].
    pub fn cname(&self) -> &str {
        &self.cname
    }

    /// Add rid as one we are expecting to receive for this mid.
    ///
    /// This is used for situations where we don't know the SSRC upfront, such as not having
    /// a=ssrc lines in an SDP. Adding a rid means we are dynamically discovering the SSRC from
    /// a mid/rid combination in the RTP header extensions.
    ///
    /// RTP level.
    pub fn expect_rid_rx(&mut self, rid: Rid) {
        self.rids_rx.add(rid);
    }

    /// Rids we are expecting to see on incoming RTP packets that map to this mid.
    ///
    /// By default this is set to [`Rids::Any`], which changes to [`Rids::Specific`] via SDP negotiation
    /// that configures Simulcast where specific rids are expected.
    ///
    /// RTP level.
    pub fn rids_rx(&self) -> &Rids {
        &self.rids_rx
    }

    /// Rids we are can send via the [`Writer`].
    ///
    /// By default this is set to [`Rids::None`], which changes to [`Rids::Specific`] via SDP negotiation
    /// that configures Simulcast where specific rids are expected.
    ///
    /// RTP level.
    pub fn rids_tx(&self) -> &Rids {
        &self.rids_tx
    }

    pub(crate) fn index(&self) -> usize {
        self.index
    }

    pub(crate) fn msid(&self) -> &Msid {
        &self.msid
    }

    /// Identifier for the group this Media belongs to.
    pub fn stream_id(&self) -> &str {
        &self.msid().stream_id
    }

    /// Identifier for this Media. Should be unique for the given stream id.
    pub fn track_id(&self) -> &str {
        &self.msid().track_id
    }

    /// Whether this media is audio or video.
    ///
    /// SDP level property.
    pub fn kind(&self) -> MediaKind {
        self.kind
    }

    /// Current direction. This can be changed using
    /// [`SdpApi::set_direction()`][crate::SdpApi::set_direction()] followed by an SDP negotiation.
    ///
    /// To test whether it's possible to send media with the current direction, use
    ///
    /// ```no_run
    /// # use str0m::media::Media;
    /// let media: Media = todo!(); // Get hold of media row.
    /// if media.direction().is_sending() {
    ///     // media.write(...);
    /// }
    /// ```
    ///
    /// SDP level property.
    pub fn direction(&self) -> Direction {
        self.dir
    }

    /// Whether this m-line is disabled/rejected (port=0 in SDP).
    ///
    /// An m-line is disabled if it has been stopped (via
    /// [`SdpApi::stop_media`](crate::change::SdpApi::stop_media) or by the
    /// remote peer), or if no codecs matched during negotiation.
    ///
    /// SDP level property.
    pub fn disabled(&self) -> bool {
        self.stopped || self.remote_pts.is_empty()
    }

    /// Whether this m-line has been stopped.
    ///
    /// Unlike [`disabled`](Self::disabled) this does not include the "no
    /// codecs matched" case - only explicit stop via
    /// [`SdpApi::stop_media`](crate::change::SdpApi::stop_media) or a
    /// port=0 m-line received from the remote peer. A stopped m-line
    /// cannot be reactivated; its slot can however be recycled by a
    /// subsequent new m-line (RFC 8829 §5.2.2).
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    pub(crate) fn mark_stopped(&mut self) {
        self.stopped = true;
    }

    pub(crate) fn simulcast(&self) -> Option<&SdpSimulcast> {
        self.simulcast.as_ref()
    }

    pub(crate) fn poll_sample(
        &mut self,
        params: &[PayloadParams],
        now: Instant,
        audio_timeout: Option<Duration>,
    ) -> Result<Option<MediaData>, RtcError> {
        for ((pt, rid), buf) in &mut self.depayloaders {
            if let Some(r) = buf.pop(now, audio_timeout) {
                let dep = r.map_err(|e| RtcError::Packet(self.mid, *pt, e))?;
                let Some(codec) = params.iter().find(|c| c.pt() == *pt) else {
                    return Ok(None);
                };
                // Telephone payloads are audio media even though Codec::is_audio()
                // excludes their payload format.
                let audio_start_of_talk_spurt =
                    codec.spec().codec.kind().is_audio() && dep.start_of_talkspurt();
                return Ok(Some(MediaData {
                    mid: self.mid,
                    pt: *pt,
                    rid: *rid,
                    params: *codec,
                    // The depacketized time is in the RTP wire clock rate. For the
                    // media (samples/frame) API we present it in the codec's nominal
                    // clock rate. These differ only for G722, which has a 16 kHz
                    // nominal rate but an 8 kHz RTP clock rate (RFC 3551 §4.5.2). For
                    // all other codecs this rebase is a no-op. See
                    // https://en.wikipedia.org/wiki/RTP_payload_formats#cite_note-55
                    time: dep.time.rebase(codec.spec().clock_rate),
                    network_time: dep.first_network_time(),
                    seq_range: dep.seq_range(),
                    contiguous: dep.contiguous,
                    ext_vals: dep.ext_vals(),
                    codec_extra: dep.codec_extra,
                    last_sender_info: dep.first_sender_info(),
                    audio_start_of_talk_spurt,
                    data: dep.data.into(),
                }));
            }
        }
        Ok(None)
    }

    pub(crate) fn depayload(
        &mut self,
        rid: Option<Rid>,
        packet: RtpPacket,
        reordering_size_audio: usize,
        reordering_size_video: usize,
        params: &[PayloadParams],
    ) {
        if !self.dir.is_receiving() {
            return;
        }

        let pt = packet.header.payload_type;

        let key = (pt, rid);

        let exists = self.depayloaders.contains_key(&key);

        if !exists {
            // This unwrap is ok, because the handle_input doesn't accept the RtpPacket for
            // depayloading unless we have matched the PT to one in the session.
            let params = params.iter().find(|p| p.pt == pt).unwrap();

            let codec = params.spec.codec;

            // How many packets to hold back in the jitter buffer.
            let hold_back = if codec.is_tele() {
                // Telephone-event reports are self-contained, so never wait for missing packets.
                0
            } else if codec.is_audio() {
                reordering_size_audio
            } else {
                reordering_size_video
            };

            let mut depack: CodecDepacketizer = codec.into();

            if let CodecDepacketizer::Tele(ref mut tele) = depack {
                tele.clock_rate = params.spec.rtp_clock_rate();
            }

            // Enable DONL for H.265 when sprop-max-don-diff > 0 (RFC 7798 §7.1)
            if let CodecDepacketizer::H265(ref mut h265) = depack {
                if params.spec.format.sprop_max_don_diff.unwrap_or(0) > 0 {
                    h265.with_donl(true);
                }
            }

            // Enable DONL for H.266 when sprop-max-don-diff > 0 (RFC 9328 §7.2)
            if let CodecDepacketizer::H266(ref mut h266) = depack {
                if params.spec.format.sprop_max_don_diff.unwrap_or(0) > 0 {
                    h266.with_donl(true);
                }
            }

            // RFC 4867: without octet-align=1 the payload is bandwidth-efficient.
            if let CodecDepacketizer::AmrWb(ref mut amr) = depack {
                amr.octet_align = params.spec.format.octet_align.unwrap_or(false);
            }

            let buffer = DepacketizingBuffer::new(depack, hold_back);

            self.depayloaders.insert((pt, rid), buffer);
        }

        let meta = RtpMeta {
            received: packet.timestamp,
            time: packet.time,
            seq_no: packet.seq_no,
            header: packet.header.clone(),
            last_sender_info: packet.last_sender_info,
        };

        for ((other_pt, other_rid), buffer) in &mut self.depayloaders {
            if *other_pt != pt && *other_rid == rid {
                buffer.push_padding(meta.clone());
            }
        }

        // The entry will be there by now.
        let buffer = self.depayloaders.get_mut(&key).unwrap();

        buffer.push(meta, packet.payload);
    }

    pub(crate) fn set_cname(&mut self, cname: String) {
        self.cname = cname;
    }

    pub(crate) fn set_msid(&mut self, msid: Msid) {
        self.msid = msid;
    }

    pub(crate) fn set_direction(&mut self, new_dir: Direction) {
        self.need_changed_event = self.dir != new_dir;
        self.dir = new_dir;

        if !new_dir.is_sending() {
            self.to_payload.clear();
            self.last_tele_end = None;
        }
    }

    /// Toggle RFC 2198 RED wrapping for outgoing packets. Takes effect on the next payloaded
    /// packet; RED must be available (negotiated or set via DirectAPI) for `true` to wrap.
    /// Returns whether the setting changed.
    pub(crate) fn set_red_send(&mut self, enabled: bool) -> bool {
        if self.red_send_enabled == enabled {
            return false;
        }
        self.red_send_enabled = enabled;
        true
    }

    pub(crate) fn set_simulcast(&mut self, s: SdpSimulcast) {
        debug!("Set simulcast: {:?}", s);
        self.simulcast = Some(s);
    }

    fn payloader_for(
        &mut self,
        pt: Pt,
        rid: Option<Rid>,
        params: &[PayloadParams],
        vp9_mode: Vp9PacketizerMode,
    ) -> &mut PayloaderEntry {
        self.payloaders.entry((pt, rid)).or_insert_with(|| {
            // Unwrap is OK, the pt should be checked already when calling this function.
            let params = params.iter().find(|p| p.pt == pt).unwrap();
            PayloaderEntry {
                payloader: Payloader::new(params.spec, vp9_mode),
                red: None,
            }
        })
    }

    fn queue_position(&self, to_payload: &ToPayload) -> usize {
        let queue_time = to_payload.not_before.unwrap_or(to_payload.wallclock);
        self.to_payload
            .iter()
            .position(|p| p.not_before.unwrap_or(p.wallclock) > queue_time)
            .unwrap_or(self.to_payload.len())
    }

    fn set_to_payload(&mut self, to_payload: ToPayload) -> Result<(), RtcError> {
        let position = self.queue_position(&to_payload);
        self.to_payload.insert(position, to_payload);
        if self.to_payload.len() > MAX_PENDING_PAYLOADS {
            self.to_payload.remove(position);
            return Err(RtcError::WriteWithoutPoll);
        }
        Ok(())
    }

    pub(crate) fn telephone_event_too_soon(&self, start: Instant) -> bool {
        self.last_tele_end
            .is_some_and(|end| start < end + tele::MIN_PAUSE)
    }

    pub(crate) fn queue_telephone_packets(
        &mut self,
        packets: impl Iterator<Item = ToPayload>,
        end: Instant,
    ) -> Result<(), RtcError> {
        for packet in packets {
            self.set_to_payload(packet)?;
        }
        self.last_tele_end = Some(end);
        Ok(())
    }

    pub(crate) fn poll_timeout(&self) -> Option<Instant> {
        self.to_payload
            .front()
            .map(|p| p.not_before.unwrap_or_else(already_happened))
    }

    pub(crate) fn receive_buffers(
        &mut self,
    ) -> impl Iterator<Item = (Option<Rid>, &mut DepacketizingBuffer)> {
        self.depayloaders
            .iter_mut()
            .map(|((_, rid), buf)| (*rid, buf))
    }

    pub(crate) fn poll_receive_timeout(
        &mut self,
        audio_timeout: Option<Duration>,
    ) -> Option<Instant> {
        self.depayloaders
            .values_mut()
            .filter_map(|buf| buf.poll_timeout(audio_timeout))
            .min()
    }

    pub(crate) fn do_payload(
        &mut self,
        now: Instant,
        streams: &mut Streams,
        params: &[PayloadParams],
        vp9_mode: Vp9PacketizerMode,
        mtu: usize,
        red_distances: &[u32],
    ) -> Result<(), RtcError> {
        let Some(front) = self.to_payload.front() else {
            return Ok(());
        };
        let is_waiting = front.not_before.is_some_and(|deadline| deadline > now);
        if is_waiting {
            return Ok(());
        }

        let to_payload = self
            .to_payload
            .pop_front()
            .expect("front was checked above");
        let ToPayload { pt, rid, .. } = &to_payload;

        let midrid = MidRid(self.mid, *rid);

        let stream = streams.stream_tx_by_midrid(midrid);

        let Some(stream) = stream else {
            return Err(RtcError::NoSenderSource);
        };

        let pt = *pt;

        let rtp_size: usize = mtu - SRTP_OVERHEAD;
        // align to SRTP block size to minimize padding needs
        let aligned_mtu: usize = rtp_size - rtp_size % SRTP_BLOCK_SIZE;

        // The RED PT to wrap into, if RED is negotiated for this codec and send-side wrapping is
        // on. When off we send on the plain PT even though RED stays negotiated on the m-line (it
        // can be toggled back on with no renegotiation).
        let red_pt = if self.red_send_enabled {
            params.iter().find(|p| p.pt == pt).and_then(|p| p.red)
        } else {
            None
        };

        let PayloaderEntry { payloader, red } = self.payloader_for(pt, *rid, params, vp9_mode);

        let result = if let Some(red_pt) = red_pt {
            // Keep the RED sender current with the negotiated PT/pattern without discarding a live
            // talk-spurt's history. Reserve the 1-byte RED primary header so the wrapped packet
            // still fits the MTU; the sink uses the full aligned MTU as its shed budget.
            let red = red.get_or_insert_with(|| RedSender::new(red_pt, pt, red_distances));
            red.sync(red_pt, pt, red_distances);

            let packetize_mtu = aligned_mtu.saturating_sub(1);
            let mut sink = RedSink::new(stream, red, aligned_mtu);
            payloader.push_sample(to_payload, packetize_mtu, &mut sink)
        } else {
            // Drop any stale sender so its history does not persist across a RED disable.
            *red = None;
            payloader.push_sample(to_payload, aligned_mtu, stream)
        };

        result.map_err(|e| RtcError::Packet(self.mid, pt, e))
    }

    pub(crate) fn set_remote_pts(&mut self, pts: Vec<Pt>) {
        // Have we already set PTs?
        if !self.remote_pts.is_empty() {
            return;
        }

        // TODO: We should verify the remote peer doesn't suddenly change the PT
        // order or removes/adds PTs that weren't there from the start.
        debug!("Mid ({}) remote PT order is: {:?}", self.mid, pts);
        self.remote_pts = pts;
    }

    pub(crate) fn set_remote_extmap(&mut self, exts: ExtensionMap) {
        self.remote_exts = exts;
    }

    /// The remote PT (payload types) configured for this Media.
    ///
    /// These are negotiated with the remote peer and is the order the remote prefer them.
    ///
    /// I.e. these can be fewer than the `PayloadParams` configured for the `Rtc` instance,
    /// and in a different order.
    pub fn remote_pts(&self) -> &[Pt] {
        &self.remote_pts
    }

    /// The remote, agreed on, extension map, configured for this Media.
    ///
    /// For the SDP API, these are negotiated with the remote peer.
    ///
    /// For the Direct API, these are a clone of the session configured values narrowed by media
    /// kind (audio/video).
    pub fn remote_extmap(&self) -> &ExtensionMap {
        &self.remote_exts
    }

    pub(crate) fn remote_created(&self) -> bool {
        self.remote_created
    }

    pub(crate) fn first_pt_with_rtx(&self, config: &CodecConfig) -> Option<Pt> {
        config
            .all_for_kind(self.kind, false)
            // Only consider negotiated PTs
            .filter(|p| self.remote_pts.contains(&p.pt))
            // Map to the first PT found in payload params with RTX
            .find_map(|p| p.resend().map(|_| p.pt))
    }

    pub(crate) fn reset_depayloaders_for_rid(&mut self, rid: Option<Rid>) {
        // A replacement SSRC can use any negotiated payload type for this RID.
        self.depayloaders
            .retain(|(_, existing_rid), _| *existing_rid != rid);
    }

    pub(crate) fn set_rid_rx(&mut self, rids: Rids) {
        self.rids_rx = rids;
    }

    pub(crate) fn set_rid_tx(&mut self, rids: Rids) {
        self.rids_tx = rids;
    }

    pub(crate) fn add_to_rid_tx(&mut self, rid: Rid) {
        self.rids_tx.add(rid)
    }
}

impl Default for Media {
    fn default() -> Self {
        Self {
            mid: Mid::new(),
            index: 0,
            app_tmp: false,
            cname: Id::<20>::random().to_string(),
            msid: Msid::random(),
            kind: MediaKind::Video,
            remote_pts: vec![],
            stopped: false,
            remote_exts: ExtensionMap::empty(),
            remote_created: false,
            dir: Direction::SendRecv,
            simulcast: None,
            rids_rx: Rids::Any,
            rids_tx: Rids::None,
            payloaders: HashMap::new(),
            depayloaders: HashMap::new(),
            to_payload: VecDeque::default(),
            last_tele_end: None,
            need_open_event: true,
            need_changed_event: false,
            red_send_enabled: true,
        }
    }
}

impl Media {
    pub(crate) fn from_remote_media_line(
        l: &MediaLine,
        index: usize,
        remote_created: bool,
    ) -> Self {
        Media {
            mid: l.mid(),
            index,
            // This is not reflected back, and thus added by add_pending_changes().
            // cname,
            msid: l
                .msid()
                .filter(|m| !is_no_stream(&m.stream_id))
                .unwrap_or(Msid::random()),
            kind: l.typ.clone().into(),
            dir: if l.disabled {
                Direction::Inactive
            } else {
                l.direction().invert() // remote direction is reverse.
            },
            remote_created,
            ..Default::default()
        }
    }

    // Going from AddMedia to Media for pending in a Change and are sent
    // in the offer to the other side.
    //
    // from_add_media is only used when creating temporary Media to be
    // included in the SDP. We don't want to make an _actual_ changes with this.
    pub(crate) fn from_add_media(a: AddMedia) -> Self {
        Media {
            mid: a.mid,
            index: a.index,
            cname: a.cname,
            msid: a.msid,
            kind: a.kind,
            dir: a.dir,
            remote_pts: a.pts,
            remote_exts: a.exts,
            remote_created: false,
            simulcast: a.simulcast.map(|s| s.into_sdp()),
            ..Default::default()
        }
    }

    pub(crate) fn from_app_tmp(mid: Mid, index: usize) -> Media {
        Media {
            mid,
            index,
            app_tmp: true,
            ..Default::default()
        }
    }

    pub(crate) fn from_direct_api(
        mid: Mid,
        index: usize,
        kind: MediaKind,
        exts: ExtensionMap,
    ) -> Media {
        Media {
            mid,
            index,
            kind,
            dir: Direction::SendRecv,
            remote_exts: exts,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod receive_timeout_test {
    use super::*;
    use crate::format::Codec;
    use crate::rtp_::RtpHeader;

    fn blocked_buffer(base: Instant) -> DepacketizingBuffer {
        let mut buffer = DepacketizingBuffer::new(Codec::Vp8.into(), 30);
        for seq in [1u64, 3] {
            buffer.push(
                RtpMeta {
                    received: base,
                    time: MediaTime::from_90khz(seq * 3000),
                    seq_no: seq.into(),
                    header: RtpHeader {
                        marker: true,
                        ..Default::default()
                    },
                    last_sender_info: None,
                },
                [0x10, 0, 0],
            );
            if seq == 1 {
                buffer.pop(base, None).unwrap().unwrap();
            }
        }
        buffer
    }

    /// Test the earliest deadline covers all payload types and RIDs and changes after buffer resets.
    #[test]
    fn deadlines_cover_payload_types_rids_and_resets() {
        let base = Instant::now();
        let mut media = Media::default();
        let pt1 = Pt::new_with_value(96);
        let pt2 = Pt::new_with_value(98);
        let rid1 = Some("a".into());
        let rid2 = Some("b".into());
        media.depayloaders.insert(
            (pt1, rid1),
            blocked_buffer(base + Duration::from_millis(100)),
        );
        media.depayloaders.insert((pt2, rid1), blocked_buffer(base));
        media.depayloaders.insert(
            (pt1, rid2),
            blocked_buffer(base + Duration::from_millis(50)),
        );
        let timeout = Some(Duration::from_millis(250));
        assert_eq!(
            media.poll_receive_timeout(timeout),
            Some(base + Duration::from_millis(250))
        );
        media.reset_depayloaders_for_rid(rid1);
        assert_eq!(
            media.poll_receive_timeout(timeout),
            Some(base + Duration::from_millis(300))
        );
        media.reset_depayloaders_for_rid(rid2);
        assert_eq!(media.poll_receive_timeout(timeout), None);
        assert_eq!(media.poll_receive_timeout(None), None);
        media.depayloaders.insert((pt1, rid1), blocked_buffer(base));
        assert_eq!(media.poll_receive_timeout(Some(Duration::ZERO)), Some(base));
    }
}
