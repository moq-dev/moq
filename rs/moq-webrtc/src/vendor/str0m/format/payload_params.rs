use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ops::RangeInclusive;

use crate::packet::H264ProfileLevel;
use crate::packet::H265ProfileTierLevel;
use crate::packet::H266ProfileTierLevel;
use crate::rtp_::Pt;

use super::codec::{Codec, CodecSpec};
use super::format_params::FormatParams;

/// Preferred ranges for dynamic payload type allocation.
pub(crate) const PREFERED_RANGES: &[RangeInclusive<usize>] = &[
    // Payload identifiers 96–127 are used for payloads defined dynamically during a session.
    96..=127,
    // "unassigned" ranged. note that RTCP packet type 207 (XR, Extended Reports) would be
    // indistinguishable from RTP payload types 79 with the marker bit set
    80..=95,
    77..=78,
    // reserved because RTCP packet types 200–204 would otherwise be indistinguishable
    // from RTP payload types 72–76
    // 72..77,
    // lol range.
    35..=71,
];

pub(crate) trait Claimed {
    fn assert_claim_once(&mut self, pt: Pt);
    fn is_claimed(&self, pt: Pt) -> bool;
    fn find_unclaimed(
        &self,
        ranges: &[RangeInclusive<usize>],
        unlocked: &HashSet<Pt>,
    ) -> Option<Pt>;
}

impl Claimed for [bool; 128] {
    fn assert_claim_once(&mut self, pt: Pt) {
        let idx = *pt as usize;
        assert!(!self[idx], "Pt locked multiple times: {}", pt);
        self[idx] = true;
    }
    fn is_claimed(&self, pt: Pt) -> bool {
        let idx = *pt as usize;
        self[idx]
    }
    fn find_unclaimed(
        &self,
        ranges: &[RangeInclusive<usize>],
        unlocked: &HashSet<Pt>,
    ) -> Option<Pt> {
        for range in ranges {
            for i in range.clone() {
                if !self[i] && !unlocked.contains(&(i as u8).into()) {
                    let pt: Pt = (i as u8).into();
                    return Some(pt);
                }
            }
        }

        // Failed to find unclaimed PT.
        None
    }
}

/// Group of parameters for a payload type (PT).
///
/// In the SDP a payload type has a number of associated parameters. See example below:
///
/// ```text
/// a=rtpmap:96 H264/90000
/// a=rtcp-fb:96 goog-remb
/// a=rtcp-fb:96 transport-cc
/// a=rtcp-fb:96 ccm fir
/// a=rtcp-fb:96 nack
/// a=rtcp-fb:96 nack pli
/// a=fmtp:96 level-asymmetry-allowed=1;packetization-mode=0;profile-level-id=42001f
/// ```
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct PayloadParams {
    /// The payload type that groups these parameters.
    pub(crate) pt: Pt,

    /// Whether these parameters are repairing some other set of parameters.
    /// This is used to, via PT, separate RTX resend streams from the main stream.
    pub(crate) resend: Option<Pt>,

    /// The codec with settings for this group of parameters.
    pub(crate) spec: CodecSpec,

    /// Whether the payload use the TWCC feedback mechanic.
    pub(crate) fb_transport_cc: bool,

    /// Whether the payload uses NACK to request resends.
    pub(crate) fb_nack: bool,

    /// Whether the payload uses the PLI (Picture Loss Indication) mechanic.
    pub(crate) fb_pli: bool,

    /// Whether the payload uses the FIR (Full Intra Request) mechanic.
    pub(crate) fb_fir: bool,

    /// Whether the payload uses the REMB (Receiver Estimated Maximum Bitrate) mechanic.
    pub(crate) fb_remb: bool,

    /// The RFC 2198 RED payload type that wraps this primary codec, if negotiated.
    /// Like `resend`, this links a secondary PT to this primary via SDP.
    pub(crate) red: Option<Pt>,

    /// Whether the payload is locked by negotiation or can still be debated.
    ///
    /// If we make an OFFER or ANSWER and the direction is sendrecv/recvonly, the parameters are locked
    /// can't be further changed. If we make an OFFER for a sendonly, the parameters are only proposed
    /// and don't lock.
    pub(crate) locked: bool,
}

// we don't want to compare "locked"
impl PartialEq for PayloadParams {
    fn eq(&self, other: &Self) -> bool {
        self.pt == other.pt
            && self.resend == other.resend
            && self.spec == other.spec
            && self.fb_transport_cc == other.fb_transport_cc
            && self.fb_nack == other.fb_nack
            && self.fb_pli == other.fb_pli
            && self.fb_fir == other.fb_fir
            && self.fb_remb == other.fb_remb
            && self.red == other.red
    }
}

impl Eq for PayloadParams {}

impl PayloadParams {
    /// Maximum match score.
    ///
    /// This is returned for an exact `CodecSpec` match in `match_score()`. It is also used as the
    /// starting point for codec-specific scoring in this file:
    /// - Opus/H.264/H.265 decrement from a perfect match.
    /// - VP9/AV1 return the max score when their relevant fmtp parameters match.
    /// - VP8 currently only matches via the exact `CodecSpec` equality fast-path (no fuzzy scorer).
    const EXACT_MATCH_SCORE: usize = 100;

    /// Score used for H.265 profile-only compatibility matches.
    ///
    /// Some endpoints (notably browsers) may signal H.265 using only `profile-id` without
    /// tier/level information; when we can only validate profile equality we return this lower
    /// score so full ProfileTierLevel matches win when available.
    const PROFILE_ONLY_MATCH_SCORE: usize = 90;

    /// Creates new payload params.
    ///
    /// * `pt` is the payload type RTP mapping in the session.
    /// * `resend` is the payload type used for (RTX) resend channel.
    /// * `spec` configures details about the codec.
    pub fn new(pt: Pt, resend: Option<Pt>, spec: CodecSpec) -> Self {
        let is_video = spec.codec.is_video();

        PayloadParams {
            pt,
            resend,

            spec,

            // Both audio and video use TWCC
            fb_transport_cc: true,

            // Only true for video.
            fb_fir: is_video,
            fb_nack: is_video,
            fb_pli: is_video,
            fb_remb: is_video,

            red: None,
            locked: false,
        }
    }

    /// Creates minimal payload params for SSRC 0 BWE probes.
    ///
    /// These probes don't carry real media, only padding for bandwidth estimation.
    /// Uses 90kHz clock rate (video rate) and enables only transport_cc feedback.
    pub(crate) fn new_probe(pt: Pt) -> Self {
        use super::codec::Codec;
        use super::format_params::FormatParams;
        use crate::rtp_::Frequency;

        PayloadParams {
            pt,
            resend: None,
            spec: CodecSpec {
                codec: Codec::Null,
                clock_rate: Frequency::NINETY_KHZ,
                channels: None,
                format: FormatParams::default(),
            },
            fb_transport_cc: true,
            fb_nack: false,
            fb_pli: false,
            fb_fir: false,
            fb_remb: false,
            red: None,
            locked: false,
        }
    }

    /// The payload type that groups these parameters.
    pub fn pt(&self) -> Pt {
        self.pt
    }

    /// Whether these parameters are repairing some other set of parameters.
    /// This is used to, via PT, separate RTX resend streams from the main stream.
    pub fn resend(&self) -> Option<Pt> {
        self.resend
    }

    /// The codec with settings for this group of parameters.
    pub fn spec(&self) -> CodecSpec {
        self.spec
    }

    /// Sets whether the payload use the TWCC feedback mechanic.
    pub fn set_fb_transport_cc(&mut self, fb_transport_cc: bool) {
        self.fb_transport_cc = fb_transport_cc
    }

    /// Whether the payload use the TWCC feedback mechanic.
    pub fn fb_transport_cc(&self) -> bool {
        self.fb_transport_cc
    }

    /// Sets whether the payload uses NACK to request resends.
    pub fn set_fb_nack(&mut self, fb_nack: bool) {
        self.fb_nack = fb_nack
    }

    /// Whether the payload uses NACK to request resends.
    pub fn fb_nack(&self) -> bool {
        self.fb_nack
    }

    /// Set whether the payload uses the PLI (Picture Loss Indication) mechanic.
    pub fn set_fb_pli(&mut self, fb_pli: bool) {
        self.fb_pli = fb_pli
    }

    /// Whether the payload uses the PLI (Picture Loss Indication) mechanic.
    pub fn fb_pli(&self) -> bool {
        self.fb_pli
    }

    /// Set whether the payload uses the FIR (Full Intra Request) mechanic.
    pub fn set_fb_fir(&mut self, fb_fir: bool) {
        self.fb_fir = fb_fir
    }

    /// Whether the payload uses the FIR (Full Intra Request) mechanic.
    pub fn fb_fir(&self) -> bool {
        self.fb_fir
    }

    /// Set whether the payload uses the REMB (Receiver Estimated Maximum Bitrate) mechanic.
    pub fn set_fb_remb(&mut self, fb_remb: bool) {
        self.fb_remb = fb_remb
    }

    /// Whether the payload uses the REMB (Receiver Estimated Maximum Bitrate) mechanic.
    pub fn fb_remb(&self) -> bool {
        self.fb_remb
    }

    /// The RFC 2198 RED payload type wrapping this primary codec, if negotiated.
    pub fn red(&self) -> Option<Pt> {
        self.red
    }

    pub(crate) fn match_score(&self, o: &PayloadParams) -> Option<usize> {
        // we don't want to compare PT
        let c0 = self.spec;
        let c1 = o.spec;

        if c0 == c1 && c0.codec != Codec::AmrWb {
            // Exact match
            return Some(Self::EXACT_MATCH_SCORE);
        }

        // Attempt fuzzy matching, since we don't have an exact match
        if c0.codec != c1.codec || (c0.codec == Codec::Unknown && c1.codec == Codec::Unknown) {
            // Codecs must match
            return None;
        }

        if c0.clock_rate != c1.clock_rate {
            // Clock rates must match
            return None;
        }

        // Telephone events match by clock rate, regardless of channel count.
        if c0.codec.is_tele() {
            return Some(Self::EXACT_MATCH_SCORE);
        }

        // AMR-WB defaults an omitted channel count to mono, so it must match
        // before the generic channel comparison below.
        if c0.codec == Codec::AmrWb {
            return Self::match_amr_wb_score(c0, c1);
        }

        if c0.channels != c1.channels {
            // Channels must match
            return None;
        }

        if c0.codec.is_audio() && c0.codec == Codec::Opus {
            return Some(Self::match_opus_score(c0, c1));
        }

        if c0.codec == Codec::H264 {
            return Self::match_h264_score(c0, c1);
        }

        if c0.codec == Codec::H265 {
            return Self::match_h265_score(c0, c1);
        }

        if c0.codec == Codec::H266 {
            return Self::match_h266_score(c0, c1);
        }

        if c0.codec == Codec::Vp9 {
            return Self::match_vp9_score(c0, c1);
        }

        if c0.codec == Codec::Av1 {
            return Self::match_av1_score(c0, c1);
        }

        // TODO: Fuzzy matching for any other audio codecs
        // TODO: Fuzzy matching for video

        None
    }

    fn match_opus_score(c0: CodecSpec, c1: CodecSpec) -> usize {
        let mut score: usize = Self::EXACT_MATCH_SCORE;

        // If neither value is specified both sides should assume the default value, 3.
        let either_p_time_specified =
            c0.format.min_p_time.is_some() || c1.format.min_p_time.is_some();
        if either_p_time_specified && c0.format.min_p_time != c1.format.min_p_time {
            score = score.saturating_sub(1);
        }

        // If neither value is specified both sides should assume FEC is not used as this is
        // the default.
        let either_fec_specified =
            c0.format.use_inband_fec.is_some() || c1.format.use_inband_fec.is_some();

        if either_fec_specified && c0.format.use_inband_fec != c1.format.use_inband_fec {
            score = score.saturating_sub(2);
        }

        // If neither value is specified both sides should assume DTX is not used as this is
        // the default.
        let either_dtx_specified = c0.format.use_dtx.is_some() || c1.format.use_dtx.is_some();
        if either_dtx_specified && c0.format.use_dtx != c1.format.use_dtx {
            score = score.saturating_sub(4);
        }

        // If neither value is specified both sides should assume mono for stereo
        // and the sprop-stereo hint defaults to false.
        let c0_stereo = c0.format.stereo.unwrap_or(false);
        let c1_stereo = c1.format.stereo.unwrap_or(false);
        if c0_stereo != c1_stereo {
            score = score.saturating_sub(8);
        }

        let c0_sprop_stereo = c0.format.sprop_stereo.unwrap_or(false);
        let c1_sprop_stereo = c1.format.sprop_stereo.unwrap_or(false);
        if c0_sprop_stereo != c1_sprop_stereo {
            score = score.saturating_sub(8);
        }

        score
    }

    fn match_vp9_score(c0: CodecSpec, c1: CodecSpec) -> Option<usize> {
        // Default profile_id is 0. https://datatracker.ietf.org/doc/html/draft-ietf-payload-vp9-16#section-6
        let c0_profile_id = c0.format.profile_id.unwrap_or(0);
        let c1_profile_id = c1.format.profile_id.unwrap_or(0);

        if c0_profile_id != c1_profile_id {
            return None;
        }

        Some(Self::EXACT_MATCH_SCORE)
    }

    fn match_av1_score(c0: CodecSpec, c1: CodecSpec) -> Option<usize> {
        // TODO: consider media direction for a proper less or equal matching
        // The AV1 stream sent by either the offerer or the answerer MUST be
        // encoded with a profile, level and tier, lesser or equal to the values
        // of the level-idx, profile and tier declared in the SDP by the receiving
        // agent.
        // https://aomediacodec.github.io/av1-rtp-spec/#723-usage-with-the-sdp-offeranswer-model

        // Default values: profile = 0, level-idx = 5, tier = 0
        // https://aomediacodec.github.io/av1-rtp-spec/#72-sdp-parameters
        let c0_profile = c0.format.profile.unwrap_or(0);
        let c1_profile = c1.format.profile.unwrap_or(0);
        if c0_profile != c1_profile {
            return None;
        }

        let c0_level_idx = c0.format.level_idx.unwrap_or(5);
        let c1_level_idx = c1.format.level_idx.unwrap_or(5);
        if c0_level_idx != c1_level_idx {
            return None;
        }

        let c0_tier = c0.format.tier.unwrap_or(0);
        let c1_tier = c1.format.tier.unwrap_or(0);
        if c0_tier != c1_tier {
            return None;
        }

        Some(Self::EXACT_MATCH_SCORE)
    }

    fn match_amr_wb_score(c0: CodecSpec, c1: CodecSpec) -> Option<usize> {
        // Only mono is implemented, and octet-aligned and bandwidth-efficient
        // payloads are not interchangeable.
        if c0.channels.unwrap_or(1) != 1 || c1.channels.unwrap_or(1) != 1 {
            return None;
        }
        if c0.format.octet_align.unwrap_or(false) != c1.format.octet_align.unwrap_or(false) {
            return None;
        }

        // A mode-set binds both peers (RFC 4867 §8.3.1), so prefer unrestricted
        // payload types when the remote offers both.
        match (c0.format.mode_set, c1.format.mode_set) {
            (None, None) => Some(Self::EXACT_MATCH_SCORE),
            (Some(a), Some(b)) if a != b => None,
            _ => Some(Self::EXACT_MATCH_SCORE - 1),
        }
    }

    pub(crate) fn match_h264_score(c0: CodecSpec, c1: CodecSpec) -> Option<usize> {
        // Default packetization mode is 0. https://www.rfc-editor.org/rfc/rfc6184#section-6.2
        let c0_packetization_mode = c0.format.packetization_mode.unwrap_or(0);
        let c1_packetization_mode = c1.format.packetization_mode.unwrap_or(0);

        if c0_packetization_mode != c1_packetization_mode {
            return None;
        }

        let c0_profile_level = c0
            .format
            .profile_level_id
            .map(|l| l.try_into().ok())
            .unwrap_or(Some(H264ProfileLevel::FALLBACK))?;
        let c1_profile_level = c1
            .format
            .profile_level_id
            .map(|l| l.try_into().ok())
            .unwrap_or(Some(H264ProfileLevel::FALLBACK))?;

        // RFC 6184 Section 8.2.2: Profiles must match exactly.
        if c0_profile_level.profile() != c1_profile_level.profile() {
            return None;
        }

        // RFC 6184 Section 8.2.2: the level is a negotiable (downgradable)
        // parameter, so a level mismatch in either direction is allowed and
        // only penalizes the match score rather than rejecting the payload.
        //
        // NOTE: this only affects *matching*. Unlike H.265/H.266, `update_param`
        // does not narrow the answered H.264 level to `min(local, remote)`, so
        // when `level-asymmetry-allowed` is absent we may answer with a level
        // higher than the offer. See the note in `update_param`.
        let level_difference: usize = c0_profile_level
            .level()
            .ordinal()
            .abs_diff(c1_profile_level.level().ordinal());

        Some(Self::EXACT_MATCH_SCORE.saturating_sub(level_difference))
    }

    /// Match H.265 codec specifications and return compatibility score.
    ///
    /// # Matching Rules — Full PTL (both sides have profile+tier+level)
    ///
    /// - **Profiles** must match exactly (no cross-profile compatibility)
    /// - **Tiers** must match exactly (Main vs High tier are distinct)
    /// - **Levels** penalize score by `|c0_level - c1_level|` but never reject.
    ///   Both H.264 and H.265 tolerate level mismatches in either direction at
    ///   match time. For H.265, `update_param` additionally narrows the
    ///   negotiated level to `min(local, remote)` afterward; H.264 does not.
    ///
    /// # Matching Rules — Profile-only (at least one side lacks tier+level)
    ///
    /// - **Profiles** must match; missing profile info falls back to
    ///   `FALLBACK.profile()` (not rejected).
    /// - **Tiers and levels** are not checked (insufficient information).
    /// - Returns a lower score (90) so full-PTL matches are preferred.
    ///
    /// # Returns
    ///
    /// - `Some(100)` for exact full-PTL match
    /// - `Some(100 - level_gap)` for profile/tier match with level difference
    /// - `Some(90)` for profile-only match (when tier/level unavailable)
    /// - `None` for profile or tier mismatch
    pub(crate) fn match_h265_score(c0: CodecSpec, c1: CodecSpec) -> Option<usize> {
        match (
            c0.format.h265_profile_tier_level,
            c1.format.h265_profile_tier_level,
        ) {
            (Some(c0_ptl), Some(c1_ptl)) => {
                // Both have full PTL - strict matching

                // Profiles must match exactly.
                // RFC 7798 §7.2.2: profile-id is a configuration parameter
                // that MUST be used symmetrically in offer/answer.
                // https://www.rfc-editor.org/rfc/rfc7798#section-7.2.2
                if c0_ptl.profile() != c1_ptl.profile() {
                    return None;
                }

                // Tiers must match exactly.
                // RFC 7798 §7.2.2: tier-flag is a configuration parameter
                // that MUST be used symmetrically in offer/answer.
                // https://www.rfc-editor.org/rfc/rfc7798#section-7.2.2
                if c0_ptl.tier() != c1_ptl.tier() {
                    return None;
                }

                // Level difference penalizes score but never causes rejection.
                // Actual level narrowing to min(local, remote) is done in update_param.
                let level_difference: usize =
                    c0_ptl.level().ordinal().abs_diff(c1_ptl.level().ordinal());

                // Pure scoring without artificial floor - let caller decide policy
                Some(Self::EXACT_MATCH_SCORE.saturating_sub(level_difference))
            }
            _ => {
                // At least one side has only profile-id (not full PTL).
                // Match on profile only, using FALLBACK for missing values.

                // Get profile from PTL if present, otherwise from profile_id field
                let c0_profile = c0
                    .format
                    .h265_profile_tier_level
                    .map(|ptl| ptl.profile().to_id())
                    .or_else(|| c0.format.profile_id.map(|p| p as u8))
                    .unwrap_or(H265ProfileTierLevel::FALLBACK.profile().to_id());

                let c1_profile = c1
                    .format
                    .h265_profile_tier_level
                    .map(|ptl| ptl.profile().to_id())
                    .or_else(|| c1.format.profile_id.map(|p| p as u8))
                    .unwrap_or(H265ProfileTierLevel::FALLBACK.profile().to_id());

                // Profiles must match
                if c0_profile != c1_profile {
                    return None;
                }

                // When only profile is specified, we can't verify tier/level compatibility.
                // Return a lower score to prefer exact PTL matches when available.
                Some(Self::PROFILE_ONLY_MATCH_SCORE)
            }
        }
    }

    pub(crate) fn match_h266_score(c0: CodecSpec, c1: CodecSpec) -> Option<usize> {
        match (
            c0.format.h266_profile_tier_level,
            c1.format.h266_profile_tier_level,
        ) {
            (Some(c0_ptl), Some(c1_ptl)) => {
                // Both have full PTL - strict matching

                // Profiles must match exactly.
                // RFC 9328 §7.2: profile-id is a configuration parameter
                // that MUST be used symmetrically in offer/answer.
                if c0_ptl.profile() != c1_ptl.profile() {
                    return None;
                }

                // Tiers must match exactly.
                // RFC 9328 §7.2: tier-flag is a configuration parameter
                // that MUST be used symmetrically in offer/answer.
                if c0_ptl.tier() != c1_ptl.tier() {
                    return None;
                }

                // Level difference penalizes score but never causes rejection.
                let level_difference: usize =
                    c0_ptl.level().ordinal().abs_diff(c1_ptl.level().ordinal());

                Some(Self::EXACT_MATCH_SCORE.saturating_sub(level_difference))
            }
            _ => {
                // At least one side has no full PTL (e.g. a GStreamer offer
                // with a bare rtpmap and no fmtp). Match on profile only,
                // using FALLBACK for missing values.
                let c0_profile = c0
                    .format
                    .h266_profile_tier_level
                    .map(|ptl| ptl.profile().to_id())
                    .or_else(|| c0.format.profile_id.map(|p| p as u8))
                    .unwrap_or(H266ProfileTierLevel::FALLBACK.profile().to_id());

                let c1_profile = c1
                    .format
                    .h266_profile_tier_level
                    .map(|ptl| ptl.profile().to_id())
                    .or_else(|| c1.format.profile_id.map(|p| p as u8))
                    .unwrap_or(H266ProfileTierLevel::FALLBACK.profile().to_id());

                // Profiles must match
                if c0_profile != c1_profile {
                    return None;
                }

                Some(Self::PROFILE_ONLY_MATCH_SCORE)
            }
        }
    }

    pub(crate) fn update_param(
        &mut self,
        remote_pts: &[PayloadParams],
        claimed: &mut [bool; 128],
        local_is_controlling: bool,
        unlocked: &mut HashSet<Pt>,
    ) {
        let Some((first, _)) = remote_pts
            .iter()
            // One unrestricted AMR-WB payload can match several local mode sets.
            // Leave the other alternatives unlocked once its PT has been claimed.
            .filter(|p| {
                self.spec.codec != Codec::AmrWb
                    || self.locked
                    || local_is_controlling
                    || !claimed.is_claimed(p.pt)
            })
            .filter_map(|p| self.match_score(p).map(|s| (p, s)))
            .max_by_key(|(_, s)| *s)
        else {
            return;
        };

        // The first negotiated m-line determines TWCC support for this PT.
        if !self.locked {
            self.fb_transport_cc &= first.fb_transport_cc();
        }

        if self.spec.codec.is_tele() {
            let local = self
                .spec
                .format
                .telephone_event_max
                .unwrap_or(FormatParams::DEFAULT_TELEPHONE_EVENT_MAX);
            let remote = first
                .spec
                .format
                .telephone_event_max
                .unwrap_or(FormatParams::DEFAULT_TELEPHONE_EVENT_MAX);
            self.spec.format.telephone_event_max = Some(local.min(remote));
        }

        // Mirror the remote's H.265 fmtp shape: echo back only the params they offered.
        if self.spec.codec == Codec::H265 && first.spec.codec == Codec::H265 {
            if let Some(remote_ptl) = first.spec.format.h265_profile_tier_level {
                // Narrow level to min(local, remote) so the negotiated level
                // never exceeds either side's capability.
                let negotiated_ptl =
                    if let Some(local_ptl) = self.spec.format.h265_profile_tier_level {
                        remote_ptl.with_level(std::cmp::min(local_ptl.level(), remote_ptl.level()))
                    } else {
                        remote_ptl
                    };
                self.spec.format.h265_profile_tier_level = Some(negotiated_ptl);
                // Avoid also serializing `profile-id` via the VP9 `profile_id` field.
                self.spec.format.profile_id = None;
            } else if let Some(profile_id) = first.spec.format.profile_id {
                // Remote only offered `profile-id`.
                self.spec.format.h265_profile_tier_level = None;
                self.spec.format.profile_id = Some(profile_id);
            } else {
                // No remote H.265 fmtp, omit ours as well.
                self.spec.format.h265_profile_tier_level = None;
                self.spec.format.profile_id = None;
            }

            // Adopt remote's sprop-max-don-diff so the depacketizer knows whether
            // incoming packets contain DONL fields (RFC 7798 §7.1).
            // The packetizer also uses this to decide whether to emit DONL.
            if first.spec.format.sprop_max_don_diff.is_some() {
                self.spec.format.sprop_max_don_diff = first.spec.format.sprop_max_don_diff;
            }
        }

        // Same negotiation for H.266 (RFC 9328 §7.2, offer/answer model §7.3.2).
        if self.spec.codec == Codec::H266 && first.spec.codec == Codec::H266 {
            if let Some(remote_ptl) = first.spec.format.h266_profile_tier_level {
                // Narrow level to min(local, remote) so the negotiated level
                // never exceeds either side's capability.
                let negotiated_ptl =
                    if let Some(local_ptl) = self.spec.format.h266_profile_tier_level {
                        remote_ptl.with_level(std::cmp::min(local_ptl.level(), remote_ptl.level()))
                    } else {
                        remote_ptl
                    };
                self.spec.format.h266_profile_tier_level = Some(negotiated_ptl);
                // Avoid also serializing `profile-id` via the VP9 `profile_id` field.
                self.spec.format.profile_id = None;
            } else if let Some(profile_id) = first.spec.format.profile_id {
                // Remote only offered `profile-id`.
                self.spec.format.h266_profile_tier_level = None;
                self.spec.format.profile_id = Some(profile_id);
            } else {
                // No remote H.266 fmtp, omit ours as well.
                self.spec.format.h266_profile_tier_level = None;
                self.spec.format.profile_id = None;
            }

            // Adopt remote's sprop-max-don-diff (RFC 9328 §7.2) — enables
            // DONL on both packetizer and depacketizer.
            if first.spec.format.sprop_max_don_diff.is_some() {
                self.spec.format.sprop_max_don_diff = first.spec.format.sprop_max_don_diff;
            }
        }

        // KNOWN LIMITATION: H.264 has no equivalent level-narrowing pass here.
        // `match_h264_score` treats the level as negotiable, but we keep our
        // locally configured `profile-level-id` in the answer instead of
        // narrowing it to `min(local, remote)`. Per RFC 6184 §8.2.2, when
        // `level-asymmetry-allowed` is not set the answer should use the lower
        // level.

        // RFC 4867 §8.3.1: an answer repeats the offer's octet-align and channels,
        // and a mode-set from either side applies to both directions.
        if self.spec.codec == Codec::AmrWb {
            let remote = first.spec;
            self.spec.channels = remote.channels;
            self.spec.format.octet_align = remote.format.octet_align;
            self.spec.format.mode_set = self.spec.format.mode_set.or(remote.format.mode_set);
        }

        let mut remote_pt = first.pt;
        let mut remote_rtx = first.resend;

        if self.locked {
            // This can happen if the incoming PTs are suggestions (send-direction) rather than demanded
            // (receive-direction). We only want to warn if we get receive direction changes.
            if local_is_controlling {
                return;
            }
            // Just verify it's still the same. We should validate this in apply_offer/answer instead
            // of ever seeing this error message.
            if self.pt != remote_pt {
                warn!("Ignore remote PT change {} => {}", self.pt, remote_pt);
            }

            if self.resend != remote_rtx {
                warn!(
                    "Ignore remote PT RTX change {:?} => {:?}",
                    self.resend, remote_rtx
                );
            }

            if self.red != first.red {
                warn!(
                    "Ignore remote PT RED change {:?} => {:?}",
                    self.red, first.red
                );
            }
        } else {
            // Before locking, check if the remote PT conflicts with an already locked PT.
            // If we're receiving, we control what PTs to use,
            // so we can remap the conflicting PT.
            if local_is_controlling && claimed.is_claimed(remote_pt) {
                // Prefer our configured PT, then fall back to any free PT.
                let new_pt = (!claimed.is_claimed(self.pt))
                    .then_some(self.pt)
                    .or_else(|| claimed.find_unclaimed(PREFERED_RANGES, unlocked));

                if let Some(new_pt) = new_pt {
                    debug!(
                        "Remapped conflicting PT {} => {} for codec {:?}",
                        remote_pt, new_pt, self.spec.codec
                    );
                    remote_pt = new_pt;
                    unlocked.remove(&new_pt);
                } else {
                    panic!("Exhausted all PT ranges, inconsistent PayloadParam state");
                }
            }

            // Lock down the PT
            self.pt = remote_pt;
            claimed.assert_claim_once(remote_pt);

            // Check if RTX PT also conflicts
            if local_is_controlling {
                if let Some(rtx) = remote_rtx {
                    if claimed.is_claimed(rtx) {
                        let new_rtx = self
                            .resend
                            .filter(|pt| !claimed.is_claimed(*pt))
                            .or_else(|| claimed.find_unclaimed(PREFERED_RANGES, unlocked));

                        if let Some(new_rtx) = new_rtx {
                            debug!("Remapped conflicting RTX PT {:?} => {}", rtx, new_rtx);
                            remote_rtx = Some(new_rtx);
                            unlocked.remove(&new_rtx);
                        } else {
                            panic!("Exhausted all PT ranges, inconsistent PayloadParam state");
                        }
                    }
                }
            }

            // Lock down the RTX PT
            self.resend = remote_rtx;
            if let Some(rtx) = remote_rtx {
                claimed.assert_claim_once(rtx);
            }

            // RED is opt-in on our side and audio-only: keep it only if the matched remote also
            // offers RED (`and`) and our codec is an audio codec. A RED PT can still collide (two
            // codecs mapped to the same PT), so resolve any collision before claiming it. The
            // collision check runs regardless of who controls PT allocation: only the resolution
            // differs, and claiming the same PT twice would panic.
            let mut remote_red = self
                .red
                .and(first.red)
                .filter(|_| self.spec.codec.is_audio());
            if let Some(red) = remote_red {
                if claimed.is_claimed(red) {
                    if local_is_controlling {
                        // We control PT allocation: remap the conflicting RED to a free PT.
                        if let Some(new_red) = claimed.find_unclaimed(PREFERED_RANGES, unlocked) {
                            debug!("Remapped conflicting RED PT {:?} => {}", red, new_red);
                            remote_red = Some(new_red);
                            unlocked.remove(&new_red);
                        } else {
                            // RED is opt-in; drop it rather than panic if no PT is free.
                            debug!("No free PT for RED; dropping it");
                            remote_red = None;
                        }
                    } else {
                        // The remote dictated the PTs and mapped a RED PT that is already taken.
                        // We can't reassign PTs the remote controls, and RED is opt-in, so drop it
                        // rather than claim the same PT twice (which would panic).
                        debug!("Conflicting remote RED PT {:?}; dropping it", red);
                        remote_red = None;
                    }
                }
            }
            self.red = remote_red;
            if let Some(red) = remote_red {
                claimed.assert_claim_once(red);
            }

            // This is now locked.
            self.locked = true;
        }
    }
}

#[cfg(test)]
mod test {
    use crate::rtp_::Frequency;

    use super::*;
    use crate::format::{CodecSpec, FormatParams};

    #[test]
    fn payload_params_red_defaults_none() {
        let spec = CodecSpec {
            codec: Codec::Opus,
            clock_rate: Frequency::NINETY_KHZ,
            channels: Some(2),
            format: FormatParams::default(),
        };
        let p = PayloadParams::new(Pt::new_with_value(111), None, spec);
        assert_eq!(p.red(), None);
    }

    #[test]
    fn red_pt_remapped_on_conflict() {
        use std::collections::HashSet;

        // When we control PT allocation (the remote is send-only) and the RED PT we would adopt
        // from the remote is already taken, RED is remapped to a free PT rather than colliding
        // or being dropped.
        let spec = CodecSpec {
            codec: Codec::Opus,
            clock_rate: Frequency::FORTY_EIGHT_KHZ,
            channels: Some(2),
            format: FormatParams::default(),
        };

        let red_pt = Pt::new_with_value(63);

        let mut local = PayloadParams::new(Pt::new_with_value(111), None, spec);
        local.red = Some(red_pt);

        let mut remote = PayloadParams::new(Pt::new_with_value(111), None, spec);
        remote.red = Some(red_pt);

        // Pre-claim the RED PT as if another payload already holds it.
        let mut claimed = [false; 128];
        claimed[*red_pt as usize] = true;
        let mut unlocked = HashSet::new();

        // local_is_controlling = true: we allocate PTs and may remap the conflict.
        local.update_param(&[remote], &mut claimed, true, &mut unlocked);

        let got = local.red.expect("RED is kept, remapped to a free PT");
        assert_ne!(
            got, red_pt,
            "RED PT must be remapped away from the conflict"
        );
        assert!(
            claimed.is_claimed(got),
            "the remapped RED PT must be claimed"
        );
    }

    mod h264 {
        use super::*;

        fn h264_codec_spec(
            level_asymmetry_allowed: Option<bool>,
            packetization_mode: Option<u8>,
            profile_level_id: Option<u32>,
        ) -> CodecSpec {
            CodecSpec {
                codec: Codec::H264,
                clock_rate: Frequency::NINETY_KHZ,
                channels: None,
                format: FormatParams {
                    level_asymmetry_allowed,
                    packetization_mode,
                    profile_level_id,
                    ..Default::default()
                },
            }
        }

        #[test]
        fn test_h264_profile_matching() {
            struct Case {
                c0: CodecSpec,
                c1: CodecSpec,
                must_match: bool,
                msg: &'static str,
            }

            let cases = [
                Case {
                    c0: h264_codec_spec(None, None, Some(0x42E01F)),
                    c1: h264_codec_spec(None, None, Some(0x4DA01F)),
                    must_match: true,
                    msg: "0x42A01F and 0x4DF01F should match, they are both constrained baseline subprofile",
                },
                Case {
                    c0: h264_codec_spec(None, None, Some(0x42E01F)),
                    c1: h264_codec_spec(None, Some(1), Some(0x4DA01F)),
                    must_match: false,
                    msg: "0x42A01F and 0x4DF01F with differing packetization modes should not match",
                },
                Case {
                    c0: h264_codec_spec(None, Some(0), Some(0x422000)),
                    c1: h264_codec_spec(None, None, Some(0x42B00A)),
                    must_match: true,
                    msg: "0x424000 and 0x42B00A should match because they are both the baseline subprofile \
                    and the level idc of 0x42F01F will be adjusted to Level1B because the constraint \
                    set 3 flag is set",
                },
            ];

            for Case {
                c0,
                c1,
                must_match,
                msg,
            } in cases.into_iter()
            {
                let matched = PayloadParams::match_h264_score(c0, c1).is_some();
                assert_eq!(matched, must_match, "{msg}\nc0: {c0:#?}\nc1: {c1:#?}");
            }
        }

        #[test]
        fn test_h264_level_matching_rfc_compliant() {
            struct Case {
                c0: CodecSpec,
                c1: CodecSpec,
                expected: Option<usize>,
                msg: &'static str,
            }

            let cases = [
                // Test 1: Same profile, same level -> should match
                Case {
                    c0: h264_codec_spec(None, None, Some(0x42e028)), // CB L4.0
                    c1: h264_codec_spec(None, None, Some(0x42e028)), // CB L4.0
                    expected: Some(PayloadParams::EXACT_MATCH_SCORE),
                    msg: "Same profile (CB) and same level (4.0) should match",
                },
                // Test 2: Same profile, offered level lower -> should match (RFC 6184)
                Case {
                    c0: h264_codec_spec(None, None, Some(0x42e028)), // CB L4.0 (configured)
                    c1: h264_codec_spec(None, None, Some(0x42e01f)), // CB L3.1 (offered)
                    expected: Some(PayloadParams::EXACT_MATCH_SCORE - 2),
                    msg: "Same profile (CB), offered level 3.1 < configured 4.0 should match per RFC 6184",
                },
                // Test 3: Same profile, offered level higher -> should match (RFC 6184)
                Case {
                    c0: h264_codec_spec(None, None, Some(0x42e01f)), // CB L3.1 (configured)
                    c1: h264_codec_spec(None, None, Some(0x42e028)), // CB L4.0 (offered)
                    expected: Some(PayloadParams::EXACT_MATCH_SCORE - 2),
                    msg: "Same profile (CB), offered level 4.0 > configured 3.1 should match, \
                    the level is negotiable per RFC 6184",
                },
                // Test 4: Different profiles -> should NOT match
                Case {
                    c0: h264_codec_spec(None, None, Some(0x42e028)), // CB L4.0
                    c1: h264_codec_spec(None, None, Some(0x4d0028)), // Main L4.0
                    expected: None,
                    msg: "Different profiles (CB vs Main) should NOT match even with same level",
                },
                // Test 5: Main profile, offered lower level -> should match
                Case {
                    c0: h264_codec_spec(None, None, Some(0x4d002a)), // Main L4.2 (configured)
                    c1: h264_codec_spec(None, None, Some(0x4d0028)), // Main L4.0 (offered)
                    expected: Some(PayloadParams::EXACT_MATCH_SCORE - 2),
                    msg: "Same profile (Main), offered level 4.0 < configured 4.2 should match",
                },
                // Test 6: High profile, multiple levels down -> should match
                Case {
                    c0: h264_codec_spec(None, None, Some(0x640033)), // High L5.1 (configured)
                    c1: h264_codec_spec(None, None, Some(0x64001f)), // High L3.1 (offered)
                    expected: Some(PayloadParams::EXACT_MATCH_SCORE - 6),
                    msg: "Same profile (High), offered level 3.1 < configured 5.1 should match",
                },
                // Test 7: Baseline (not CB) with Level1B as offered
                Case {
                    c0: h264_codec_spec(None, None, Some(0x42001f)), // Baseline L3.1
                    c1: h264_codec_spec(None, None, Some(0x420000)), // Baseline Level1B
                    expected: Some(PayloadParams::EXACT_MATCH_SCORE - 8),
                    msg: "Same profile (Baseline), offered level 1B < configured 3.1 should match",
                },
                // Test 8: Baseline (not CB) offered lower level > should match with (special Level1B case)
                Case {
                    c0: h264_codec_spec(None, None, Some(0x420000)), // Baseline Level1B
                    c1: h264_codec_spec(None, None, Some(0x42000a)), // Baseline L1
                    expected: Some(PayloadParams::EXACT_MATCH_SCORE - 1),
                    msg: "Same profile (Baseline), offered level 1 < configured 1B should match",
                },
                // Test 9: Baseline (not CB) offered higher level -> should match (special Level1B case)
                Case {
                    c0: h264_codec_spec(None, None, Some(0x42000a)), // Baseline L1
                    c1: h264_codec_spec(None, None, Some(0x420000)), // Baseline Level1B
                    expected: Some(PayloadParams::EXACT_MATCH_SCORE - 1),
                    msg: "Same profile (Baseline), offered level 1B > configured 1 should match",
                },
                // Test 10: Offered Constrained Baseline level 5.2 is higher than
                // the default configured 3.1. Must match.
                Case {
                    c0: h264_codec_spec(Some(true), Some(1), Some(0x42e01f)), // CB L3.1 (configured)
                    c1: h264_codec_spec(Some(true), Some(1), Some(0x42e034)), // CB L5.2 (offered)
                    expected: Some(PayloadParams::EXACT_MATCH_SCORE - 7),
                    msg: "Same profile (CB), offered level 5.2 > configured 3.1 should match",
                },
                // Test 11: Constrained High (profile-iop 0x0C) must be recognized
                // and match a configured Constrained High.
                Case {
                    c0: h264_codec_spec(Some(true), Some(1), Some(0x640c1f)), // CH L3.1 (configured)
                    c1: h264_codec_spec(Some(true), Some(1), Some(0x640c34)), // CH L5.2 (offered)
                    expected: Some(PayloadParams::EXACT_MATCH_SCORE - 7),
                    msg: "Same profile (Constrained High), offered level 5.2 should match",
                },
                // Test 12: Constrained High is not the same profile as High.
                Case {
                    c0: h264_codec_spec(None, None, Some(0x64001f)), // High L3.1
                    c1: h264_codec_spec(None, None, Some(0x640c1f)), // Constrained High L3.1
                    expected: None,
                    msg: "Different profiles (High vs Constrained High) should NOT match",
                },
            ];

            for Case {
                c0,
                c1,
                expected,
                msg,
            } in cases.into_iter()
            {
                assert_eq!(
                    PayloadParams::match_h264_score(c0, c1),
                    expected,
                    "{msg}\nc0: {c0:#?}\nc1: {c1:#?}"
                );
            }
        }
    }

    mod h265 {
        use super::*;
        use crate::packet::Packetizer;

        fn h265_codec_spec(profile_id: u8, tier_flag: u8, level_id: u8) -> CodecSpec {
            CodecSpec {
                codec: Codec::H265,
                clock_rate: Frequency::NINETY_KHZ,
                channels: None,
                format: FormatParams {
                    h265_profile_tier_level: Some(
                        H265ProfileTierLevel::new(profile_id, tier_flag, level_id).unwrap(),
                    ),
                    ..Default::default()
                },
            }
        }

        fn h265_codec_spec_profile_only(profile_id: u32) -> CodecSpec {
            CodecSpec {
                codec: Codec::H265,
                clock_rate: Frequency::NINETY_KHZ,
                channels: None,
                format: FormatParams {
                    profile_id: Some(profile_id),
                    ..Default::default()
                },
            }
        }

        /// Test basic H.265 profile/tier/level matching scenarios.
        /// Verifies that full ProfileTierLevel parameters are matched correctly.
        #[test]
        fn test_profile_tier_level_matching() {
            struct Case {
                c0: CodecSpec,
                c1: CodecSpec,
                must_match: bool,
                msg: &'static str,
            }

            let cases = [
                // Same profile, tier, level -> should match
                Case {
                    c0: h265_codec_spec(1, 0, 93), // Main, Main tier, Level 3.1
                    c1: h265_codec_spec(1, 0, 93),
                    must_match: true,
                    msg: "Same profile (Main), tier (Main), level (3.1) should match",
                },
                // Same profile and tier, offered level lower -> should match
                Case {
                    c0: h265_codec_spec(1, 0, 120), // Main, Main tier, Level 4.0
                    c1: h265_codec_spec(1, 0, 93),  // Main, Main tier, Level 3.1
                    must_match: true,
                    msg: "Same profile/tier, offered level 3.1 < configured 4.0 should match",
                },
                // Same profile and tier, offered level higher -> should still match (with penalty)
                Case {
                    c0: h265_codec_spec(1, 0, 93),  // Main, Main tier, Level 3.1
                    c1: h265_codec_spec(1, 0, 120), // Main, Main tier, Level 4.0
                    must_match: true,
                    msg: "Same profile/tier, offered level 4.0 > configured 3.1 should match (penalized)",
                },
                // Different profiles -> should NOT match
                Case {
                    c0: h265_codec_spec(1, 0, 93), // Main, Main tier, Level 3.1
                    c1: h265_codec_spec(2, 0, 93), // Main10, Main tier, Level 3.1
                    must_match: false,
                    msg: "Different profiles (Main vs Main10) should NOT match",
                },
                // Different tiers -> should NOT match
                Case {
                    c0: h265_codec_spec(1, 0, 93), // Main, Main tier, Level 3.1
                    c1: h265_codec_spec(1, 1, 93), // Main, High tier, Level 3.1
                    must_match: false,
                    msg: "Different tiers (Main vs High) should NOT match",
                },
                // Main10 profile, same tier, lower level -> should match
                Case {
                    c0: h265_codec_spec(2, 0, 153), // Main10, Main tier, Level 5.1
                    c1: h265_codec_spec(2, 0, 120), // Main10, Main tier, Level 4.0
                    must_match: true,
                    msg: "Main10 profile, offered level 4.0 < configured 5.1 should match",
                },
                // High tier, multiple levels down -> should match
                Case {
                    c0: h265_codec_spec(1, 1, 153), // Main, High tier, Level 5.1
                    c1: h265_codec_spec(1, 1, 93),  // Main, High tier, Level 3.1
                    must_match: true,
                    msg: "High tier, offered level 3.1 < configured 5.1 should match",
                },
            ];

            for Case {
                c0,
                c1,
                must_match,
                msg,
            } in cases.into_iter()
            {
                let matched = PayloadParams::match_h265_score(c0, c1).is_some();
                assert_eq!(matched, must_match, "{msg}\nc0: {c0:#?}\nc1: {c1:#?}");
            }
        }

        /// Test H.265 level matching with exact scores.
        /// Verifies that the score decrements based on level difference.
        #[test]
        fn test_level_matching_scores() {
            struct Case {
                c0: CodecSpec,
                c1: CodecSpec,
                expected: Option<usize>,
                msg: &'static str,
            }

            let cases = [
                // Exact match -> score 100
                Case {
                    c0: h265_codec_spec(1, 0, 93), // Main, Main tier, Level 3.1
                    c1: h265_codec_spec(1, 0, 93),
                    expected: Some(PayloadParams::EXACT_MATCH_SCORE),
                    msg: "Exact match should return score 100",
                },
                // One level down -> score 100 - 1
                Case {
                    c0: h265_codec_spec(1, 0, 120), // Main, Main tier, Level 4.0
                    c1: h265_codec_spec(1, 0, 93),  // Main, Main tier, Level 3.1
                    expected: Some(PayloadParams::EXACT_MATCH_SCORE - 1),
                    msg: "One level difference should return score 99",
                },
                // Two levels down -> score 100 - 2
                Case {
                    c0: h265_codec_spec(1, 0, 123), // Main, Main tier, Level 4.1
                    c1: h265_codec_spec(1, 0, 93),  // Main, Main tier, Level 3.1
                    expected: Some(PayloadParams::EXACT_MATCH_SCORE - 2),
                    msg: "Two level difference should return score 98",
                },
                // Multiple levels down
                Case {
                    c0: h265_codec_spec(1, 0, 153), // Main, Main tier, Level 5.1
                    c1: h265_codec_spec(1, 0, 93),  // Main, Main tier, Level 3.1
                    expected: Some(PayloadParams::EXACT_MATCH_SCORE - 4),
                    msg: "Four level difference (5.1 to 3.1) should return score 96",
                },
                // Offered level higher -> still matches, penalized by 1 ordinal
                Case {
                    c0: h265_codec_spec(1, 0, 93),  // Main, Main tier, Level 3.1
                    c1: h265_codec_spec(1, 0, 120), // Main, Main tier, Level 4.0
                    expected: Some(PayloadParams::EXACT_MATCH_SCORE - 1),
                    msg: "Offered level one higher than configured should return score 99",
                },
                // Different profiles -> None
                Case {
                    c0: h265_codec_spec(1, 0, 93), // Main, Main tier, Level 3.1
                    c1: h265_codec_spec(2, 0, 93), // Main10, Main tier, Level 3.1
                    expected: None,
                    msg: "Different profiles should return None",
                },
                // Different tiers -> None
                Case {
                    c0: h265_codec_spec(1, 0, 93), // Main, Main tier, Level 3.1
                    c1: h265_codec_spec(1, 1, 93), // Main, High tier, Level 3.1
                    expected: None,
                    msg: "Different tiers should return None",
                },
            ];

            for Case {
                c0,
                c1,
                expected,
                msg,
            } in cases.into_iter()
            {
                assert_eq!(
                    PayloadParams::match_h265_score(c0, c1),
                    expected,
                    "{msg}\nc0: {c0:#?}\nc1: {c1:#?}"
                );
            }
        }

        /// Test H.265 profile-only matching (for Chrome compatibility).
        /// When only profile-id is provided without tier/level, match on profile only.
        #[test]
        fn test_profile_only_matching() {
            struct Case {
                c0: CodecSpec,
                c1: CodecSpec,
                expected: Option<usize>,
                msg: &'static str,
            }

            let cases = [
                // Both have profile-only, same profile -> score 90
                Case {
                    c0: h265_codec_spec_profile_only(1), // Main
                    c1: h265_codec_spec_profile_only(1), // Main
                    expected: Some(PayloadParams::PROFILE_ONLY_MATCH_SCORE),
                    msg: "Profile-only match (Main) should return score 90",
                },
                // Both have profile-only, different profiles -> None
                Case {
                    c0: h265_codec_spec_profile_only(1), // Main
                    c1: h265_codec_spec_profile_only(2), // Main10
                    expected: None,
                    msg: "Profile-only with different profiles should return None",
                },
                // One has full PTL, other has profile-only, same profile -> score 90
                Case {
                    c0: h265_codec_spec(1, 0, 93),       // Main, Main tier, Level 3.1
                    c1: h265_codec_spec_profile_only(1), // Main
                    expected: Some(PayloadParams::PROFILE_ONLY_MATCH_SCORE),
                    msg: "Mixed PTL and profile-only with matching profile should return score 90",
                },
                // One has full PTL, other has profile-only, different profiles -> None
                Case {
                    c0: h265_codec_spec(1, 0, 93),       // Main, Main tier, Level 3.1
                    c1: h265_codec_spec_profile_only(2), // Main10
                    expected: None,
                    msg: "Mixed PTL and profile-only with different profiles should return None",
                },
                // Reverse: profile-only vs full PTL, same profile -> score 90
                Case {
                    c0: h265_codec_spec_profile_only(1), // Main
                    c1: h265_codec_spec(1, 0, 93),       // Main, Main tier, Level 3.1
                    expected: Some(PayloadParams::PROFILE_ONLY_MATCH_SCORE),
                    msg: "Profile-only vs full PTL with matching profile should return score 90",
                },
            ];

            for Case {
                c0,
                c1,
                expected,
                msg,
            } in cases.into_iter()
            {
                assert_eq!(
                    PayloadParams::match_h265_score(c0, c1),
                    expected,
                    "{msg}\nc0: {c0:#?}\nc1: {c1:#?}"
                );
            }
        }

        /// Test negative cases for H.265 matching to ensure proper rejection.
        #[test]
        fn test_negative_cases() {
            struct Case {
                c0: CodecSpec,
                c1: CodecSpec,
                msg: &'static str,
            }

            let cases = [
                // Profile mismatch with full PTL
                Case {
                    c0: h265_codec_spec(1, 0, 93), // Main, Main tier, Level 3.1
                    c1: h265_codec_spec(2, 0, 93), // Main10, Main tier, Level 3.1
                    msg: "Profile mismatch (Main vs Main10) must not match",
                },
                // Tier mismatch
                Case {
                    c0: h265_codec_spec(1, 0, 93), // Main, Main tier, Level 3.1
                    c1: h265_codec_spec(1, 1, 93), // Main, High tier, Level 3.1
                    msg: "Tier mismatch (Main vs High) must not match",
                },
                // All three different (profile+tier mismatch rejects)
                Case {
                    c0: h265_codec_spec(1, 0, 93),  // Main, Main tier, Level 3.1
                    c1: h265_codec_spec(2, 1, 153), // Main10, High tier, Level 5.1
                    msg: "Profile and tier different must not match",
                },
                // Profile-only mismatch
                Case {
                    c0: h265_codec_spec_profile_only(1), // Main
                    c1: h265_codec_spec_profile_only(2), // Main10
                    msg: "Profile-only mismatch must not match",
                },
                // Mixed full PTL vs profile-only with different profiles
                Case {
                    c0: h265_codec_spec(2, 0, 93),       // Main10, Main tier, Level 3.1
                    c1: h265_codec_spec_profile_only(1), // Main
                    msg: "Full PTL (Main10) vs profile-only (Main) must not match",
                },
            ];

            for Case { c0, c1, msg } in cases.into_iter() {
                assert_eq!(
                    PayloadParams::match_h265_score(c0, c1),
                    None,
                    "{msg}\nc0: {c0:#?}\nc1: {c1:#?}"
                );
            }
        }

        /// Test that sprop-max-don-diff is adopted from remote SDP during negotiation,
        /// and that packetizer/depacketizer are correctly initialized with DONL enabled.
        #[test]
        fn test_sprop_max_don_diff_negotiation_enables_donl() {
            use crate::packet::{CodecDepacketizer, CodecPacketizer};

            // Local config: no sprop-max-don-diff (default)
            let local_spec = h265_codec_spec(1, 0, 93);
            let mut local_params = PayloadParams::new(
                Pt::new_with_value(102),
                Some(Pt::new_with_value(103)),
                local_spec,
            );
            assert_eq!(
                local_params.spec.format.sprop_max_don_diff, None,
                "Local should have no sprop-max-don-diff initially"
            );

            // Remote offers H.265 with sprop-max-don-diff=32
            let mut remote_spec = h265_codec_spec(1, 0, 93);
            remote_spec.format.sprop_max_don_diff = Some(32);
            let remote_params = PayloadParams::new(
                Pt::new_with_value(102),
                Some(Pt::new_with_value(103)),
                remote_spec,
            );

            // Simulate SDP negotiation (update_param merges remote into local)
            let mut claimed = [false; 128];
            let mut unlocked = std::collections::HashSet::new();
            local_params.update_param(
                &[remote_params],
                &mut claimed,
                false, // remote is controlling (we're answering)
                &mut unlocked,
            );

            // Verify sprop-max-don-diff was adopted from remote
            assert_eq!(
                local_params.spec.format.sprop_max_don_diff,
                Some(32),
                "sprop-max-don-diff should be adopted from remote after negotiation"
            );

            // Verify packetizer is initialized with DONL enabled
            let donl_enabled = local_params.spec.format.sprop_max_don_diff.unwrap_or(0) > 0;
            assert!(
                donl_enabled,
                "DONL should be enabled when sprop-max-don-diff > 0"
            );

            let mut pack = CodecPacketizer::new(
                local_params.spec.codec,
                crate::format::Vp9PacketizerMode::default(),
            );
            if let CodecPacketizer::H265(ref mut h265) = pack {
                h265.with_donl(donl_enabled);
            }

            // Packetize a NAL — should include DONL field (2 extra bytes)
            let nalu = vec![0x02, 0x01, 0xAA, 0xBB];
            let packets = pack.packetize(1200, &nalu).unwrap();
            assert_eq!(packets.len(), 1);
            assert_eq!(
                packets[0].len(),
                nalu.len() + 2, // +2 for DONL
                "Packetized output should include 2-byte DONL field"
            );

            // Verify depacketizer is initialized with DONL enabled
            let mut depack: CodecDepacketizer = local_params.spec.codec.into();
            if let CodecDepacketizer::H265(ref mut h265) = depack {
                h265.with_donl(donl_enabled);
            }

            // Depacketize the packet — should correctly strip DONL
            use crate::packet::{CodecExtra, Depacketizer};
            let mut out = Vec::new();
            let mut extra = CodecExtra::None;
            if let CodecDepacketizer::H265(ref mut h265) = depack {
                h265.depacketize(&packets[0], &mut out, &mut extra).unwrap();
            }

            // Output should be Annex-B: start code + original NAL
            assert_eq!(&out[0..4], &[0x00, 0x00, 0x00, 0x01]);
            assert_eq!(
                &out[4..],
                &nalu[..],
                "Depacketized NAL should match original"
            );
        }

        /// Test that sprop-max-don-diff=0 from remote does NOT enable DONL.
        #[test]
        fn test_sprop_max_don_diff_zero_disables_donl() {
            let local_spec = h265_codec_spec(1, 0, 93);
            let mut local_params = PayloadParams::new(
                Pt::new_with_value(102),
                Some(Pt::new_with_value(103)),
                local_spec,
            );

            // Remote offers with sprop-max-don-diff=0 (no reordering)
            let mut remote_spec = h265_codec_spec(1, 0, 93);
            remote_spec.format.sprop_max_don_diff = Some(0);
            let remote_params = PayloadParams::new(
                Pt::new_with_value(102),
                Some(Pt::new_with_value(103)),
                remote_spec,
            );

            let mut claimed = [false; 128];
            let mut unlocked = std::collections::HashSet::new();
            local_params.update_param(&[remote_params], &mut claimed, false, &mut unlocked);

            assert_eq!(local_params.spec.format.sprop_max_don_diff, Some(0));
            let donl_enabled = local_params.spec.format.sprop_max_don_diff.unwrap_or(0) > 0;
            assert!(
                !donl_enabled,
                "DONL should NOT be enabled when sprop-max-don-diff=0"
            );

            // Packetize without DONL — packet should be same size as NAL
            let nalu = vec![0x02, 0x01, 0xAA, 0xBB];
            let mut pack = crate::packet::CodecPacketizer::new(
                local_params.spec.codec,
                crate::format::Vp9PacketizerMode::default(),
            );
            // Don't call with_donl(true) since donl_enabled is false
            let packets = pack.packetize(1200, &nalu).unwrap();
            assert_eq!(
                packets[0].len(),
                nalu.len(),
                "No DONL field when sprop-max-don-diff=0"
            );
        }

        /// Test that missing sprop-max-don-diff from remote preserves local value.
        #[test]
        fn test_sprop_max_don_diff_absent_preserves_local() {
            // Local has sprop-max-don-diff=16
            let mut local_spec = h265_codec_spec(1, 0, 93);
            local_spec.format.sprop_max_don_diff = Some(16);
            let mut local_params = PayloadParams::new(
                Pt::new_with_value(102),
                Some(Pt::new_with_value(103)),
                local_spec,
            );

            // Remote offers without sprop-max-don-diff
            let remote_spec = h265_codec_spec(1, 0, 93);
            assert!(remote_spec.format.sprop_max_don_diff.is_none());
            let remote_params = PayloadParams::new(
                Pt::new_with_value(102),
                Some(Pt::new_with_value(103)),
                remote_spec,
            );

            let mut claimed = [false; 128];
            let mut unlocked = std::collections::HashSet::new();
            local_params.update_param(&[remote_params], &mut claimed, false, &mut unlocked);

            // Local value should be preserved (remote didn't override)
            assert_eq!(
                local_params.spec.format.sprop_max_don_diff,
                Some(16),
                "Local sprop-max-don-diff should be preserved when remote doesn't specify it"
            );
        }
    }

    mod h266 {
        use super::*;
        use crate::packet::H266ProfileTierLevel;
        use crate::packet::Packetizer;

        fn h266_codec_spec(profile_id: u8, tier_flag: u8, level_id: u8) -> CodecSpec {
            CodecSpec {
                codec: Codec::H266,
                clock_rate: Frequency::NINETY_KHZ,
                channels: None,
                format: FormatParams {
                    h266_profile_tier_level: Some(
                        H266ProfileTierLevel::new(profile_id, tier_flag, level_id).unwrap(),
                    ),
                    ..Default::default()
                },
            }
        }

        fn h266_codec_spec_profile_only(profile_id: u32) -> CodecSpec {
            CodecSpec {
                codec: Codec::H266,
                clock_rate: Frequency::NINETY_KHZ,
                channels: None,
                format: FormatParams {
                    profile_id: Some(profile_id),
                    ..Default::default()
                },
            }
        }

        /// Test basic H.266 profile/tier/level matching scenarios.
        /// Verifies that full ProfileTierLevel parameters are matched correctly.
        #[test]
        fn test_profile_tier_level_matching() {
            struct Case {
                c0: CodecSpec,
                c1: CodecSpec,
                must_match: bool,
                msg: &'static str,
            }

            let cases = [
                // Same profile, tier, level -> should match
                Case {
                    c0: h266_codec_spec(1, 0, 51), // Main, Main tier, Level 3.1
                    c1: h266_codec_spec(1, 0, 51),
                    must_match: true,
                    msg: "Same profile (Main), tier (Main), level (3.1) should match",
                },
                // Same profile and tier, offered level lower -> should match
                Case {
                    c0: h266_codec_spec(1, 0, 64), // Main, Main tier, Level 4.0
                    c1: h266_codec_spec(1, 0, 51), // Main, Main tier, Level 3.1
                    must_match: true,
                    msg: "Same profile/tier, offered level 3.1 < configured 4.0 should match",
                },
                // Same profile and tier, offered level higher -> should still match (with penalty)
                Case {
                    c0: h266_codec_spec(1, 0, 51), // Main, Main tier, Level 3.1
                    c1: h266_codec_spec(1, 0, 64), // Main, Main tier, Level 4.0
                    must_match: true,
                    msg: "Same profile/tier, offered level 4.0 > configured 3.1 should match (penalized)",
                },
                // Different profiles -> should NOT match
                Case {
                    c0: h266_codec_spec(1, 0, 51),  // Main, Main tier, Level 3.1
                    c1: h266_codec_spec(33, 0, 51), // Main10, Main tier, Level 3.1
                    must_match: false,
                    msg: "Different profiles (Main vs Main10) should NOT match",
                },
                // Different tiers -> should NOT match
                Case {
                    c0: h266_codec_spec(1, 0, 51), // Main, Main tier, Level 3.1
                    c1: h266_codec_spec(1, 1, 51), // Main, High tier, Level 3.1
                    must_match: false,
                    msg: "Different tiers (Main vs High) should NOT match",
                },
                // Main10 profile, same tier, lower level -> should match
                Case {
                    c0: h266_codec_spec(33, 0, 83), // Main10, Main tier, Level 5.1
                    c1: h266_codec_spec(33, 0, 64), // Main10, Main tier, Level 4.0
                    must_match: true,
                    msg: "Main10 profile, offered level 4.0 < configured 5.1 should match",
                },
                // High tier, multiple levels down -> should match
                Case {
                    c0: h266_codec_spec(1, 1, 83), // Main, High tier, Level 5.1
                    c1: h266_codec_spec(1, 1, 51), // Main, High tier, Level 3.1
                    must_match: true,
                    msg: "High tier, offered level 3.1 < configured 5.1 should match",
                },
            ];

            for Case {
                c0,
                c1,
                must_match,
                msg,
            } in cases.into_iter()
            {
                let matched = PayloadParams::match_h266_score(c0, c1).is_some();
                assert_eq!(matched, must_match, "{msg}\nc0: {c0:#?}\nc1: {c1:#?}");
            }
        }

        /// Test H.266 level matching with exact scores.
        /// Verifies that the score decrements based on level difference.
        #[test]
        fn test_level_matching_scores() {
            struct Case {
                c0: CodecSpec,
                c1: CodecSpec,
                expected: Option<usize>,
                msg: &'static str,
            }

            let cases = [
                // Exact match -> score 100
                Case {
                    c0: h266_codec_spec(1, 0, 51), // Main, Main tier, Level 3.1
                    c1: h266_codec_spec(1, 0, 51),
                    expected: Some(PayloadParams::EXACT_MATCH_SCORE),
                    msg: "Exact match should return score 100",
                },
                // One level down -> score 100 - 1
                Case {
                    c0: h266_codec_spec(1, 0, 64), // Main, Main tier, Level 4.0
                    c1: h266_codec_spec(1, 0, 51), // Main, Main tier, Level 3.1
                    expected: Some(PayloadParams::EXACT_MATCH_SCORE - 1),
                    msg: "One level difference should return score 99",
                },
                // Two levels down -> score 100 - 2
                Case {
                    c0: h266_codec_spec(1, 0, 67), // Main, Main tier, Level 4.1
                    c1: h266_codec_spec(1, 0, 51), // Main, Main tier, Level 3.1
                    expected: Some(PayloadParams::EXACT_MATCH_SCORE - 2),
                    msg: "Two level difference should return score 98",
                },
                // Multiple levels down
                Case {
                    c0: h266_codec_spec(1, 0, 83), // Main, Main tier, Level 5.1
                    c1: h266_codec_spec(1, 0, 51), // Main, Main tier, Level 3.1
                    expected: Some(PayloadParams::EXACT_MATCH_SCORE - 4),
                    msg: "Four level difference (5.1 to 3.1) should return score 96",
                },
                // Offered level higher -> still matches, penalized by 1 ordinal
                Case {
                    c0: h266_codec_spec(1, 0, 51), // Main, Main tier, Level 3.1
                    c1: h266_codec_spec(1, 0, 64), // Main, Main tier, Level 4.0
                    expected: Some(PayloadParams::EXACT_MATCH_SCORE - 1),
                    msg: "Offered level one higher than configured should return score 99",
                },
                // Different profiles -> None
                Case {
                    c0: h266_codec_spec(1, 0, 51),  // Main, Main tier, Level 3.1
                    c1: h266_codec_spec(33, 0, 51), // Main10, Main tier, Level 3.1
                    expected: None,
                    msg: "Different profiles should return None",
                },
                // Different tiers -> None
                Case {
                    c0: h266_codec_spec(1, 0, 51), // Main, Main tier, Level 3.1
                    c1: h266_codec_spec(1, 1, 51), // Main, High tier, Level 3.1
                    expected: None,
                    msg: "Different tiers should return None",
                },
            ];

            for Case {
                c0,
                c1,
                expected,
                msg,
            } in cases.into_iter()
            {
                assert_eq!(
                    PayloadParams::match_h266_score(c0, c1),
                    expected,
                    "{msg}\nc0: {c0:#?}\nc1: {c1:#?}"
                );
            }
        }

        /// Test H.266 profile-only matching (for Chrome compatibility).
        /// When only profile-id is provided without tier/level, match on profile only.
        #[test]
        fn test_profile_only_matching() {
            struct Case {
                c0: CodecSpec,
                c1: CodecSpec,
                expected: Option<usize>,
                msg: &'static str,
            }

            let cases = [
                // Both have profile-only, same profile -> score 90
                Case {
                    c0: h266_codec_spec_profile_only(1), // Main
                    c1: h266_codec_spec_profile_only(1), // Main
                    expected: Some(PayloadParams::PROFILE_ONLY_MATCH_SCORE),
                    msg: "Profile-only match (Main) should return score 90",
                },
                // Both have profile-only, different profiles -> None
                Case {
                    c0: h266_codec_spec_profile_only(1),  // Main
                    c1: h266_codec_spec_profile_only(33), // Main10
                    expected: None,
                    msg: "Profile-only with different profiles should return None",
                },
                // One has full PTL, other has profile-only, same profile -> score 90
                Case {
                    c0: h266_codec_spec(1, 0, 51),       // Main, Main tier, Level 3.1
                    c1: h266_codec_spec_profile_only(1), // Main
                    expected: Some(PayloadParams::PROFILE_ONLY_MATCH_SCORE),
                    msg: "Mixed PTL and profile-only with matching profile should return score 90",
                },
                // One has full PTL, other has profile-only, different profiles -> None
                Case {
                    c0: h266_codec_spec(1, 0, 51),        // Main, Main tier, Level 3.1
                    c1: h266_codec_spec_profile_only(33), // Main10
                    expected: None,
                    msg: "Mixed PTL and profile-only with different profiles should return None",
                },
                // Reverse: profile-only vs full PTL, same profile -> score 90
                Case {
                    c0: h266_codec_spec_profile_only(1), // Main
                    c1: h266_codec_spec(1, 0, 51),       // Main, Main tier, Level 3.1
                    expected: Some(PayloadParams::PROFILE_ONLY_MATCH_SCORE),
                    msg: "Profile-only vs full PTL with matching profile should return score 90",
                },
            ];

            for Case {
                c0,
                c1,
                expected,
                msg,
            } in cases.into_iter()
            {
                assert_eq!(
                    PayloadParams::match_h266_score(c0, c1),
                    expected,
                    "{msg}\nc0: {c0:#?}\nc1: {c1:#?}"
                );
            }
        }

        /// Test negative cases for H.266 matching to ensure proper rejection.
        #[test]
        fn test_negative_cases() {
            struct Case {
                c0: CodecSpec,
                c1: CodecSpec,
                msg: &'static str,
            }

            let cases = [
                // Profile mismatch with full PTL
                Case {
                    c0: h266_codec_spec(1, 0, 51),  // Main, Main tier, Level 3.1
                    c1: h266_codec_spec(33, 0, 51), // Main10, Main tier, Level 3.1
                    msg: "Profile mismatch (Main vs Main10) must not match",
                },
                // Tier mismatch
                Case {
                    c0: h266_codec_spec(1, 0, 51), // Main, Main tier, Level 3.1
                    c1: h266_codec_spec(1, 1, 51), // Main, High tier, Level 3.1
                    msg: "Tier mismatch (Main vs High) must not match",
                },
                // All three different (profile+tier mismatch rejects)
                Case {
                    c0: h266_codec_spec(1, 0, 51),  // Main, Main tier, Level 3.1
                    c1: h266_codec_spec(33, 1, 83), // Main10, High tier, Level 5.1
                    msg: "Profile and tier different must not match",
                },
                // Profile-only mismatch
                Case {
                    c0: h266_codec_spec_profile_only(1),  // Main
                    c1: h266_codec_spec_profile_only(33), // Main10
                    msg: "Profile-only mismatch must not match",
                },
                // Mixed full PTL vs profile-only with different profiles
                Case {
                    c0: h266_codec_spec(33, 0, 51),      // Main10, Main tier, Level 3.1
                    c1: h266_codec_spec_profile_only(1), // Main
                    msg: "Full PTL (Main10) vs profile-only (Main) must not match",
                },
            ];

            for Case { c0, c1, msg } in cases.into_iter() {
                assert_eq!(
                    PayloadParams::match_h266_score(c0, c1),
                    None,
                    "{msg}\nc0: {c0:#?}\nc1: {c1:#?}"
                );
            }
        }

        /// Test that sprop-max-don-diff is adopted from remote SDP during negotiation,
        /// and that packetizer/depacketizer are correctly initialized with DONL enabled.
        #[test]
        fn test_sprop_max_don_diff_negotiation_enables_donl() {
            use crate::packet::{CodecDepacketizer, CodecPacketizer};

            // Local config: no sprop-max-don-diff (default)
            let local_spec = h266_codec_spec(1, 0, 51);
            let mut local_params = PayloadParams::new(
                Pt::new_with_value(102),
                Some(Pt::new_with_value(103)),
                local_spec,
            );
            assert_eq!(
                local_params.spec.format.sprop_max_don_diff, None,
                "Local should have no sprop-max-don-diff initially"
            );

            // Remote offers H.266 with sprop-max-don-diff=32
            let mut remote_spec = h266_codec_spec(1, 0, 51);
            remote_spec.format.sprop_max_don_diff = Some(32);
            let remote_params = PayloadParams::new(
                Pt::new_with_value(102),
                Some(Pt::new_with_value(103)),
                remote_spec,
            );

            // Simulate SDP negotiation (update_param merges remote into local)
            let mut claimed = [false; 128];
            let mut unlocked = std::collections::HashSet::new();
            local_params.update_param(
                &[remote_params],
                &mut claimed,
                false, // remote is controlling (we're answering)
                &mut unlocked,
            );

            // Verify sprop-max-don-diff was adopted from remote
            assert_eq!(
                local_params.spec.format.sprop_max_don_diff,
                Some(32),
                "sprop-max-don-diff should be adopted from remote after negotiation"
            );

            // Verify packetizer is initialized with DONL enabled
            let donl_enabled = local_params.spec.format.sprop_max_don_diff.unwrap_or(0) > 0;
            assert!(
                donl_enabled,
                "DONL should be enabled when sprop-max-don-diff > 0"
            );

            let mut pack = CodecPacketizer::new(
                local_params.spec.codec,
                crate::format::Vp9PacketizerMode::default(),
            );
            if let CodecPacketizer::H266(ref mut h266) = pack {
                h266.with_donl(donl_enabled);
            }

            // Packetize a NAL — should include DONL field (2 extra bytes)
            let nalu = vec![0x00, 0x09, 0xAA, 0xBB];
            let packets = pack.packetize(1200, &nalu).unwrap();
            assert_eq!(packets.len(), 1);
            assert_eq!(
                packets[0].len(),
                nalu.len() + 2, // +2 for DONL
                "Packetized output should include 2-byte DONL field"
            );

            // Verify depacketizer is initialized with DONL enabled
            let mut depack: CodecDepacketizer = local_params.spec.codec.into();
            if let CodecDepacketizer::H266(ref mut h266) = depack {
                h266.with_donl(donl_enabled);
            }

            // Depacketize the packet — should correctly strip DONL
            use crate::packet::{CodecExtra, Depacketizer};
            let mut out = Vec::new();
            let mut extra = CodecExtra::None;
            if let CodecDepacketizer::H266(ref mut h266) = depack {
                h266.depacketize(&packets[0], &mut out, &mut extra).unwrap();
            }

            // Output should be Annex-B: start code + original NAL
            assert_eq!(&out[0..4], &[0x00, 0x00, 0x00, 0x01]);
            assert_eq!(
                &out[4..],
                &nalu[..],
                "Depacketized NAL should match original"
            );
        }

        /// Test that missing sprop-max-don-diff from remote preserves local value.
        #[test]
        fn test_sprop_max_don_diff_absent_preserves_local() {
            // Local has sprop-max-don-diff=16
            let mut local_spec = h266_codec_spec(1, 0, 51);
            local_spec.format.sprop_max_don_diff = Some(16);
            let mut local_params = PayloadParams::new(
                Pt::new_with_value(102),
                Some(Pt::new_with_value(103)),
                local_spec,
            );

            // Remote offers without sprop-max-don-diff
            let remote_spec = h266_codec_spec(1, 0, 51);
            assert!(remote_spec.format.sprop_max_don_diff.is_none());
            let remote_params = PayloadParams::new(
                Pt::new_with_value(102),
                Some(Pt::new_with_value(103)),
                remote_spec,
            );

            let mut claimed = [false; 128];
            let mut unlocked = std::collections::HashSet::new();
            local_params.update_param(&[remote_params], &mut claimed, false, &mut unlocked);

            // Local value should be preserved (remote didn't override)
            assert_eq!(
                local_params.spec.format.sprop_max_don_diff,
                Some(16),
                "Local sprop-max-don-diff should be preserved when remote doesn't specify it"
            );
        }

        /// Test that sprop-max-don-diff=0 from remote does NOT enable DONL.
        #[test]
        fn test_sprop_max_don_diff_zero_disables_donl() {
            let local_spec = h266_codec_spec(1, 0, 51);
            let mut local_params = PayloadParams::new(
                Pt::new_with_value(102),
                Some(Pt::new_with_value(103)),
                local_spec,
            );

            // Remote offers with sprop-max-don-diff=0 (no reordering)
            let mut remote_spec = h266_codec_spec(1, 0, 51);
            remote_spec.format.sprop_max_don_diff = Some(0);
            let remote_params = PayloadParams::new(
                Pt::new_with_value(102),
                Some(Pt::new_with_value(103)),
                remote_spec,
            );

            let mut claimed = [false; 128];
            let mut unlocked = std::collections::HashSet::new();
            local_params.update_param(&[remote_params], &mut claimed, false, &mut unlocked);

            assert_eq!(local_params.spec.format.sprop_max_don_diff, Some(0));
            let donl_enabled = local_params.spec.format.sprop_max_don_diff.unwrap_or(0) > 0;
            assert!(
                !donl_enabled,
                "DONL should NOT be enabled when sprop-max-don-diff=0"
            );

            // Packetize without DONL — packet should be same size as NAL
            let nalu = vec![0x00, 0x09, 0xAA, 0xBB];
            let mut pack = crate::packet::CodecPacketizer::new(
                local_params.spec.codec,
                crate::format::Vp9PacketizerMode::default(),
            );
            // Don't enable DONL since donl_enabled is false
            let packets = pack.packetize(1200, &nalu).unwrap();
            assert_eq!(
                packets[0].len(),
                nalu.len(),
                "No DONL field when sprop-max-don-diff=0"
            );
        }
    }

    mod amr_wb {
        use super::*;

        fn amr_wb(channels: Option<u8>, format: FormatParams) -> PayloadParams {
            let spec = CodecSpec {
                codec: Codec::AmrWb,
                clock_rate: Frequency::SIXTEEN_KHZ,
                channels,
                format,
            };
            PayloadParams::new(122.into(), None, spec)
        }

        fn octet_aligned() -> FormatParams {
            FormatParams {
                octet_align: Some(true),
                ..Default::default()
            }
        }

        #[test]
        fn matching() {
            let local = amr_wb(Some(1), octet_aligned());
            let score = |remote: PayloadParams| local.match_score(&remote);

            assert!(score(local).is_some());
            assert!(score(amr_wb(None, octet_aligned())).is_some());
            assert!(score(amr_wb(Some(1), FormatParams::default())).is_none());

            let bandwidth_efficient = FormatParams {
                octet_align: Some(false),
                ..Default::default()
            };
            let default = amr_wb(None, FormatParams::default());
            assert!(
                default
                    .match_score(&amr_wb(None, bandwidth_efficient))
                    .is_some()
            );

            let modes = |mode_set| FormatParams {
                mode_set: Some(mode_set),
                ..octet_aligned()
            };
            let restricted = amr_wb(Some(1), modes(0b111));
            assert!(score(restricted).is_some_and(|s| Some(s) < score(local)));
            assert!(restricted.match_score(&amr_wb(None, modes(0b11))).is_none());

            let stereo = amr_wb(Some(2), octet_aligned());
            assert!(stereo.match_score(&stereo).is_none());
        }

        #[test]
        fn update_param_mirrors_layout_channels_and_mode_set() {
            let mut local = amr_wb(Some(1), FormatParams::default());
            let remote_format = FormatParams {
                octet_align: Some(false),
                mode_set: Some(0b111),
                ..Default::default()
            };
            let remote = amr_wb(None, remote_format);

            local.update_param(&[remote], &mut [false; 128], false, &mut HashSet::new());

            assert_eq!(local.spec.channels, None);
            assert_eq!(local.spec.format, remote_format);
        }
    }
}
