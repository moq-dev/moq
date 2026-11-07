use std::fmt;
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::Instant;

use crate::packet::MediaKind;
use crate::rtp_::{Direction, ExtensionValues, MediaTime, Mid, Pt, Rid, SenderInfo, SeqNo, Ssrc};
use crate::sdp::SimulcastLayer as SdpSimulcastLayer;
use crate::sdp::{RestrictionId, Simulcast as SdpSimulcast, SimulcastGroups as SdpSimulcastGroups};

use super::PayloadParams;
use crate::format::CodecExtra;

impl From<&SdpSimulcastLayer> for SimulcastLayer {
    fn from(layer: &SdpSimulcastLayer) -> Self {
        SimulcastLayer {
            rid: Rid::from(layer.restriction_id.0.as_ref()),
            attributes: layer.attributes.clone(),
        }
    }
}

impl From<&SimulcastLayer> for SdpSimulcastLayer {
    fn from(layer: &SimulcastLayer) -> Self {
        SdpSimulcastLayer {
            restriction_id: RestrictionId::new_active(layer.rid.to_string()),
            attributes: layer.attributes.clone(),
        }
    }
}

impl From<SdpSimulcast> for Simulcast {
    fn from(s: SdpSimulcast) -> Self {
        let send = s.send.iter().map(Into::into).collect();
        let recv = s.recv.iter().map(Into::into).collect();

        Simulcast { send, recv }
    }
}

/// A new media appeared in an Rtc session.
///
/// This event fires both for negotiations triggered by a remote or local offer.
///
/// Does not fire for application media (data channel).
#[derive(Debug, PartialEq, Eq)]
pub struct MediaAdded {
    /// Identifier of the new media.
    pub mid: Mid,

    /// The kind of media carried.
    pub kind: MediaKind,

    /// Current direction.
    pub direction: Direction,

    /// If simulcast is configured, this holds the Rids.
    ///
    /// `a=simulcast:send h;l`
    pub simulcast: Option<Simulcast>,
}

/// A change happening during an SDP re-negotiation.
///
/// This event fires both for re-negotiations triggered by a remote or local offer.
///
/// Does not fire for application media (data channel).
#[derive(Debug, PartialEq, Eq)]
pub struct MediaChanged {
    /// Identifier of the media.
    pub mid: Mid,

    /// Current direction.
    pub direction: Direction,
}

/// Simplified information about the simulcast config from SDP.
///
/// The [full spec][1] covers many cases that are not used by simple simulcast.
///
/// [1]: https://datatracker.ietf.org/doc/html/draft-ietf-mmusic-sdp-simulcast-14
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Simulcast {
    /// The layers used for sending simulcast.
    pub send: Vec<SimulcastLayer>,
    /// The layers used for receiving simulcast.
    pub recv: Vec<SimulcastLayer>,
}

impl Simulcast {
    /// Create a new struct with no layers
    pub fn new() -> Simulcast {
        Simulcast {
            send: vec![],
            recv: vec![],
        }
    }
}

impl Simulcast {
    /// Add a send layer
    pub fn add_send_layer(&mut self, layer: SimulcastLayer) {
        self.send.push(layer);
    }

    /// Add a receive layer
    pub fn add_recv_layer(&mut self, layer: SimulcastLayer) {
        self.recv.push(layer);
    }
}

/// A simulcast layer which has a RID and optional attributes
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct SimulcastLayer {
    /// The layer's rid
    pub rid: Rid,
    /// The layer's attributes per RFC 8851
    pub attributes: Option<Vec<(String, String)>>,
}

impl SimulcastLayer {
    /// Creates a new layer with the rid
    pub fn new(rid: &str) -> SimulcastLayer {
        SimulcastLayer {
            rid: Rid::from(rid),
            attributes: None,
        }
    }

    /// Create a new layer builder for setting attributes
    pub fn new_with_attributes(rid: &str) -> SimulcastLayerBuilder {
        SimulcastLayerBuilder {
            rid: Rid::from(rid),
            attributes: vec![],
        }
    }
}

/// A builder which is used to populate layer attributes
pub struct SimulcastLayerBuilder {
    rid: Rid,
    // We could use a HashMap but that doesn't preserve order - which we need for tests (to validate the
    // resulting SDP). BTreeMap seems like overkill for 4-6 keys, so does HashMap, actually. A simple
    // vector of pairs is perfect: preserves order and is efficient enough.
    attributes: Vec<(String, String)>,
}

impl SimulcastLayerBuilder {
    /// Maximum video width
    pub fn max_width(mut self, max_width: u32) -> Self {
        self.update_or_insert("max-width", max_width.to_string());
        self
    }

    /// Maximum video height
    pub fn max_height(mut self, max_height: u32) -> Self {
        self.update_or_insert("max-height", max_height.to_string());
        self
    }

    /// Maximum bitrate, in bits per second (not kilobits)
    pub fn max_br(mut self, max_br: u32) -> Self {
        self.update_or_insert("max-br", max_br.to_string());
        self
    }

    /// Maximum frame rate, in frames per second, or 0 if none
    pub fn max_fps(mut self, max_fps: u32) -> Self {
        self.update_or_insert("max-fps", max_fps.to_string());
        self
    }

    /// A custom attribute
    pub fn custom(mut self, key: &str, value: &str) -> Self {
        self.update_or_insert(key, value.to_string());
        self
    }

    /// Build the layer
    pub fn build(self) -> SimulcastLayer {
        SimulcastLayer {
            rid: self.rid,
            attributes: if self.attributes.is_empty() {
                None
            } else {
                Some(self.attributes)
            },
        }
    }

    fn update_or_insert(&mut self, key: &str, value: String) {
        for (k, v) in &mut self.attributes {
            if k == key {
                *v = value;
                return;
            }
        }

        self.attributes.push((key.to_string(), value));
    }
}

impl Simulcast {
    pub(crate) fn into_sdp(self) -> SdpSimulcast {
        SdpSimulcast {
            send: SdpSimulcastGroups(self.send.iter().map(Into::into).collect()),
            recv: SdpSimulcastGroups(self.recv.iter().map(Into::into).collect()),
            is_munged: false,
        }
    }
}

/// Video or audio data from the remote peer.
///
/// This is obtained via [`Event::MediaData`][crate::Event::MediaData].
#[derive(PartialEq, Eq)]
pub struct MediaData {
    /// Identifier of the media in the session this media belongs to.
    pub mid: Mid,

    /// Payload type (PT) tells which negotiated codec is being used. Each media
    /// can carry different codecs, the payload type can theoretically change
    /// from one packet to the next.
    pub pt: Pt,

    /// Rtp Stream Id (RID) identifies an RTP stream without referring to its
    /// Synchronization Source (SSRC).
    ///
    /// This is a newer standard that is sometimes used in WebRTC to identify
    /// a stream. Specifically when using Simulcast in Chrome.
    pub rid: Option<Rid>,

    /// Parameters for the codec. This is used to match incoming PT to outgoing PT.
    pub params: PayloadParams,

    /// The RTP media time of this packet. Media time is described as a numerator/denominator
    /// quantity. The numerator is the timestamp field from the RTP header, the denominator
    /// depends on whether this is an audio or video packet.
    ///
    /// For audio the timebase is often 48kHz for video it is 90kHz.
    pub time: MediaTime,

    /// The time of the [`Input::Receive`][crate::Input::Receive] of the first packet
    /// that caused this MediaData.
    ///
    /// In simple SFU setups this can be used as wallclock for [`Writer::write`][crate::media::Writer].
    pub network_time: Instant,

    /// The (RTP) sequence numbers that made up this data.
    pub seq_range: RangeInclusive<SeqNo>,

    /// Whether the data is contiguous from the one just previously emitted. If this is false,
    /// we got an interruption in RTP packets, and the data may or may not be usable in a decoder
    /// without requesting a new keyframe.
    ///
    /// For audio this flag most likely doesn't matter.
    pub contiguous: bool,

    /// The actual packet data a.k.a Frame.
    ///
    /// Bigger frames don't fit in one UDP packet, thus WebRTC RTP is chopping up codec
    /// transmission units into smaller parts.
    ///
    /// This data is a full depayloaded Frame.
    pub data: Arc<[u8]>,

    /// RTP header extensions for this media data. This is taken from the
    /// first RTP header.
    pub ext_vals: ExtensionValues,

    /// Additional codec specific information
    pub codec_extra: CodecExtra,

    /// Sender information from the most recent Sender Report(SR).
    ///
    /// If no Sender Report(SR) has been received this is [`None`].
    pub last_sender_info: Option<SenderInfo>,

    /// First packet of a talkspurt, that is the first packet after a silence period during
    /// which packets have not been transmitted contiguously.
    ///
    /// For audio only when dtx or silence suppression is enabled. For telephone events, the first
    /// packet of an event.
    pub audio_start_of_talk_spurt: bool,
}

impl MediaData {
    /// Return true if MediaData is keyframe independently of Codec
    pub fn is_keyframe(&self) -> bool {
        match self.codec_extra {
            CodecExtra::None => false,
            CodecExtra::Tele(_) => false,
            CodecExtra::H264(h264_extra) => h264_extra.is_keyframe,
            CodecExtra::H265(h265_extra) => h265_extra.is_keyframe,
            CodecExtra::H266(h266_extra) => h266_extra.is_keyframe,
            CodecExtra::Vp8(vp8_extra) => vp8_extra.is_keyframe,
            CodecExtra::Vp9(vp9_extra) => vp9_extra.is_keyframe,
            CodecExtra::Av1(av1_extra) => av1_extra.is_keyframe,
        }
    }
}

/// Details for an incoming a keyframe request (PLI or FIR).
///
/// This is obtained via the [`Event::KeyframeRequest`][crate::Event::KeyframeRequest].
///
/// Sending a keyframe request is done via [`Media::request_keyframe()`][crate::media::Media].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyframeRequest {
    /// The media identifier this keyframe request is for.
    pub mid: Mid,

    /// Rid the keyframe request is for. Relevant when doing simulcast.
    pub rid: Option<Rid>,

    /// The kind of keyframe request (PLI or FIR).
    pub kind: KeyframeRequestKind,
}

/// Type of keyframe request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyframeRequestKind {
    /// Picture Loss Indication (PLI) is a less severe keyframe request that can be
    /// automatically generated by an SFU or by the end peer.
    Pli,

    /// Full Intra Request (FIR) is a more severe keyframe request that should only
    /// be used when it's impossible for an end peer to show a video stream.
    Fir,
}

/// Incoming application-specific Payload-Specific Feedback (PSFB FMT=15, PT=206).
///
/// Emitted when a non-REMB FMT=15 RTCP PSFB message is received. The payload
/// is opaque and application-defined (RFC 4585 Section 6.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSpecificFeedback {
    /// SSRC of the sender of this feedback message.
    pub sender_ssrc: Ssrc,
    /// SSRC of the media source this feedback relates to.
    pub media_ssrc: Ssrc,
    /// Application-dependent payload.
    pub payload: Arc<[u8]>,
}

/// Incoming feedback from a sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SenderFeedback {
    /// The media identifier this feedback is for.
    pub mid: Mid,

    /// Rid the feedback is for. Relevant when doing simulcast.
    pub rid: Option<Rid>,

    /// When the RTCP packet containing the included [`SenderInfo`] was received.
    pub received_at: Instant,

    /// Information about the sender of this report.
    pub sender_info: SenderInfo,
}

impl fmt::Debug for MediaData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaData")
            .field("mid", &self.mid)
            .field("pt", &self.pt)
            .field("rid", &self.rid)
            .field("time", &self.time)
            .field("len", &self.data.len())
            .finish()
    }
}
