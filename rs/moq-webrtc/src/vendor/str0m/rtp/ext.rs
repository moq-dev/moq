use std::any::Any;
use std::any::TypeId;
use std::collections::HashMap;
use std::fmt;
use std::fmt::Debug;
use std::hash::BuildHasherDefault;
use std::hash::Hasher;
use std::panic::UnwindSafe;
use std::str::from_utf8;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::util::InstantExt;
use crate::util::SystemTimeExt;
use crate::util::already_happened;
use crate::util::epoch_to_beginning;

use crate::rtp_::Frequency;

use super::mtime::MediaTime;
use super::{Mid, Rid};

/// RTP header extensions.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Extension {
    /// <http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time>
    AbsoluteSendTime,
    /// <http://www.webrtc.org/experiments/rtp-hdrext/abs-capture-time>
    AbsoluteCaptureTime,
    /// <urn:ietf:params:rtp-hdrext:ssrc-audio-level>
    AudioLevel,
    /// <urn:ietf:params:rtp-hdrext:toffset>
    ///
    /// Use when a RTP packet is delayed by a send queue to indicate an offset in the "transmitter".
    /// It effectively means we can set a timestamp offset exactly when the UDP packet leaves the
    /// server.
    TransmissionTimeOffset,
    /// <urn:3gpp:video-orientation>
    VideoOrientation,
    /// <http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01>
    TransportSequenceNumber,
    /// <http://www.webrtc.org/experiments/rtp-hdrext/playout-delay>
    PlayoutDelay,
    /// <http://www.webrtc.org/experiments/rtp-hdrext/video-content-type>
    VideoContentType,
    /// <http://www.webrtc.org/experiments/rtp-hdrext/video-timing>
    VideoTiming,
    /// <urn:ietf:params:rtp-hdrext:sdes:rtp-stream-id>
    ///
    /// UTF8 encoded identifier for the RTP stream. Not the same as SSRC, this is is designed to
    /// avoid running out of SSRC for very large sessions.
    RtpStreamId,
    /// <urn:ietf:params:rtp-hdrext:sdes:rtp-stream-id>
    ///
    /// UTF8 encoded identifier referencing another RTP stream's RtpStreamId. If we see
    /// this extension type, we know the stream is a repair stream.
    RepairedRtpStreamId,
    /// <urn:ietf:params:rtp-hdrext:sdes:mid>
    RtpMid,
    /// <http://tools.ietf.org/html/draft-ietf-avtext-framemarking-07>
    FrameMarking,
    /// <http://www.webrtc.org/experiments/rtp-hdrext/color-space>
    ColorSpace,

    /// Not recognized URI, but it could still be user parseable.
    #[doc(hidden)]
    UnknownUri(String, Arc<dyn ExtensionSerializer>),
}

// All header extensions must have a common "form", either using
// 1 byte for the (ID, len) or 2 bytes for the (ID, len).
// If one extension requires the two byte form
// (probably because of its size, but possibly because of ID),
// The form must be the two-byte variety for all of them.
#[repr(u16)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ExtensionsForm {
    // See RFC 8285 Section 4.2
    // ID Range: 1..=14
    // Length Range: 1..=16
    OneByte = 0xBEDE,
    // See RFC 8285 Section 4.3
    // ID Range: 1..=255
    // Length Range: 0..=255
    TwoByte = 0x1000,
}

pub const MAX_ID_ONE_BYTE_FORM: u8 = 14;
// With the two byte form, it could be 255, but supporting larger values makes the ExtensionMap larger.
// So we support only up to this ID for now.
pub const MAX_ID: u8 = 16;

impl ExtensionsForm {
    pub(crate) fn as_u16(self) -> u16 {
        self as u16
    }

    pub(crate) fn serialize(self) -> [u8; 2] {
        // App bits set to 0
        self.as_u16().to_be_bytes()
    }

    pub(crate) fn parse(bytes: [u8; 2]) -> Option<Self> {
        let serialized = u16::from_be_bytes(bytes);
        if serialized == ExtensionsForm::OneByte.as_u16() {
            Some(ExtensionsForm::OneByte)
        // Ignore the app bits
        } else if (serialized & 0xFFF0) == ExtensionsForm::TwoByte.as_u16() {
            Some(ExtensionsForm::TwoByte)
        } else {
            None
        }
    }
}

// TODO: think this through. Is it unwind safe?
impl UnwindSafe for Extension {}

/// Trait for parsing/writing user RTP header extensions.
pub trait ExtensionSerializer: Debug + Send + Sync + 'static {
    /// Write the extension to the buffer of bytes. Must return the number
    /// of bytes written. This can be 0 if the extension could not be serialized.
    fn write_to(&self, buf: &mut [u8], ev: &ExtensionValues) -> usize;

    /// Parse a value and put it in the [`ExtensionValues::user_values`] field.
    fn parse_value(&self, buf: &[u8], ev: &mut ExtensionValues) -> bool;

    /// Tell if this extension should be used for video media.
    fn is_video(&self) -> bool;

    /// Tell if this extension should be used for audio media.
    fn is_audio(&self) -> bool;

    /// When calling write_to, if the size of the written value may exceed 16 bytes,
    /// or may be 0 bytes, the two byte header extension form must be used.
    /// Otherwise, the one byte form may be used, which is usually the case.
    fn requires_two_byte_form(&self, _ev: &ExtensionValues) -> bool {
        false
    }
}

impl Extension {
    fn requires_two_byte_form(&self, ev: &ExtensionValues) -> bool {
        match self {
            Extension::UnknownUri(_, serializer) => serializer.requires_two_byte_form(ev),
            _ => false,
        }
    }
}

/// This is a placeholder value for when the Extension URI are parsed in an SDP OFFER/ANSWER.
/// The trait write_to() and parse_value() should never be called (that would be a bug).
#[derive(Debug)]
struct SdpUnknownUri;

impl ExtensionSerializer for SdpUnknownUri {
    // If an unreachable happens, it's a bug.
    fn write_to(&self, _buf: &mut [u8], _ev: &ExtensionValues) -> usize {
        unreachable!("Incorrect ExtensionSerializer::write_to")
    }
    fn parse_value(&self, _buf: &[u8], _ev: &mut ExtensionValues) -> bool {
        unreachable!("Incorrect ExtensionSerializer::parse_value")
    }
    fn is_video(&self) -> bool {
        unreachable!("Incorrect ExtensionSerializer::is_video")
    }
    fn is_audio(&self) -> bool {
        unreachable!("Incorrect ExtensionSerializer::is_audio")
    }
}

/// Mapping of extension URI to our enum
const EXT_URI: &[(Extension, &str)] = &[
    (
        Extension::AbsoluteSendTime,
        "http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time",
    ),
    (
        Extension::AbsoluteCaptureTime,
        "http://www.webrtc.org/experiments/rtp-hdrext/abs-capture-time",
    ),
    (
        Extension::AudioLevel,
        "urn:ietf:params:rtp-hdrext:ssrc-audio-level",
    ),
    (
        Extension::TransmissionTimeOffset,
        "urn:ietf:params:rtp-hdrext:toffset",
    ),
    (
        Extension::VideoOrientation, //
        "urn:3gpp:video-orientation",
    ),
    (
        Extension::TransportSequenceNumber,
        "http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01",
    ),
    (
        Extension::PlayoutDelay,
        "http://www.webrtc.org/experiments/rtp-hdrext/playout-delay",
    ),
    (
        Extension::VideoContentType,
        "http://www.webrtc.org/experiments/rtp-hdrext/video-content-type",
    ),
    (
        Extension::VideoTiming,
        "http://www.webrtc.org/experiments/rtp-hdrext/video-timing",
    ),
    (
        Extension::RtpStreamId,
        "urn:ietf:params:rtp-hdrext:sdes:rtp-stream-id",
    ),
    (
        Extension::RepairedRtpStreamId,
        "urn:ietf:params:rtp-hdrext:sdes:repaired-rtp-stream-id",
    ),
    (
        Extension::RtpMid, //
        "urn:ietf:params:rtp-hdrext:sdes:mid",
    ),
    (
        Extension::FrameMarking,
        "http://tools.ietf.org/html/draft-ietf-avtext-framemarking-07",
    ),
    (
        Extension::ColorSpace,
        "http://www.webrtc.org/experiments/rtp-hdrext/color-space",
    ),
];

impl Extension {
    /// Parses an extension from a URI. This only happens for incoming SDP OFFER/ANSWER
    /// while the corresponding Extension with a potential ExtensionSerializer is
    /// in Rtc::session.
    pub(crate) fn from_sdp_uri(uri: &str) -> Self {
        for (t, spec) in EXT_URI.iter() {
            if *spec == uri {
                return t.clone();
            }
        }

        Extension::UnknownUri(uri.to_string(), Arc::new(SdpUnknownUri))
    }

    /// Extension for a uri not handled by str0m itself.
    pub fn with_serializer(uri: &str, s: impl ExtensionSerializer) -> Self {
        Extension::UnknownUri(uri.to_string(), Arc::new(s))
    }

    /// Represents the extension as an URI.
    pub fn as_uri(&self) -> &str {
        for (t, spec) in EXT_URI.iter() {
            if t == self {
                return spec;
            }
        }

        if let Extension::UnknownUri(uri, _) = self {
            return uri;
        }

        "unknown"
    }

    pub(crate) fn is_serialized(&self) -> bool {
        if let Self::UnknownUri(_, s) = self {
            // Check if this Arc contains the SdpUnknownUri.
            let is_sdp = (s as &(dyn Any + 'static))
                .downcast_ref::<SdpUnknownUri>()
                .is_some();

            // If it is the SdpUnknownUri, we are not serializing. If this happens,
            // it's probably a bug. The only way to construct SdpUnknownUri is via SDP,
            // but those values are only for Eq-comparison vs the values in Session.
            // The SdpUnknownUri should not even try to be serialized.
            if is_sdp {
                panic!("is_serialized on SdpUnkownUri, this is a bug");
            }
        }
        true
    }

    fn is_audio(&self) -> bool {
        use Extension::*;

        if let UnknownUri(_, serializer) = self {
            return serializer.is_audio();
        }

        matches!(
            self,
            RtpStreamId
                | RepairedRtpStreamId
                | RtpMid
                | AbsoluteSendTime
                | AbsoluteCaptureTime
                | AudioLevel
                | TransportSequenceNumber
                | TransmissionTimeOffset
                | PlayoutDelay
        )
    }

    fn is_video(&self) -> bool {
        use Extension::*;

        if let UnknownUri(_, serializer) = self {
            return serializer.is_video();
        }

        matches!(
            self,
            RtpStreamId
                | RepairedRtpStreamId
                | RtpMid
                | AbsoluteSendTime
                | AbsoluteCaptureTime
                | VideoOrientation
                | TransportSequenceNumber
                | TransmissionTimeOffset
                | PlayoutDelay
                | VideoContentType
                | VideoTiming
                | FrameMarking
                | ColorSpace
        )
    }
}

// As of 2022-09-28, for audio google chrome offers these.
// "a=extmap:1 urn:ietf:params:rtp-hdrext:ssrc-audio-level"
// "a=extmap:2 http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time"
// "a=extmap:3 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01"
// "a=extmap:4 urn:ietf:params:rtp-hdrext:sdes:mid"
//
// For video these.
// "a=extmap:2 http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time"
// "a=extmap:3 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01"
// "a=extmap:4 urn:ietf:params:rtp-hdrext:sdes:mid"
// "a=extmap:5 http://www.webrtc.org/experiments/rtp-hdrext/playout-delay"
// "a=extmap:6 http://www.webrtc.org/experiments/rtp-hdrext/video-content-type"
// "a=extmap:7 http://www.webrtc.org/experiments/rtp-hdrext/video-timing"
// "a=extmap:8 http://www.webrtc.org/experiments/rtp-hdrext/color-space"
// "a=extmap:10 urn:ietf:params:rtp-hdrext:sdes:rtp-stream-id"
// "a=extmap:11 urn:ietf:params:rtp-hdrext:sdes:repaired-rtp-stream-id"
// "a=extmap:13 urn:3gpp:video-orientation"
// "a=extmap:14 urn:ietf:params:rtp-hdrext:toffset"

/// Mapping between RTP extension id to what extension that is.
#[derive(Clone, PartialEq, Eq)]
pub struct ExtensionMap([Option<MapEntry>; MAX_ID as usize]); // index 0 is extmap:1.

#[derive(Debug, Clone, PartialEq, Eq)]
struct MapEntry {
    ext: Extension,
    locked: bool,
}

impl ExtensionMap {
    /// Create an empty map.
    pub fn empty() -> Self {
        ExtensionMap(std::array::from_fn(|_| None))
    }

    /// Creates a map with the "standard" mappings.
    ///
    /// The standard are taken from Chrome.
    pub fn standard() -> Self {
        let mut exts = Self::empty();

        exts.set(1, Extension::AudioLevel);
        exts.set(2, Extension::AbsoluteSendTime);
        exts.set(3, Extension::TransportSequenceNumber);
        exts.set(4, Extension::RtpMid);
        // exts.set_mapping(&ExtMap::new(8, Extension::ColorSpace));
        exts.set(10, Extension::RtpStreamId);
        exts.set(11, Extension::RepairedRtpStreamId);
        exts.set(13, Extension::VideoOrientation);

        exts
    }

    pub(crate) fn clear(&mut self) {
        for i in &mut self.0 {
            *i = None;
        }
    }

    /// Set a mapping for an extension.
    ///
    /// The id must be in 1..=MAX_ID (1-indexed).
    pub fn set(&mut self, id: u8, ext: Extension) {
        if id < 1 || id > MAX_ID {
            debug!("Set RTP extension out of range 1-{}: {}", MAX_ID, id);
            return;
        }
        let idx = id as usize - 1;

        let m = MapEntry { ext, locked: false };

        self.0[idx] = Some(m);
    }

    /// Look up the extension for the id.
    ///
    /// The id must be in 1..=MAX_ID (1-indexed).
    pub fn lookup(&self, id: u8) -> Option<&Extension> {
        if id >= 1 && id <= MAX_ID {
            self.0[id as usize - 1].as_ref().map(|m| &m.ext)
        } else {
            debug!("Lookup RTP extension out of range 1-{}: {}", MAX_ID, id);
            None
        }
    }

    /// Finds the id for an extension (if mapped).
    ///
    /// The returned id will be 1-based.
    pub fn id_of(&self, e: Extension) -> Option<u8> {
        self.0
            .iter()
            .position(|x| x.as_ref().map(|e| &e.ext) == Some(&e))
            .map(|p| p as u8 + 1)
    }

    /// Returns an iterator over the elements of the extension map
    pub fn iter(&self) -> impl Iterator<Item = (u8, &Extension)> + '_ {
        self.0
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.as_ref().map(|e| (i, e)))
            .map(|(i, e)| ((i + 1) as u8, &e.ext))
    }

    /// Returns an iterator over the audio or video elements of the extension map
    pub fn iter_by_media_type(&self, audio: bool) -> impl Iterator<Item = (u8, &Extension)> + '_ {
        self.iter().filter(move |(_id, ext)| {
            if audio {
                ext.is_audio()
            } else {
                ext.is_video()
            }
        })
    }

    /// Returns an iterator over the audio elements of the extension map
    #[allow(unused)]
    pub fn iter_audio(&self) -> impl Iterator<Item = (u8, &Extension)> + '_ {
        self.iter_by_media_type(true)
    }

    /// Returns an iterator over the video elements of the extension map
    #[allow(unused)]
    pub fn iter_video(&self) -> impl Iterator<Item = (u8, &Extension)> + '_ {
        self.iter_by_media_type(false)
    }

    pub(crate) fn cloned_with_type(&self, audio: bool) -> Self {
        let mut x = ExtensionMap::empty();
        for (id, ext) in self.iter_by_media_type(audio) {
            x.set(id, ext.clone());
        }
        x
    }

    // https://tools.ietf.org/html/rfc5285
    pub(crate) fn parse(
        &self,
        mut buf: &[u8],
        form: ExtensionsForm,
        ext_vals: &mut ExtensionValues,
    ) {
        loop {
            if buf.is_empty() {
                return;
            }

            if buf[0] == 0 {
                // padding
                buf = &buf[1..];
                continue;
            }

            let (id, len) = match form {
                ExtensionsForm::OneByte => {
                    let id = buf[0] >> 4;
                    let len = (buf[0] & 0xf) as usize + 1;
                    buf = &buf[1..];

                    if id == 15 {
                        // If the ID value 15 is
                        // encountered, its length field should be ignored, processing of the
                        // entire extension should terminate at that point, and only the
                        // extension elements present prior to the element with ID 15
                        // considered.
                        return;
                    }
                    (id, len)
                }
                ExtensionsForm::TwoByte => {
                    if buf.len() < 2 {
                        trace!("Not enough ext header len: {} < {}", buf.len(), 2);
                        return;
                    }
                    let id = buf[0];
                    let len = buf[1] as usize;
                    buf = &buf[2..];
                    (id, len)
                }
            };

            if buf.len() < len {
                trace!("Not enough type ext len: {} < {}", buf.len(), len);
                return;
            }

            let ext_buf = &buf[..len];
            if let Some(ext) = self.lookup(id) {
                ext.parse_value(ext_buf, ext_vals);
            }

            buf = &buf[len..];
        }
    }

    pub(crate) fn form(&self, ev: &ExtensionValues) -> ExtensionsForm {
        if self
            .iter()
            .any(|(id, ext)| id > MAX_ID_ONE_BYTE_FORM || ext.requires_two_byte_form(ev))
        {
            ExtensionsForm::TwoByte
        } else {
            ExtensionsForm::OneByte
        }
    }

    pub(crate) fn write_to(
        &self,
        ext_buf: &mut [u8],
        ev: &ExtensionValues,
        form: ExtensionsForm,
    ) -> usize {
        let orig_len = ext_buf.len();
        let mut b = ext_buf;

        for (idx, x) in self.0.iter().enumerate() {
            if let Some(v) = x {
                match form {
                    ExtensionsForm::OneByte => {
                        if let Some(n) = v.ext.write_to(&mut b[1..], ev) {
                            assert!(n <= 16);
                            assert!(n > 0);
                            b[0] = (idx as u8 + 1) << 4 | (n as u8 - 1);
                            b = &mut b[1 + n..];
                        }
                    }
                    ExtensionsForm::TwoByte => {
                        if let Some(n) = v.ext.write_to(&mut b[2..], ev) {
                            b[0] = (idx + 1) as u8;
                            b[1] = n as u8;
                            b = &mut b[2 + n..];
                        }
                    }
                };
            }
        }

        orig_len - b.len()
    }

    pub(crate) fn remap(&mut self, remote_exts: &[(u8, &Extension)]) {
        // Match remote numbers and lock down those we see for the first time.
        for (id, ext) in remote_exts {
            self.swap(*id, ext);
        }
    }

    fn swap(&mut self, id: u8, ext: &Extension) {
        if id < 1 || id > MAX_ID {
            return;
        }

        // Mapping goes from 0 to 13.
        let new_index = id as usize - 1;

        let Some(old_index) = self
            .0
            .iter()
            .enumerate()
            .find(|(_, m)| m.as_ref().map(|m| &m.ext) == Some(ext))
            .map(|(i, _)| i)
        else {
            return;
        };

        // Unwrap OK because index is checking just above.
        let old = self.0[old_index].as_mut().unwrap();

        let is_change = new_index != old_index;

        // If either audio or video is locked, we got a previous extmap negotiation.
        if is_change && old.locked {
            warn!(
                "Extmap locked by previous negotiation. Ignore change: {} -> {}",
                old_index, new_index
            );
            return;
        }

        // Locking must be done regardless of whether there was an actual change.
        old.locked = true;

        if !is_change {
            return;
        }

        self.0.swap(old_index, new_index);
    }
}

impl Extension {
    pub(crate) fn write_to(&self, buf: &mut [u8], ev: &ExtensionValues) -> Option<usize> {
        use Extension::*;
        match self {
            AbsoluteSendTime => {
                // 24 bit fixed point 6 bits for seconds, 18 for the decimals.
                // wraps around at 64 seconds.

                // We assume the Instant is absolute.
                let time_abs = ev.abs_send_time?;

                // This should be a 64 second offset from unix epoch.
                let dur = time_abs.to_unix_duration();

                // Rebase to the 6.18 format.
                let time_24 = MediaTime::from(dur)
                    .rebase(Frequency::FIXED_POINT_6_18)
                    .numer() as u32;

                buf[..3].copy_from_slice(&time_24.to_be_bytes()[1..]);
                Some(3)
            }
            AbsoluteCaptureTime => {
                let act = ev.abs_capture_time.as_ref()?;
                let ntp_64 = act.capture_time.as_ntp_64();

                if let Some(offset) = act.clock_offset {
                    if buf.len() < 16 {
                        return None;
                    }
                    buf[..8].copy_from_slice(&ntp_64.to_be_bytes());
                    buf[8..16].copy_from_slice(&offset.to_be_bytes());
                    Some(16)
                } else {
                    if buf.len() < 8 {
                        return None;
                    }
                    buf[..8].copy_from_slice(&ntp_64.to_be_bytes());
                    Some(8)
                }
            }
            AudioLevel => {
                let v1 = ev.audio_level?;
                let v2 = ev.voice_activity?;
                // `v1` is negative, so masking it before negating leaves the
                // sign bit in place: for -37 the expression `-(0x7f & v1)`
                // yields 0xA5, which already has bit 7 set. Taking the
                // magnitude first keeps the sign out of the V bit.
                buf[0] = if v2 { 0x80 } else { 0 } | (v1.unsigned_abs() & 0x7f);
                Some(1)
            }
            TransmissionTimeOffset => {
                let v = ev.tx_time_offs?;
                buf[..4].copy_from_slice(&v.to_be_bytes());
                Some(4)
            }
            VideoOrientation => {
                let v = ev.video_orientation?;
                buf[0] = v as u8;
                Some(1)
            }
            TransportSequenceNumber => {
                let v = ev.transport_cc?;
                buf[..2].copy_from_slice(&v.to_be_bytes());
                Some(2)
            }
            PlayoutDelay => {
                let v1 = ev.play_delay_min?.rebase(Frequency::HUNDREDTHS);
                let v2 = ev.play_delay_max?.rebase(Frequency::HUNDREDTHS);
                let min = (v1.numer() & 0xfff) as u32;
                let max = (v2.numer() & 0xfff) as u32;
                buf[0] = (min >> 4) as u8;
                buf[1] = (min << 4) as u8 | (max >> 8) as u8;
                buf[2] = max as u8;
                Some(3)
            }
            VideoContentType => {
                let v = ev.video_content_type?;
                buf[0] = v;
                Some(1)
            }
            VideoTiming => {
                let v = ev.video_timing?;
                buf[0] = v.flags;
                buf[1..3].copy_from_slice(&v.encode_start.to_be_bytes());
                buf[3..5].copy_from_slice(&v.encode_finish.to_be_bytes());
                buf[5..7].copy_from_slice(&v.packetize_complete.to_be_bytes());
                buf[7..9].copy_from_slice(&v.last_left_pacer.to_be_bytes());
                // Reserved for network
                buf[9..11].copy_from_slice(&0_u16.to_be_bytes());
                buf[11..13].copy_from_slice(&0_u16.to_be_bytes());
                Some(13)
            }
            RtpStreamId => {
                let v = ev.rid?;
                let l = v.len();
                buf[..l].copy_from_slice(v.as_bytes());
                Some(l)
            }
            RepairedRtpStreamId => {
                let v = ev.rid_repair?;
                let l = v.len();
                buf[..l].copy_from_slice(v.as_bytes());
                Some(l)
            }
            RtpMid => {
                let v = ev.mid?;
                let l = v.len();
                buf[..l].copy_from_slice(v.as_bytes());
                Some(l)
            }
            FrameMarking => {
                let v = ev.frame_mark?;
                buf[..4].copy_from_slice(&v.to_be_bytes());
                Some(4)
            }
            ColorSpace => {
                // TODO HDR color space
                None
            }
            UnknownUri(_, serializer) => {
                let n = serializer.write_to(buf, ev);

                if n == 0 { None } else { Some(n) }
            }
        }
    }

    pub(crate) fn parse_value(&self, buf: &[u8], ev: &mut ExtensionValues) -> Option<()> {
        use Extension::*;
        match self {
            // 3
            AbsoluteSendTime => {
                // 24 bit fixed point 6 bits for seconds, 18 for the decimals.
                // wraps around at 64 seconds.
                if buf.len() < 3 {
                    return None;
                }
                let time_24 = u32::from_be_bytes([0, buf[0], buf[1], buf[2]]);

                // Rebase to micros
                let time_micros = MediaTime::from_fixed_point_6_18(time_24 as u64)
                    .rebase(Frequency::MICROS)
                    .numer();

                // This should be the duration in 0-64 seconds from a fixed 64 second offset
                // from UNIX EPOCH. For now, we must save this as offset from _something else_ and
                // fix the correct value when we have the exact Instant::now() to relate it to.
                let time_dur = Duration::from_micros(time_micros);

                let time_tmp = already_happened() + time_dur;
                ev.abs_send_time = Some(time_tmp);
            }
            // 8 or 16
            AbsoluteCaptureTime => {
                if buf.len() < 8 {
                    return None;
                }
                let ntp_64 = u64::from_be_bytes(buf[..8].try_into().unwrap());
                let clock_offset =
                    (buf.len() >= 16).then(|| i64::from_be_bytes(buf[8..16].try_into().unwrap()));
                ev.abs_capture_time = Some(AbsCaptureTime {
                    capture_time: SystemTime::from_ntp_64(ntp_64),
                    clock_offset,
                });
            }
            // 1
            AudioLevel => {
                if buf.is_empty() {
                    return None;
                }
                ev.audio_level = Some(-(0x7f & buf[0] as i8));
                ev.voice_activity = Some(buf[0] & 0x80 > 0);
            }
            // 3
            TransmissionTimeOffset => {
                if buf.len() < 4 {
                    return None;
                }
                ev.tx_time_offs = Some(u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]));
            }
            // 1
            VideoOrientation => {
                if buf.is_empty() {
                    return None;
                }
                ev.video_orientation = Some(super::ext::VideoOrientation::from(buf[0] & 3));
            }
            // 2
            TransportSequenceNumber => {
                if buf.len() < 2 {
                    return None;
                }
                ev.transport_cc = Some(u16::from_be_bytes([buf[0], buf[1]]));
            }
            // 3
            PlayoutDelay => {
                if buf.len() < 3 {
                    return None;
                }
                let min = (buf[0] as u32) << 4 | (buf[1] as u32) >> 4;
                let max = ((buf[1] & 0xf) as u32) << 8 | buf[2] as u32;
                ev.play_delay_min = Some(MediaTime::from_hundredths(min as u64));
                ev.play_delay_max = Some(MediaTime::from_hundredths(max as u64));
            }
            // 1
            VideoContentType => {
                if buf.is_empty() {
                    return None;
                }
                ev.video_content_type = Some(buf[0]);
            }
            // 13
            VideoTiming => {
                if buf.len() < 9 {
                    return None;
                }
                ev.video_timing = Some(self::VideoTiming {
                    flags: buf[0],
                    encode_start: u16::from_be_bytes([buf[1], buf[2]]),
                    encode_finish: u16::from_be_bytes([buf[3], buf[4]]),
                    packetize_complete: u16::from_be_bytes([buf[5], buf[6]]),
                    last_left_pacer: u16::from_be_bytes([buf[7], buf[8]]),
                    //  9 - 10 // reserved for network
                    // 11 - 12 // reserved for network
                });
            }
            RtpStreamId => {
                let s = from_utf8(buf).ok()?;
                ev.rid = Some(s.into());
            }
            RepairedRtpStreamId => {
                let s = from_utf8(buf).ok()?;
                ev.rid_repair = Some(s.into());
            }
            RtpMid => {
                let s = from_utf8(buf).ok()?;
                ev.mid = Some(s.into());
            }
            FrameMarking => {
                if buf.len() < 4 {
                    return None;
                }
                ev.frame_mark = Some(u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]));
            }
            ColorSpace => {
                // TODO HDR color space
            }
            UnknownUri(_, serializer) => {
                let success = serializer.parse_value(buf, ev);
                if !success {
                    return None;
                }
            }
        }

        Some(())
    }
}

/// Values in an RTP header extension.
///
/// This is metadata that is available also without decrypting the SRTP packets.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ExtensionValues {
    /// Audio level is measured in negative decibel. 0 is max and a "normal" value might be -30.
    pub audio_level: Option<i8>,

    /// Indication that there is sound from a voice.
    pub voice_activity: Option<bool>,

    /// Tell a receiver what rotation a video need to replay correctly.
    pub video_orientation: Option<VideoOrientation>,

    // The values below are considered internal until we have a reason to expose them.
    // Generally we want to avoid expose experimental features unless there are strong
    // reasons to do so.
    #[doc(hidden)]
    pub video_content_type: Option<u8>, // 0 = unspecified, 1 = screenshare
    #[doc(hidden)]
    pub tx_time_offs: Option<u32>,
    #[doc(hidden)]
    pub abs_send_time: Option<Instant>,
    #[doc(hidden)]
    pub abs_capture_time: Option<AbsCaptureTime>,
    #[doc(hidden)]
    pub transport_cc: Option<u16>, // (buf[0] << 8) | buf[1];
    #[doc(hidden)]
    // https://webrtc.googlesource.com/src/+/refs/heads/master/docs/native-code/rtp-hdrext/playout-delay
    pub play_delay_min: Option<MediaTime>,
    #[doc(hidden)]
    pub play_delay_max: Option<MediaTime>,
    #[doc(hidden)]
    pub video_timing: Option<VideoTiming>,
    #[doc(hidden)]
    pub rid: Option<Rid>,
    #[doc(hidden)]
    pub rid_repair: Option<Rid>,
    #[doc(hidden)]
    pub mid: Option<Mid>,
    #[doc(hidden)]
    pub frame_mark: Option<u32>,

    /// User values for [`ExtensionSerializer`] to parse into and write from.
    pub user_values: UserExtensionValues,
}
impl ExtensionValues {
    pub(crate) fn update_absolute_send_time(&mut self, now: Instant) {
        let Some(v) = self.abs_send_time else {
            return;
        };

        // This should be 0-64 seconds, or we are not working with a newly parsed value.
        let relative_64_secs = v - already_happened();
        assert!(relative_64_secs <= Duration::from_secs(64));

        let now_since_epoch = now.to_unix_duration();

        let closest_64 = now_since_epoch.saturating_sub(Duration::from_micros(
            now_since_epoch.as_micros() as u64 % 64_000_000,
        ));

        let since_beginning = closest_64.saturating_sub(epoch_to_beginning());

        let mut offset = already_happened() + since_beginning;

        if offset + relative_64_secs > now {
            offset = offset
                .checked_sub(Duration::from_secs(64))
                .unwrap_or(offset);
        }

        self.abs_send_time = Some(offset + relative_64_secs);
    }
}

/// Space for storing user extension values via [`ExtensionSerializer`].
#[derive(Clone, Default)]
pub struct UserExtensionValues {
    map: Option<AnyMap>,
}

// The "AnyMap" idea is borrowed from the http crate but replacing Box for Any.
type AnyMap = HashMap<TypeId, Arc<dyn Any + Send + Sync>, BuildHasherDefault<IdHasher>>;

// No point in hashing the TypeId, since it is already unique.
#[derive(Default)]
struct IdHasher(u64);

impl Hasher for IdHasher {
    fn write(&mut self, _: &[u8]) {
        unreachable!("TypeId calls write_u64");
    }

    #[inline]
    fn write_u64(&mut self, id: u64) {
        self.0 = id;
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

// TODO: I don't see a good way of comparing this. Is there one?
impl PartialEq for UserExtensionValues {
    fn eq(&self, other: &Self) -> bool {
        let (Some(m1), Some(m2)) = (&self.map, &other.map) else {
            return self.map.is_none() == other.map.is_none();
        };

        for k1 in m1.keys() {
            if !m2.contains_key(k1) {
                return false;
            }
        }

        for k2 in m2.keys() {
            if !m1.contains_key(k2) {
                return false;
            }
        }

        true
    }
}

impl Eq for UserExtensionValues {}

impl UserExtensionValues {
    /// Set a user extension value.
    ///
    /// This uses the type of the value as "key", i.e. it can only hold a single
    /// per type. The user should make a wrapper type for the extension they want
    /// to parse/write.
    ///
    /// ```
    /// # use str0m::rtp::ExtensionValues;
    /// let mut exts = ExtensionValues::default();
    ///
    /// #[derive(Debug, PartialEq, Eq)]
    /// struct MySpecialType(u8);
    ///
    /// exts.user_values.set(MySpecialType(42));
    /// ```
    pub fn set<T: Send + Sync + 'static>(&mut self, val: T) {
        // TODO: Consider simplifying to "self.set_arc(Arc::new(val))";
        self.map
            .get_or_insert_with(HashMap::default)
            .insert(TypeId::of::<T>(), Arc::new(val));
    }

    /// Get a user extension value (by type).
    /// ```
    /// # use str0m::rtp::ExtensionValues;
    /// let mut exts = ExtensionValues::default();
    ///
    /// #[derive(Debug, PartialEq, Eq)]
    /// struct MySpecialType(u8);
    ///
    /// exts.user_values.set(MySpecialType(42));
    ///
    /// let v = exts.user_values.get::<MySpecialType>();
    ///
    /// assert_eq!(v, Some(&MySpecialType(42)));
    /// ```
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.map
            .as_ref()
            .and_then(|map| map.get(&TypeId::of::<T>()))
            // unwrap here is OK because TypeId::of::<T> is guaranteed to be unique
            .map(|boxed| (&**boxed as &(dyn Any + 'static)).downcast_ref().unwrap())
    }

    /// Like .set(), but takes an Arc, which can be nice to avoid cloning
    /// large extension values.
    pub fn set_arc<T: Send + Sync + 'static>(&mut self, val: Arc<T>) {
        self.map
            .get_or_insert_with(HashMap::default)
            .insert(TypeId::of::<T>(), val);
    }

    /// Like .get(), but clones and returns the Arc, which can be nice to
    /// avoid cloning large extension values.
    pub fn get_arc<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.map
            .as_ref()?
            .get(&TypeId::of::<T>())?
            .clone()
            .downcast()
            .ok()
    }

    /// Remove a user extension value (by type).
    pub fn remove<T: Send + Sync + 'static>(&mut self) {
        if let Some(map) = &mut self.map {
            map.remove(&TypeId::of::<T>());
        }
    }
}

impl UnwindSafe for UserExtensionValues {}

impl fmt::Debug for ExtensionValues {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ExtensionValues {{")?;

        if let Some(t) = self.mid {
            write!(f, " mid: {t}")?;
        }
        if let Some(t) = self.rid {
            write!(f, " rid: {t}")?;
        }
        if let Some(t) = self.rid_repair {
            write!(f, " rid_repair: {t}")?;
        }
        if let Some(t) = self.abs_send_time {
            write!(f, " abs_send_time: {:?}", t)?;
        }
        if let Some(t) = &self.abs_capture_time {
            write!(f, " abs_capture_time: {:?}", t)?;
        }
        if let Some(t) = self.voice_activity {
            write!(f, " voice_activity: {t}")?;
        }
        if let Some(t) = self.audio_level {
            write!(f, " audio_level: {t}")?;
        }
        if let Some(t) = self.tx_time_offs {
            write!(f, " tx_time_offs: {t}")?;
        }
        if let Some(t) = self.video_orientation {
            write!(f, " video_orientation: {t:?}")?;
        }
        if let Some(t) = self.transport_cc {
            write!(f, " transport_cc: {t}")?;
        }
        if let Some(t) = self.play_delay_min {
            write!(f, " play_delay_min: {}", t.as_seconds())?;
        }
        if let Some(t) = self.play_delay_max {
            write!(f, " play_delay_max: {}", t.as_seconds())?;
        }
        if let Some(t) = self.video_content_type {
            write!(f, " video_content_type: {t}")?;
        }
        if let Some(t) = &self.video_timing {
            write!(f, " video_timing: {t:?}")?;
        }
        if let Some(t) = &self.frame_mark {
            write!(f, " frame_mark: {t}")?;
        }

        write!(f, " }}")?;
        Ok(())
    }
}

/// Absolute capture time from the `abs-capture-time` RTP header extension.
///
/// Contains the NTP wall-clock timestamp of original frame capture, and
/// optionally the sender's estimate of the offset between its own clock
/// and the capture system's clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbsCaptureTime {
    /// NTP wall-clock timestamp of original frame capture.
    pub capture_time: SystemTime,
    /// Sender's estimate of offset between its own NTP clock and the
    /// capture system's NTP clock. Signed NTP fixed-point (Q32.32):
    /// upper 32 bits whole seconds, lower 32 bits fractional.
    pub clock_offset: Option<i64>,
}

/// 2^32 as f64, for NTP Q32.32 fixed-point conversion.
const NTP_F32: f64 = 4_294_967_296.0;

impl AbsCaptureTime {
    /// Clock offset in seconds, if present.
    ///
    /// Per the spec: `Capture NTP Clock = Sender NTP Clock + offset`.
    pub fn clock_offset_secs(&self) -> Option<f64> {
        self.clock_offset.map(|v| v as f64 / NTP_F32)
    }

    /// Set the clock offset in seconds.
    pub fn set_clock_offset(&mut self, secs: f64) {
        self.clock_offset = Some((secs * NTP_F32) as i64);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoTiming {
    // 0x01 = extension is set due to timer.
    // 0x02 - extension is set because the frame is larger than usual.
    pub flags: u8,
    pub encode_start: u16,
    pub encode_finish: u16,
    pub packetize_complete: u16,
    pub last_left_pacer: u16,
}

impl fmt::Display for Extension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use Extension::*;
        write!(
            f,
            "{}",
            match self {
                AbsoluteSendTime => "abs-send-time",
                AbsoluteCaptureTime => "abs-capture-time",
                AudioLevel => "ssrc-audio-level",
                TransmissionTimeOffset => "toffset",
                VideoOrientation => "video-orientation",
                TransportSequenceNumber => "transport-wide-cc",
                PlayoutDelay => "playout-delay",
                VideoContentType => "video-content-type",
                VideoTiming => "video-timing",
                RtpStreamId => "rtp-stream-id",
                RepairedRtpStreamId => "repaired-rtp-stream-id",
                RtpMid => "mid",
                FrameMarking => "frame-marking07",
                ColorSpace => "color-space",
                UnknownUri(uri, _) => uri,
            }
        )
    }
}

impl fmt::Debug for ExtensionMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Extensions(")?;
        let joined = self
            .0
            .iter()
            .enumerate()
            .filter_map(|(i, v)| v.as_ref().map(|v| (i + 1, v)))
            .map(|(i, v)| format!("{}={}", i, v.ext))
            .collect::<Vec<_>>()
            .join(", ");
        write!(f, "{joined}")?;
        write!(f, ")")?;
        Ok(())
    }
}

/// How the video is rotated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoOrientation {
    /// Not rotated.
    Deg0 = 0,
    /// 90 degress clockwise.
    Deg90 = 3,
    /// Upside down.
    Deg180 = 2,
    /// 90 degrees counter clockwise.
    Deg270 = 1,
}

#[cfg(feature = "drv")]
crate::drv_identity_copy!(VideoOrientation);

impl From<u8> for VideoOrientation {
    fn from(value: u8) -> Self {
        match value {
            1 => Self::Deg270,
            2 => Self::Deg180,
            3 => Self::Deg90,
            _ => Self::Deg0,
        }
    }
}

impl PartialEq for Extension {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Extension::AbsoluteSendTime, Extension::AbsoluteSendTime) => true,
            (Extension::AbsoluteCaptureTime, Extension::AbsoluteCaptureTime) => true,
            (Extension::AudioLevel, Extension::AudioLevel) => true,
            (Extension::TransmissionTimeOffset, Extension::TransmissionTimeOffset) => true,
            (Extension::VideoOrientation, Extension::VideoOrientation) => true,
            (Extension::TransportSequenceNumber, Extension::TransportSequenceNumber) => true,
            (Extension::PlayoutDelay, Extension::PlayoutDelay) => true,
            (Extension::VideoContentType, Extension::VideoContentType) => true,
            (Extension::VideoTiming, Extension::VideoTiming) => true,
            (Extension::RtpStreamId, Extension::RtpStreamId) => true,
            (Extension::RepairedRtpStreamId, Extension::RepairedRtpStreamId) => true,
            (Extension::RtpMid, Extension::RtpMid) => true,
            (Extension::FrameMarking, Extension::FrameMarking) => true,
            (Extension::ColorSpace, Extension::ColorSpace) => true,
            (Extension::UnknownUri(uri1, _), Extension::UnknownUri(uri2, _)) => uri1 == uri2,
            _ => false,
        }
    }
}

impl Eq for Extension {}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn abs_send_time() {
        let now = Instant::now() + Duration::from_secs(1000);

        let mut exts = ExtensionMap::empty();
        exts.set(4, Extension::AbsoluteSendTime);
        let ev = ExtensionValues {
            abs_send_time: Some(now),
            ..Default::default()
        };

        let mut buf = vec![0_u8; 8];
        exts.write_to(&mut buf[..], &ev, ExtensionsForm::OneByte);

        let mut ev2 = ExtensionValues::default();
        exts.parse(&buf, ExtensionsForm::OneByte, &mut ev2);

        // Let's pretend a 50 millisecond network latency.
        ev2.update_absolute_send_time(now + Duration::from_millis(50));

        let now2 = ev2.abs_send_time.unwrap();

        let abs = if now > now2 { now - now2 } else { now2 - now };

        assert!(abs < Duration::from_millis(1));
    }

    #[test]
    fn abs_send_time_two_byte_form() {
        let now = Instant::now() + Duration::from_secs(1000);

        let mut exts = ExtensionMap::empty();
        exts.set(16, Extension::AbsoluteSendTime);
        let ev = ExtensionValues {
            abs_send_time: Some(now),
            ..Default::default()
        };

        let mut buf = vec![0_u8; 8];
        assert_eq!(ExtensionsForm::TwoByte, exts.form(&ev));
        exts.write_to(&mut buf[..], &ev, ExtensionsForm::TwoByte);

        let mut ev2 = ExtensionValues::default();
        exts.parse(&buf, ExtensionsForm::TwoByte, &mut ev2);

        // Let's pretend a 50 millisecond network latency.
        ev2.update_absolute_send_time(now + Duration::from_millis(50));

        let now2 = ev2.abs_send_time.unwrap();

        let abs = if now > now2 { now - now2 } else { now2 - now };

        assert!(abs < Duration::from_millis(1));
    }

    #[test]
    fn abs_capture_time_short_form() {
        let now = SystemTime::now();

        let mut exts = ExtensionMap::empty();
        exts.set(5, Extension::AbsoluteCaptureTime);
        let ev = ExtensionValues {
            abs_capture_time: Some(AbsCaptureTime {
                capture_time: now,
                clock_offset: None,
            }),
            ..Default::default()
        };

        let mut buf = vec![0_u8; 16];
        exts.write_to(&mut buf[..], &ev, ExtensionsForm::OneByte);

        let mut ev2 = ExtensionValues::default();
        exts.parse(&buf, ExtensionsForm::OneByte, &mut ev2);

        let act = ev2.abs_capture_time.unwrap();

        // Allow for small rounding errors in NTP conversion
        let abs = if now > act.capture_time {
            now.duration_since(act.capture_time)
        } else {
            act.capture_time.duration_since(now)
        }
        .unwrap();

        assert!(abs < Duration::from_millis(1));
        assert_eq!(act.clock_offset, None);
    }

    #[test]
    fn abs_capture_time_extended_form() {
        let now = SystemTime::now();
        let clock_offset: i64 = 123456789;

        let mut exts = ExtensionMap::empty();
        exts.set(5, Extension::AbsoluteCaptureTime);
        let ev = ExtensionValues {
            abs_capture_time: Some(AbsCaptureTime {
                capture_time: now,
                clock_offset: Some(clock_offset),
            }),
            ..Default::default()
        };

        let mut buf = vec![0_u8; 24];
        exts.write_to(&mut buf[..], &ev, ExtensionsForm::OneByte);

        let mut ev2 = ExtensionValues::default();
        exts.parse(&buf, ExtensionsForm::OneByte, &mut ev2);

        let act = ev2.abs_capture_time.unwrap();

        // Allow for small rounding errors in NTP conversion
        let abs = if now > act.capture_time {
            now.duration_since(act.capture_time)
        } else {
            act.capture_time.duration_since(now)
        }
        .unwrap();

        assert!(abs < Duration::from_millis(1));
        assert_eq!(act.clock_offset, Some(clock_offset));
    }

    #[test]
    fn abs_capture_time_two_byte_form() {
        let now = SystemTime::now();
        let clock_offset: i64 = -987654321;

        let mut exts = ExtensionMap::empty();
        exts.set(16, Extension::AbsoluteCaptureTime);
        let ev = ExtensionValues {
            abs_capture_time: Some(AbsCaptureTime {
                capture_time: now,
                clock_offset: Some(clock_offset),
            }),
            ..Default::default()
        };

        // Should require two-byte form due to extension ID > 14
        assert_eq!(ExtensionsForm::TwoByte, exts.form(&ev));

        let mut buf = vec![0_u8; 24];
        exts.write_to(&mut buf[..], &ev, ExtensionsForm::TwoByte);

        let mut ev2 = ExtensionValues::default();
        exts.parse(&buf, ExtensionsForm::TwoByte, &mut ev2);

        let act = ev2.abs_capture_time.unwrap();

        // Allow for small rounding errors in NTP conversion
        let abs = if now > act.capture_time {
            now.duration_since(act.capture_time)
        } else {
            act.capture_time.duration_since(now)
        }
        .unwrap();

        assert!(abs < Duration::from_millis(1));
        assert_eq!(act.clock_offset, Some(clock_offset));
    }

    #[test]
    fn playout_delay() {
        let mut exts = ExtensionMap::empty();
        exts.set(2, Extension::PlayoutDelay);
        let ev = ExtensionValues {
            play_delay_min: Some(MediaTime::from_hundredths(100)),
            play_delay_max: Some(MediaTime::from_hundredths(200)),
            ..Default::default()
        };

        let mut buf = vec![0_u8; 8];
        exts.write_to(&mut buf[..], &ev, ExtensionsForm::OneByte);

        let mut ev2 = ExtensionValues::default();
        exts.parse(&buf, ExtensionsForm::OneByte, &mut ev2);

        assert_eq!(ev.play_delay_min, ev2.play_delay_min);
        assert_eq!(ev.play_delay_max, ev2.play_delay_max);
    }

    #[test]
    fn remap_exts_audio() {
        use Extension::*;

        let mut e1 = ExtensionMap::standard();
        let mut e2 = ExtensionMap::empty();
        e2.set(14, TransportSequenceNumber);

        println!("{:?}", e1.iter_video().collect::<Vec<_>>());

        e1.remap(&e2.iter_audio().collect::<Vec<_>>());

        // e1 should have adjusted the TransportSequenceNumber for audio
        assert_eq!(
            e1.iter_audio().collect::<Vec<_>>(),
            vec![
                (1, &AudioLevel),
                (2, &AbsoluteSendTime),
                (4, &RtpMid),
                (10, &RtpStreamId),
                (11, &RepairedRtpStreamId),
                (14, &TransportSequenceNumber)
            ]
        );

        // e1 should have adjusted the TransportSequenceNumber for vudeo
        assert_eq!(
            e1.iter_video().collect::<Vec<_>>(),
            vec![
                (2, &AbsoluteSendTime),
                (4, &RtpMid),
                (10, &RtpStreamId),
                (11, &RepairedRtpStreamId),
                (13, &VideoOrientation),
                (14, &TransportSequenceNumber),
            ]
        );
    }

    #[test]
    fn remap_exts_video() {
        use Extension::*;

        let mut e1 = ExtensionMap::empty();
        e1.set(3, TransportSequenceNumber);
        e1.set(4, VideoOrientation);
        e1.set(5, VideoContentType);
        let mut e2 = ExtensionMap::empty();
        e2.set(14, TransportSequenceNumber);
        e2.set(12, VideoOrientation);

        e1.remap(&e2.iter_video().collect::<Vec<_>>());

        // e1 should have adjusted to e2.
        assert_eq!(
            e1.iter_video().collect::<Vec<_>>(),
            vec![
                (5, &VideoContentType),
                (12, &VideoOrientation),
                (14, &TransportSequenceNumber)
            ]
        );
    }

    #[test]
    fn remap_exts_swaparoo() {
        use Extension::*;

        let mut e1 = ExtensionMap::empty();
        e1.set(12, TransportSequenceNumber);
        e1.set(14, VideoOrientation);
        let mut e2 = ExtensionMap::empty();
        e2.set(14, TransportSequenceNumber);
        e2.set(12, VideoOrientation);

        e1.remap(&e2.iter_video().collect::<Vec<_>>());

        // just make sure the logic isn't wrong for 12-14 -> 14-12
        assert_eq!(
            e1.iter_video().collect::<Vec<_>>(),
            vec![(12, &VideoOrientation), (14, &TransportSequenceNumber)]
        );
    }

    #[test]
    fn remap_exts_illegal() {
        use Extension::*;

        let mut e1 = ExtensionMap::empty();
        e1.set(12, TransportSequenceNumber);
        e1.set(14, VideoOrientation);

        let mut e2 = ExtensionMap::empty();
        e2.set(14, TransportSequenceNumber);
        e2.set(12, VideoOrientation);

        let mut e3 = ExtensionMap::empty();
        // Illegal change of already negotiated/locked number
        e3.set(1, TransportSequenceNumber);
        e3.set(12, AudioLevel); // change of type for existing.

        // First apply e2
        e1.remap(&e2.iter_video().collect::<Vec<_>>());

        println!("{:#?}", e1.0);
        assert_eq!(
            e1.iter_video().collect::<Vec<_>>(),
            vec![(12, &VideoOrientation), (14, &TransportSequenceNumber)]
        );

        // Now attempt e3
        e1.remap(&e3.iter_audio().collect::<Vec<_>>());

        println!("{:#?}", e1.0);
        // At this point we should have not allowed the change, but remain as it was in first apply.
        assert_eq!(
            e1.iter_video().collect::<Vec<_>>(),
            vec![(12, &VideoOrientation), (14, &TransportSequenceNumber)]
        );
    }

    #[test]
    fn audio_level_round_trip() {
        // The V bit (voice activity) must survive serialization for every
        // level on the scale, not just when it is set.
        let mut exts = ExtensionMap::empty();
        exts.set(1, Extension::AudioLevel);

        for level in -127..=0 {
            for voice_activity in [false, true] {
                let ev = ExtensionValues {
                    audio_level: Some(level),
                    voice_activity: Some(voice_activity),
                    ..Default::default()
                };

                let mut buf = vec![0_u8; 8];
                exts.write_to(&mut buf[..], &ev, ExtensionsForm::OneByte);

                let mut ev2 = ExtensionValues::default();
                exts.parse(&buf, ExtensionsForm::OneByte, &mut ev2);

                assert_eq!(ev2.audio_level, Some(level));
                assert_eq!(ev2.voice_activity, Some(voice_activity));
            }
        }
    }
}
