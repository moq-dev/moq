use std::collections::HashSet;
use std::ops::{Deref, DerefMut};

use crate::packet::H266ProfileTierLevel;
use crate::packet::MAX_RED_RECOVERY_DEPTH;
use crate::packet::MediaKind;
use crate::rtp_::Pt;
use crate::rtp_::{Direction, Frequency};

use super::codec::{Codec, CodecSpec};
use super::format_params::FormatParams;
use super::payload_params::{Claimed, PREFERED_RANGES, PayloadParams};

/// Default payload type for PCMU (G.711 μ-law).
pub(crate) const PT_PCMU: Pt = Pt::new_with_value(0);

/// Default payload type for PCMA (G.711 A-law).
pub(crate) const PT_PCMA: Pt = Pt::new_with_value(8);

/// Default payload type for G722.
pub(crate) const PT_G722: Pt = Pt::new_with_value(9);

/// Static payload type for Comfort Noise at 8000 Hz (RFC 3389).
pub(crate) const PT_COMFORT_NOISE: Pt = Pt::new_with_value(13);

/// Default payload type for VP8.
pub(crate) const PT_VP8: Pt = Pt::new_with_value(96);

/// Default payload type for VP8 RTX.
pub(crate) const PT_VP8_RTX: Pt = Pt::new_with_value(97);

/// Default payload type for VP9 profile 0.
pub(crate) const PT_VP9: Pt = Pt::new_with_value(98);

/// Default payload type for VP9 profile 0 RTX.
pub(crate) const PT_VP9_RTX: Pt = Pt::new_with_value(99);

/// Default payload type for AV1.
pub(crate) const PT_AV1: Pt = Pt::new_with_value(45);

/// Default payload type for AV1 RTX.
pub(crate) const PT_AV1_RTX: Pt = Pt::new_with_value(46);

/// Default payload type for H265.
pub(crate) const PT_H265: Pt = Pt::new_with_value(102);

/// Default payload type for H265 RTX.
pub(crate) const PT_H265_RTX: Pt = Pt::new_with_value(103);

/// Default payload type for H266 (VVC).
pub(crate) const PT_H266: Pt = Pt::new_with_value(104);

/// Default payload type for H266 RTX.
pub(crate) const PT_H266_RTX: Pt = Pt::new_with_value(105);

/// Default payload type for Opus.
pub(crate) const PT_OPUS: Pt = Pt::new_with_value(111);

/// Default RFC 2198 RED payload types, one per audio codec. RED links a single primary codec
/// via `a=fmtp` and carries that codec's clock rate in `a=rtpmap`, so each audio codec that has
/// RED needs its own RED payload type. These are the defaults; they are reassigned on conflict
/// during negotiation, mirroring RTX. They stay below 64 so they are never mistaken for RTCP by
/// the RTP/RTCP demultiplexer (which reserves payload types 64..=95, see `MultiplexKind`).
pub(crate) const PT_RED: Pt = Pt::new_with_value(63);
pub(crate) const PT_G722_RED: Pt = Pt::new_with_value(62);
pub(crate) const PT_PCMU_RED: Pt = Pt::new_with_value(61);
pub(crate) const PT_PCMA_RED: Pt = Pt::new_with_value(60);

/// Default payload type for AMR-WB.
pub(crate) const PT_AMR_WB: Pt = Pt::new_with_value(122);

/// Session config for all codecs.
#[derive(Debug, Clone, Default)]
pub struct CodecConfig {
    params: Vec<PayloadParams>,
    /// RFC 2198 RED send-side redundancy pattern: how many packets back each redundant level
    /// carries. Empty means the default single level `[1]` (see [`Self::red_distances`]).
    red_distances: Box<[u32]>,
}

impl CodecConfig {
    /// Creates a new empty config.
    pub fn empty() -> Self {
        CodecConfig::default()
    }

    /// Creates a new config from payload params
    pub fn new_from_payload_params(payload_params: Vec<PayloadParams>) -> Self {
        CodecConfig {
            params: payload_params,
            ..Default::default()
        }
    }

    /// Creates a new config with all default configurations enabled.
    pub fn new_with_defaults() -> Self {
        let mut c = Self::empty();
        c.enable_opus(true, false);

        c.enable_vp8(true);
        c.enable_h264(true);
        c.enable_h265(true);
        // H266/VVC off by default: no browser supports it.
        c.enable_h266(false);
        c.enable_vp9(true);
        c.enable_av1(true);

        c
    }

    /// Returns a reference to the payload parameters.
    pub fn params(&self) -> &[PayloadParams] {
        &self.params
    }

    /// Clear all configured configs.
    pub fn clear(&mut self) {
        self.params.clear();
    }

    /// Manually configure a payload type. For telephone events, configure one
    /// payload type per supported RTP clock rate; no rates are added automatically.
    pub fn add_config(
        &mut self,
        pt: Pt,
        resend: Option<Pt>,
        codec: Codec,
        clock_rate: Frequency,
        channels: Option<u8>,
        format: FormatParams,
    ) {
        let (fb_transport_cc, fb_fir, fb_nack, fb_pli, fb_remb) = if codec.is_video() {
            (true, true, true, true, true)
        } else {
            (true, false, false, false, false)
        };

        let p = PayloadParams {
            pt,
            spec: CodecSpec {
                codec,
                clock_rate,
                channels,
                format,
            },
            resend,
            fb_transport_cc,
            fb_fir,
            fb_nack,
            fb_pli,
            fb_remb,
            red: None,
            locked: false,
        };

        self.params.push(p);
    }

    fn add_static_config(&mut self, pt: Pt) {
        let spec = CodecSpec::from_static_pt(pt).expect("known static payload type");
        self.add_config(
            pt,
            None,
            spec.codec,
            spec.clock_rate,
            spec.channels,
            spec.format,
        );
    }

    /// Convenience for adding a h264 payload type.
    pub fn add_h264(
        &mut self,
        pt: Pt,
        resend: Option<Pt>,
        packetization_mode: bool,
        profile_level_id: u32,
    ) {
        self.add_config(
            pt,
            resend,
            Codec::H264,
            Frequency::NINETY_KHZ,
            None,
            FormatParams {
                level_asymmetry_allowed: Some(true),
                packetization_mode: if packetization_mode { Some(1) } else { Some(0) },
                profile_level_id: Some(profile_level_id),
                ..Default::default()
            },
        )
    }

    /// Convenience for adding a h265 payload type.
    pub fn add_h265(
        &mut self,
        pt: Pt,
        resend: Option<Pt>,
        profile_id: u8,
        tier_flag: u8,
        level_id: u8,
    ) {
        use crate::packet::H265ProfileTierLevel;

        let ptl = H265ProfileTierLevel::new(profile_id, tier_flag, level_id)
            .unwrap_or(H265ProfileTierLevel::FALLBACK);

        self.add_config(
            pt,
            resend,
            Codec::H265,
            Frequency::NINETY_KHZ,
            None,
            FormatParams {
                h265_profile_tier_level: Some(ptl),
                ..Default::default()
            },
        )
    }

    /// Convenience for adding a PCM u-law payload type, optionally with RFC 2198 RED.
    ///
    /// `use_red` folds a RED payload type onto this codec so the receiver can recover packet loss
    /// without retransmission, at the cost of extra audio payload size.
    pub fn enable_pcmu(&mut self, enabled: bool, use_red: bool) {
        self.params.retain(|c| c.spec.codec != Codec::PCMU);
        if !enabled {
            return;
        }
        self.add_static_config(PT_PCMU);
        if use_red {
            self.set_red_pt(Codec::PCMU, PT_PCMU_RED);
        }
    }

    /// Convenience for adding a PCM a-law payload type, optionally with RFC 2198 RED.
    ///
    /// See [`Self::enable_pcmu`] for `use_red`.
    pub fn enable_pcma(&mut self, enabled: bool, use_red: bool) {
        self.params.retain(|c| c.spec.codec != Codec::PCMA);
        if !enabled {
            return;
        }
        self.add_static_config(PT_PCMA);
        if use_red {
            self.set_red_pt(Codec::PCMA, PT_PCMA_RED);
        }
    }

    /// Convenience for adding a G722 payload type.
    ///
    /// G722 samples audio at 16 kHz, but per RFC 3551 §4.5.2 the RTP clock rate is
    /// 8000 Hz. The codec is configured at 16 kHz here; str0m maps to 8 kHz for the
    /// SDP `a=rtpmap` line and when converting to/from RTP timestamps. See
    /// <https://en.wikipedia.org/wiki/RTP_payload_formats#cite_note-55>
    ///
    /// See [`Self::enable_pcmu`] for `use_red`.
    pub fn enable_g722(&mut self, enabled: bool, use_red: bool) {
        self.params.retain(|c| c.spec.codec != Codec::G722);
        if !enabled {
            return;
        }
        self.add_static_config(PT_G722);
        if use_red {
            self.set_red_pt(Codec::G722, PT_G722_RED);
        }
    }

    /// Add the static Comfort Noise payload type at 8000 Hz.
    ///
    /// Higher clock rates require dynamic payload types and can be configured
    /// with [`CodecConfig::add_config`].
    pub fn enable_comfort_noise(&mut self, enabled: bool) {
        self.params.retain(|c| c.spec.codec != Codec::CN);
        if !enabled {
            return;
        }
        self.add_static_config(PT_COMFORT_NOISE);
    }

    /// Add a default AMR-WB payload type.
    pub fn enable_amr_wb(&mut self, enabled: bool) {
        self.params.retain(|c| c.spec.codec != Codec::AmrWb);
        if !enabled {
            return;
        }
        self.add_config(
            PT_AMR_WB,
            None,
            Codec::AmrWb,
            Frequency::SIXTEEN_KHZ,
            Some(1),
            FormatParams {
                octet_align: Some(true),
                ..Default::default()
            },
        );
    }

    /// Add a default OPUS payload type, optionally with RFC 2198 RED.
    ///
    /// See [`Self::enable_pcmu`] for `use_red`.
    pub fn enable_opus(&mut self, enabled: bool, use_red: bool) {
        self.params.retain(|c| c.spec.codec != Codec::Opus);
        if !enabled {
            return;
        }
        self.add_config(
            PT_OPUS,
            None,
            Codec::Opus,
            Frequency::FORTY_EIGHT_KHZ,
            Some(2),
            FormatParams {
                min_p_time: Some(10),
                use_inband_fec: Some(true),
                ..Default::default()
            },
        );
        if use_red {
            self.set_red_pt(Codec::Opus, PT_RED);
        }
    }

    /// Fold the given RFC 2198 RED payload type onto the enabled param for `codec`. Used by the
    /// audio enable fns when `use_red` is set; each audio codec has its own RED payload type so the
    /// `a=rtpmap` entries (which carry each codec's clock rate) don't collide in SDP.
    fn set_red_pt(&mut self, codec: Codec, red_pt: Pt) {
        if let Some(p) = self.params.iter_mut().find(|p| p.spec.codec == codec) {
            p.red = Some(red_pt);
        }
    }

    /// The RFC 2198 RED send-side redundancy pattern: how many packets back each redundant level
    /// carries. Defaults to `[1]` (one level, the previous packet, as browsers send).
    pub fn red_distances(&self) -> &[u32] {
        if self.red_distances.is_empty() {
            &[1]
        } else {
            &self.red_distances
        }
    }

    /// Set the RFC 2198 RED send-side redundancy pattern. Each entry is how many packets back a
    /// redundant level carries: `[1]` is one level (the previous packet), `[1, 3, 5]` sends three
    /// levels for higher loss. Deeper redundancy trades bandwidth for resilience.
    ///
    /// The input is sanitised: zeros and values deeper than the receiver recovery depth are
    /// dropped, then the rest is sorted and de-duplicated. Distance 1 is always included: recovery
    /// derives each block's sequence distance from the closest redundant block, treating it as one
    /// packet back, so a pattern must anchor on distance 1 or the receiver maps the blocks to the
    /// wrong sequence numbers (a `[2, 4]` pattern becomes `[1, 2, 4]`). An empty or fully invalid
    /// input falls back to `[1]`. This only sets the pattern; RED still has to be enabled via the
    /// `use_red` argument of an audio enable fn (e.g. [`Self::enable_opus`]) or negotiated.
    pub fn set_red_distances(&mut self, distances: &[u32]) {
        let mut sane: Vec<u32> = distances
            .iter()
            .copied()
            .filter(|&d| (1..=MAX_RED_RECOVERY_DEPTH as u32).contains(&d))
            .collect();
        sane.sort_unstable();
        sane.dedup();
        // Always anchor on distance 1 so the receiver can establish the packet duration from the
        // closest block; without it a pattern like `[2, 4]` is silently mis-recovered.
        if sane.first() != Some(&1) {
            sane.insert(0, 1);
        }
        self.red_distances = sane.into_boxed_slice();
    }

    /// Add a default VP8 payload type.
    pub fn enable_vp8(&mut self, enabled: bool) {
        self.params.retain(|c| c.spec.codec != Codec::Vp8);
        if !enabled {
            return;
        }
        self.add_config(
            PT_VP8,
            Some(PT_VP8_RTX),
            Codec::Vp8,
            Frequency::NINETY_KHZ,
            None,
            FormatParams::default(),
        )
    }

    /// Add a default H264 payload type.
    pub fn enable_h264(&mut self, enabled: bool) {
        self.params.retain(|c| c.spec.codec != Codec::H264);
        if !enabled {
            return;
        }
        const PARAMS: &[(u8, u8, bool, u32)] = &[
            (127, 121, true, 0x42001f),
            (125, 107, false, 0x42001f),
            (108, 109, true, 0x42e01f),
            (124, 120, false, 0x42e01f),
            (123, 119, true, 0x4d001f),
            (35, 36, false, 0x4d001f),
            (114, 115, true, 0x64001f),
        ];

        for p in PARAMS {
            self.add_h264(p.0.into(), Some(p.1.into()), p.2, p.3)
        }
    }

    /// Add a default H265 payload type.
    /// This enables H265 as a video codec (clock rate 90kHz) with default RTX payload type.
    /// Note: H265 is still considered an experimental/hidden codec in parts of the public API,
    /// but it is supported internally for packetization/depacketization.
    pub fn enable_h265(&mut self, enabled: bool) {
        self.params.retain(|c| c.spec.codec != Codec::H265);
        if !enabled {
            return;
        }

        // Start with single default configuration
        // Profile/tier/level negotiation will happen via SDP fmtp parameters
        //
        // Level 6.0 (level_id=180) — a high capability level well supported by modern browsers.
        // Level-id formula per ITU-T H.265 Annex A: level_id = (a*10 + b) * 3
        //   e.g. Level 6.0 → (6*10 + 0) * 3 = 180
        //
        // Chromium H265Level enum definition (kLevel6 = 180):
        //   https://source.chromium.org/chromium/chromium/src/+/main:
        //   third_party/webrtc/api/video_codecs/h265_profile_tier_level.h
        self.add_h265(PT_H265, Some(PT_H265_RTX), 1, 0, 180); // Main, Main tier, Level 6.0
    }

    /// Convenience for adding a h266 payload type.
    pub fn add_h266(
        &mut self,
        pt: Pt,
        resend: Option<Pt>,
        profile_id: u8,
        tier_flag: u8,
        level_id: u8,
    ) {
        let ptl = H266ProfileTierLevel::new(profile_id, tier_flag, level_id)
            .unwrap_or(H266ProfileTierLevel::FALLBACK);

        self.add_config(
            pt,
            resend,
            Codec::H266,
            Frequency::NINETY_KHZ,
            None,
            FormatParams {
                h266_profile_tier_level: Some(ptl),
                ..Default::default()
            },
        )
    }

    /// Add a default H266 (VVC) payload type.
    ///
    /// Enables H266 as a video codec (clock rate 90kHz) with a default RTX
    /// payload type. Profile/tier/level negotiation happens via the SDP fmtp
    /// parameters profile-id / tier-flag / level-id (RFC 9328 §7.2).
    pub fn enable_h266(&mut self, enabled: bool) {
        self.params.retain(|c| c.spec.codec != Codec::H266);
        if !enabled {
            return;
        }

        // Single default configuration with a high capability level.
        //
        // Level 6.0 (level_id=96) — per ITU-T H.266 Annex A the level_id is
        // 16*major + 3*minor, e.g. Level 6.0 → 16*6 = 96.
        self.add_h266(PT_H266, Some(PT_H266_RTX), 1, 0, 96); // Main 10, Main tier, Level 6.0
    }

    /// Add a default VP9 payload type.
    pub fn enable_vp9(&mut self, enabled: bool) {
        self.params.retain(|c| c.spec.codec != Codec::Vp9);
        if !enabled {
            return;
        }
        self.add_config(
            PT_VP9,
            Some(PT_VP9_RTX),
            Codec::Vp9,
            Frequency::NINETY_KHZ,
            None,
            FormatParams {
                profile_id: Some(0),
                ..Default::default()
            },
        );
        self.add_config(
            100.into(),
            Some(101.into()),
            Codec::Vp9,
            Frequency::NINETY_KHZ,
            None,
            FormatParams {
                profile_id: Some(2),
                ..Default::default()
            },
        );
    }

    /// Add a default AV1 payload type.
    pub fn enable_av1(&mut self, enabled: bool) {
        self.params.retain(|c| c.spec.codec != Codec::Av1);
        if !enabled {
            return;
        }
        self.add_config(
            PT_AV1,
            Some(PT_AV1_RTX),
            Codec::Av1,
            Frequency::NINETY_KHZ,
            None,
            FormatParams::default(),
        );
    }

    /// Match the given parameters to the configured parameters.
    ///
    /// In a server scenario, a certain codec configuration might not have the same
    /// payload type (PT) for two different peers. We will have incoming data with one
    /// PT and need to match that against the PT of the outgoing.
    ///
    /// This call performs matching and if a match is found, returns the _local_ PT
    /// that can be used for sending media.
    pub fn match_params(&self, params: PayloadParams) -> Option<&PayloadParams> {
        let c = self.params.iter().max_by_key(|p| p.match_score(&params))?;
        c.match_score(&params)?; // avoid None, which isn't a match.
        Some(c)
    }

    /// When we get remote payload parameters, we need to match differently depending on direction.
    pub(crate) fn sdp_match_remote(
        &self,
        remote_params: PayloadParams,
        remote_dir: Direction,
    ) -> Option<Pt> {
        // If we have no matching parameters locally, we can't accept the remote in any way.
        let our_params = self.match_params(remote_params)?;

        if remote_dir.sdp_is_receiving() {
            // The remote is talking about its own receive requirements. The PTs are not suggestions.
            Some(remote_params.pt())
        } else {
            // We can override the remote with our local config. We would have adjusted to
            // the remote earlier if we can.
            Some(our_params.pt())
        }
    }

    /// Find a payload parameter using a finder function.
    pub fn find(&self, mut f: impl FnMut(&PayloadParams) -> bool) -> Option<&PayloadParams> {
        self.params.iter().find(move |p| f(p))
    }

    /// Return configured payloads for a media kind, optionally preferring event
    /// clock rates that match audio codecs in a new local offer.
    pub(crate) fn all_for_kind(
        &self,
        kind: MediaKind,
        for_new_offer: bool,
    ) -> impl Iterator<Item = &PayloadParams> {
        let matches_audio_clock = move |event: &PayloadParams| {
            self.params.iter().any(|audio| {
                audio.spec.codec.is_audio()
                    && audio.spec.rtp_clock_rate() == event.spec.rtp_clock_rate()
            })
        };
        let has_matching_events = self
            .params
            .iter()
            .any(|p| p.spec.codec.is_tele() && matches_audio_clock(p));

        self.params.iter().filter(move |p| {
            if kind == MediaKind::Video {
                p.spec.codec.is_video()
            } else {
                p.spec.codec.is_audio()
                    || (p.spec.codec.is_tele()
                        && (!for_new_offer || !has_matching_events || matches_audio_clock(p)))
            }
        })
    }

    /// Highest telephone-event code for this payload type.
    ///
    /// Before SDP negotiation this is the configured range (default `0-16`).
    /// After negotiation it is the smaller range advertised by both peers,
    /// shared across all m-lines using the payload type. Returns `None` if
    /// the payload type is not configured as telephone-event.
    pub(crate) fn tele_event_max(&self, pt: Pt) -> Option<u8> {
        self.params
            .iter()
            .find(|p| p.pt() == pt && p.spec.codec.is_tele())
            .map(|p| {
                p.spec
                    .format
                    .telephone_event_max
                    .unwrap_or(FormatParams::DEFAULT_TELEPHONE_EVENT_MAX)
            })
    }

    pub(crate) fn update_params(&mut self, remote_params: &[PayloadParams], remote_dir: Direction) {
        // 0-128 of "claimed" PTs. I.e. PTs that we already allocated to something.
        let mut claimed: [bool; 128] = [false; 128];

        // Make a pass with all that are definitely confirmed by the remote, since these can't change.
        for p in self.params.iter() {
            if !p.locked {
                continue;
            }

            claimed.assert_claim_once(p.pt);

            if let Some(rtx) = p.resend {
                claimed.assert_claim_once(rtx);
            }

            if let Some(red) = p.red {
                claimed.assert_claim_once(red);
            }
        }

        // Collect all currently unlocked PTs since we will need to avoid them when reassigning.
        let mut unlocked = self
            .params
            .iter()
            .filter(|p| !p.locked)
            .flat_map(|p| [Some(p.pt()), p.resend(), p.red()])
            .flatten()
            .collect::<HashSet<_>>();

        // Now lock potential new parameters to remote.
        //
        // If the remote is doing `SendOnly`, we are receiving, so the PTs are suggestions,
        // and we are allowed to ANSWER with our own allocations as overrides.
        // If SendRecv or RecvOnly, the remote is talking about its own receiving capabilities
        // and we are not allowed to change it in the ANSWER.
        let local_is_controlling = !remote_dir.sdp_is_receiving();

        for p in self.params.iter_mut() {
            p.update_param(
                remote_params,
                &mut claimed,
                local_is_controlling,
                &mut unlocked,
            );
        }

        // Make a pass to reassign unconfirmed payloads that have PT which are now claimed.
        for p in self.params.iter_mut() {
            if p.locked {
                continue;
            }

            if claimed.is_claimed(p.pt) {
                let Some(pt) = claimed.find_unclaimed(PREFERED_RANGES, &unlocked) else {
                    // TODO: handle this gracefully.
                    panic!("Exhausted all PT ranges, inconsistent PayloadParam state");
                };

                debug!("Reassigned PT {} => {}", p.pt, pt);
                p.pt = pt;

                claimed.assert_claim_once(pt);
                unlocked.remove(&pt);
            }

            // RED rides its own PT (like RTX); reassign it if it now collides.
            if let Some(red) = p.red {
                if claimed.is_claimed(red) {
                    match claimed.find_unclaimed(PREFERED_RANGES, &unlocked) {
                        Some(new_red) => {
                            debug!("Reassigned RED PT {:?} => {:?}", p.red, new_red);
                            p.red = Some(new_red);
                            claimed.assert_claim_once(new_red);
                            unlocked.remove(&new_red);
                        }
                        // RED is opt-in; drop it rather than panic if no PT is free.
                        None => {
                            debug!("No free PT for RED; dropping it");
                            p.red = None;
                        }
                    }
                }
            }

            let Some(rtx) = p.resend else {
                continue;
            };

            if claimed.is_claimed(rtx) {
                let Some(rtx) = claimed.find_unclaimed(PREFERED_RANGES, &unlocked) else {
                    // TODO: handle this gracefully.
                    panic!("Exhausted all PT ranges, inconsistent PayloadParam state");
                };

                debug!("Reassigned RTX PT {:?} => {:?}", p.resend, rtx);
                p.resend = Some(rtx);

                claimed.assert_claim_once(rtx);
                unlocked.remove(&rtx);
            }
        }
    }

    pub(crate) fn has_pt(&self, pt: Pt) -> bool {
        self.params.iter().any(|p| p.pt() == pt)
    }
}

impl Deref for CodecConfig {
    type Target = [PayloadParams];

    fn deref(&self) -> &Self::Target {
        &self.params
    }
}

impl DerefMut for CodecConfig {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.params
    }
}

impl CodecSpec {
    /// Get the standardized codec definition for a static RTP payload type.
    pub(crate) fn from_static_pt(pt: Pt) -> Option<Self> {
        let (codec, clock_rate) = if pt == PT_PCMU {
            (Codec::PCMU, Frequency::EIGHT_KHZ)
        } else if pt == PT_PCMA {
            (Codec::PCMA, Frequency::EIGHT_KHZ)
        } else if pt == PT_G722 {
            // G722 is represented as 16 kHz internally. CodecSpec::rtp_clock_rate()
            // maps it to the standardized 8 kHz RTP clock rate on the wire.
            (Codec::G722, Frequency::SIXTEEN_KHZ)
        } else if pt == PT_COMFORT_NOISE {
            (Codec::CN, Frequency::EIGHT_KHZ)
        } else {
            return None;
        };

        Some(CodecSpec {
            codec,
            clock_rate,
            channels: None,
            format: FormatParams::default(),
        })
    }
}

#[cfg(test)]
mod test {
    use crate::io::MultiplexKind;
    use crate::packet::MediaKind;
    use crate::rtp_::Direction;
    use crate::rtp_::Frequency;

    use super::*;
    use crate::format::{CodecSpec, FormatParams};

    #[test]
    fn transport_cc_uses_first_section_and_preserves_local_capability() {
        for direction in [Direction::SendOnly, Direction::RecvOnly] {
            for local_enabled in [false, true] {
                for sections in [[false, false], [false, true], [true, false]] {
                    let mut config = CodecConfig::empty();
                    config.enable_vp8(true);
                    config.params[0].set_fb_transport_cc(local_enabled);
                    let mut remote = config.params[0];
                    let expected = local_enabled && sections[0];
                    for enabled in sections {
                        remote.set_fb_transport_cc(enabled);
                        config.update_params(&[remote], direction);
                        assert_eq!(config.params()[0].fb_transport_cc(), expected);
                    }
                }
            }
        }
    }

    #[test]
    fn static_payload_types_have_canonical_codec_definitions() {
        let definitions = [
            (PT_PCMU, Codec::PCMU, Frequency::EIGHT_KHZ),
            (PT_PCMA, Codec::PCMA, Frequency::EIGHT_KHZ),
            (PT_G722, Codec::G722, Frequency::SIXTEEN_KHZ),
            (PT_COMFORT_NOISE, Codec::CN, Frequency::EIGHT_KHZ),
        ];

        for (pt, codec, clock_rate) in definitions {
            let spec = CodecSpec::from_static_pt(pt).expect("static payload definition");
            assert_eq!(spec.codec, codec);
            assert_eq!(spec.clock_rate, clock_rate);
            assert_eq!(spec.channels, None);
            assert_eq!(spec.format, FormatParams::default());
        }

        assert!(CodecSpec::from_static_pt(PT_OPUS).is_none());
    }

    #[test]
    fn default_red_payload_types_are_demultiplexed_as_rtp() {
        for pt in [PT_G722_RED, PT_PCMU_RED, PT_PCMA_RED] {
            let packet = [0x80, *pt, 0];
            assert_eq!(
                MultiplexKind::try_from(packet.as_slice()).unwrap(),
                MultiplexKind::Rtp,
                "RED payload type {pt} must not be demultiplexed as RTCP"
            );
        }
    }

    #[test]
    fn dynamic_comfort_noise_collision_preserves_configured_pt() {
        let mut claimed = PayloadParams::new(
            96.into(),
            None,
            CodecSpec {
                codec: Codec::Opus,
                clock_rate: Frequency::FORTY_EIGHT_KHZ,
                channels: Some(2),
                format: FormatParams::default(),
            },
        );
        claimed.locked = true;

        let local_cn = PayloadParams::new(
            97.into(),
            None,
            CodecSpec {
                codec: Codec::CN,
                clock_rate: Frequency::SIXTEEN_KHZ,
                channels: None,
                format: FormatParams::default(),
            },
        );
        let remote_cn = PayloadParams::new(
            96.into(),
            None,
            CodecSpec {
                codec: Codec::CN,
                clock_rate: Frequency::SIXTEEN_KHZ,
                channels: None,
                format: FormatParams::default(),
            },
        );

        let mut config = CodecConfig::new_from_payload_params(vec![claimed, local_cn]);
        config.update_params(&[remote_cn], Direction::SendOnly);

        let cn = config
            .iter()
            .find(|p| p.spec().codec == Codec::CN)
            .expect("configured CN payload");
        assert_eq!(
            *cn.pt(),
            97,
            "dynamic CN must retain its configured payload type"
        );
    }

    #[test]
    fn collision_preserves_configured_primary_and_rtx_pts() {
        let mut claimed = PayloadParams::new(
            96.into(),
            Some(97.into()),
            CodecSpec {
                codec: Codec::H264,
                clock_rate: Frequency::NINETY_KHZ,
                channels: None,
                format: FormatParams::default(),
            },
        );
        claimed.locked = true;

        let vp8 = CodecSpec {
            codec: Codec::Vp8,
            clock_rate: Frequency::NINETY_KHZ,
            channels: None,
            format: FormatParams::default(),
        };
        let local_vp8 = PayloadParams::new(110.into(), Some(111.into()), vp8);
        let remote_vp8 = PayloadParams::new(96.into(), Some(97.into()), vp8);

        let mut config = CodecConfig::new_from_payload_params(vec![claimed, local_vp8]);
        config.update_params(&[remote_vp8], Direction::SendOnly);

        let vp8 = config
            .iter()
            .find(|p| p.spec().codec == Codec::Vp8)
            .expect("configured VP8 payload");
        assert_eq!((*vp8.pt(), vp8.resend().map(|pt| *pt)), (110, Some(111)));
    }

    #[test]
    fn enable_comfort_noise_uses_static_8khz_payload_type() {
        let mut config = CodecConfig::empty();

        config.enable_comfort_noise(true);

        let params = config
            .params()
            .iter()
            .find(|p| p.spec().codec == Codec::CN)
            .expect("Comfort Noise should be enabled");
        assert_eq!(params.pt(), Pt::new_with_value(13));
        assert_eq!(params.spec().clock_rate, Frequency::EIGHT_KHZ);
        assert_eq!(params.spec().channels, None);

        config.enable_comfort_noise(false);
        assert!(config.params().iter().all(|p| p.spec().codec != Codec::CN));
    }

    #[test]
    fn comfort_noise_supports_dynamic_payload_types_at_higher_clock_rates() {
        let mut config = CodecConfig::empty();

        for (pt, clock_rate) in [(96, 16_000), (97, 24_000), (98, 32_000), (99, 48_000)] {
            config.add_config(
                pt.into(),
                None,
                Codec::CN,
                Frequency::new(clock_rate).unwrap(),
                None,
                FormatParams::default(),
            );
        }

        let configured: Vec<_> = config
            .params()
            .iter()
            .map(|p| (*p.pt(), p.spec().clock_rate.get()))
            .collect();
        assert_eq!(
            configured,
            vec![(96, 16_000), (97, 24_000), (98, 32_000), (99, 48_000)]
        );
    }

    #[test]
    fn enable_red_links_opus() {
        let mut c = CodecConfig::empty();
        c.enable_opus(true, true);
        let opus = c
            .params()
            .iter()
            .find(|p| p.spec().codec == Codec::Opus)
            .unwrap();
        assert_eq!(opus.red(), Some(PT_RED));
    }

    #[test]
    fn enable_red_links_all_audio_codecs_with_distinct_pts() {
        let mut c = CodecConfig::empty();
        c.enable_opus(true, true);
        c.enable_g722(true, true);
        c.enable_pcmu(true, true);
        c.enable_pcma(true, true);

        let params = c.params();
        let red_of = |codec| {
            params
                .iter()
                .find(|p| p.spec().codec == codec)
                .unwrap()
                .red()
        };

        // Every enabled audio codec gets RED, each with its own distinct payload type so the
        // `a=rtpmap` entries (which each carry that codec's clock rate) don't collide in SDP.
        assert_eq!(red_of(Codec::Opus), Some(PT_RED));
        assert_eq!(red_of(Codec::G722), Some(PT_G722_RED));
        assert_eq!(red_of(Codec::PCMU), Some(PT_PCMU_RED));
        assert_eq!(red_of(Codec::PCMA), Some(PT_PCMA_RED));
        // All four RED payload types are distinct.
        let pts = [PT_RED, PT_G722_RED, PT_PCMU_RED, PT_PCMA_RED];
        for (i, a) in pts.iter().enumerate() {
            for b in &pts[i + 1..] {
                assert_ne!(a, b, "RED payload types must be distinct");
            }
        }
    }

    #[test]
    fn enable_amr_wb_uses_a_free_default_pt() {
        let mut config = CodecConfig::new_with_defaults();

        config.enable_amr_wb(true);

        let amr_wb = config
            .params()
            .iter()
            .find(|p| p.spec().codec == Codec::AmrWb)
            .expect("AMR-WB should be enabled");
        assert_eq!(amr_wb.pt(), PT_AMR_WB);

        let mut seen = std::collections::HashSet::new();
        for pt in config
            .params()
            .iter()
            .flat_map(|p| [Some(p.pt()), p.resend(), p.red()])
            .flatten()
        {
            assert!(seen.insert(pt), "duplicate PT {pt}");
        }
    }

    #[test]
    fn set_red_distances_sanitises_input() {
        let mut c = CodecConfig::empty();
        // Default is a single level, the previous packet.
        assert_eq!(c.red_distances(), &[1]);

        // Unsorted, duplicated, zero, and over-cap values are cleaned to a sorted distinct set.
        c.set_red_distances(&[5, 1, 0, 3, 3, 1, 999]);
        assert_eq!(c.red_distances(), &[1, 3, 5]);

        // Empty or fully invalid input falls back to the default single level.
        c.set_red_distances(&[0, 999]);
        assert_eq!(c.red_distances(), &[1]);

        // Distance 1 is always included as the recovery anchor: a pattern without it (which the
        // receiver would mis-map) gets 1 prepended rather than mis-recovered.
        c.set_red_distances(&[2, 4]);
        assert_eq!(c.red_distances(), &[1, 2, 4]);
        c.set_red_distances(&[3, 5]);
        assert_eq!(c.red_distances(), &[1, 3, 5]);
    }

    #[test]
    fn red_kept_when_both_sides_enable() {
        let mut local = CodecConfig::empty();
        local.enable_opus(true, true);

        let mut remote = CodecConfig::empty();
        remote.enable_opus(true, true);

        local.update_params(remote.params(), Direction::SendRecv);

        let opus = local
            .params()
            .iter()
            .find(|p| p.spec().codec == Codec::Opus)
            .unwrap();
        assert_eq!(opus.red(), Some(PT_RED));
    }

    #[test]
    fn red_dropped_when_remote_lacks_red() {
        let mut local = CodecConfig::empty();
        local.enable_opus(true, true);

        let mut remote = CodecConfig::empty();
        remote.enable_opus(true, false); // remote does not offer RED

        local.update_params(remote.params(), Direction::SendRecv);

        let opus = local
            .params()
            .iter()
            .find(|p| p.spec().codec == Codec::Opus)
            .unwrap();
        assert_eq!(
            opus.red(),
            None,
            "RED must be dropped when remote doesn't offer it"
        );
    }

    #[test]
    fn conflicting_remote_red_mappings_do_not_panic() {
        let shared_red_pt = Pt::new_with_value(63);

        let mut local = CodecConfig::empty();
        local.enable_g722(true, true);
        local.enable_pcmu(true, true);

        let mut remote = CodecConfig::empty();
        remote.enable_g722(true, true);
        remote.enable_pcmu(true, true);
        for p in remote.params.iter_mut() {
            p.red = Some(shared_red_pt);
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            local.update_params(remote.params(), Direction::SendRecv);
        }));

        assert!(
            result.is_ok(),
            "a conflicting remote RED mapping must be rejected or negotiated without panicking"
        );
    }

    #[test]
    fn test_pt_conflict_different_directions() {
        // Simulates:
        // 1. str0m OFFER sendonly H264 PT 108, RTX PT 109
        // 2. FF ANSWER recvonly (locks PT 109 as H264 RTX)
        // 3. FF OFFER sendonly Opus PT 109
        // 4. Crash on PT 109 conflict

        let mut config = CodecConfig::empty();

        // Initial local config: H264 with PT 108, RTX 109 and Opus with PT 111
        config.add_config(
            108.into(),
            Some(109.into()),
            Codec::H264,
            Frequency::NINETY_KHZ,
            None,
            FormatParams {
                packetization_mode: Some(1),
                profile_level_id: Some(0x42e01f),
                ..Default::default()
            },
        );
        config.add_config(
            111.into(),
            None,
            Codec::Opus,
            Frequency::FORTY_EIGHT_KHZ,
            Some(2),
            FormatParams::default(),
        );

        // Step 2: Remote answers recvonly with our H264 PT 108/109
        // This locks PT 109 as RTX
        let remote_h264_params = vec![PayloadParams::new(
            108.into(),
            Some(109.into()),
            CodecSpec {
                codec: Codec::H264,
                clock_rate: Frequency::NINETY_KHZ,
                channels: None,
                format: FormatParams {
                    packetization_mode: Some(1),
                    profile_level_id: Some(0x42e01f),
                    ..Default::default()
                },
            },
        )];

        config.update_params(&remote_h264_params, Direction::RecvOnly);

        // Verify PT 109 is now locked as RTX for H264
        assert!(
            config
                .params()
                .iter()
                .any(|p| p.resend() == Some(109.into()) && p.locked)
        );

        // Step 3: Remote offers sendonly Opus PT 109
        // This should NOT crash - PT 109 should be remapped to 111 (Opus default)
        let remote_opus_params = vec![PayloadParams::new(
            109.into(),
            None,
            CodecSpec {
                codec: Codec::Opus,
                clock_rate: Frequency::FORTY_EIGHT_KHZ,
                channels: Some(2),
                format: FormatParams::default(),
            },
        )];

        config.update_params(&remote_opus_params, Direction::SendOnly);

        // Verify the fix: Opus should be remapped to PT_OPUS (its default)
        let opus_param = config
            .params()
            .iter()
            .find(|p| p.spec().codec == Codec::Opus);
        assert!(opus_param.is_some(), "Opus param should exist");
        assert_eq!(
            opus_param.unwrap().pt(),
            PT_OPUS,
            "Opus should be remapped to PT_OPUS (default)"
        );
        assert!(opus_param.unwrap().locked, "Opus param should be locked");

        // Verify H264 RTX PT 109 is still locked
        assert!(
            config
                .params()
                .iter()
                .any(|p| p.resend() == Some(109.into()) && p.locked)
        );
    }

    #[test]
    fn test_pt_conflict_same_mline_multiple_codecs() {
        // Reproduces a bug we have had where reassigned PTs were conflicting with unlocked PTs.
        //
        // SCENARIO & ROOT CAUSE:
        // This test reproduces the exact bug from sdp.json using str0m's default config.
        //
        // 1. str0m starts with DEFAULT codec configuration:
        //    - VP8 at PT 96/97 (unlocked)
        //    - H264 High (0x64001f) at PT 114/115 (unlocked)
        //    - Many other H264 profiles at various PTs (127, 125, 108, 124, 123, 35)
        //
        // 2. Chrome (peer A) offers video with H264 only - PTs: 103/104, 109/114, 117/118
        //    Chrome's OFFER direction: sendonly (Chrome sends video to us)
        //    **CRITICAL**: Chrome uses PT 114 as RTX for H264 42e01f (PT 109)
        //
        // 3. str0m ANSWERs accepting Chrome's H264 offer:
        //    - Chrome's H264 PTs (103, 109, 117) are LOCKED
        //    - Chrome's RTX PTs (104, 114, 118) are LOCKED
        //    - VP8 at PT 96 remains UNLOCKED (Chrome didn't negotiate it)
        //    - H264 High at PT 114 CONFLICTS with Chrome's RTX usage of PT 114!
        //
        // 4. THE BUG (before fix): update_params reassigns H264 High from PT 114:
        //    - Searches for available PT starting from 96
        //    - PT 96 appears "unclaimed" because it only checked LOCKED PTs
        //    - VP8 is at PT 96 but UNLOCKED, so wasn't checked!
        //    - H264 High gets reassigned to PT 96
        //    - Now BOTH VP8 and H264 High are at PT 96! ❌
        //
        // 5. Later, Firefox (peer B) joins - str0m creates NEW video m-line (mid KvE)
        //    When generating SDP, str0m collects ALL video codecs from codec_config
        //    Result (before fix): PT 96 appears TWICE in the same m-line!
        //    - Line 729 in sdp.json: a=rtpmap:96 VP8/90000
        //    - Line 791 in sdp.json: a=rtpmap:96 H264/90000 profile-level-id=64001f
        //
        // EXPECTED: Each PT in an m-line must be unique
        // THE FIX: Now checks BOTH locked and unlocked PTs when reassigning

        // Start with str0m's default codec configuration
        // This includes VP8 at 96/97 and H264 High at 114/115 (among others)
        let mut config = CodecConfig::new_with_defaults();

        // Verify H264 High starts at its default PT 114 BEFORE Chrome's offer
        let h264_high_before = config
            .params()
            .iter()
            .find(|p| {
                p.spec().codec == Codec::H264 && p.spec().format.profile_level_id == Some(0x64001f)
            })
            .expect("H264 High should exist");
        assert_eq!(
            h264_high_before.pt(),
            114.into(),
            "H264 High should start at PT 114"
        );

        // Simulate Chrome's OFFER with H264 only (sendonly from Chrome's perspective)
        // Chrome is sending video to us, so we're receiving
        // Direction::SendOnly means the remote is sending (we receive)
        let chrome_h264_params = vec![
            PayloadParams::new(
                103.into(),
                Some(104.into()),
                CodecSpec {
                    codec: Codec::H264,
                    clock_rate: Frequency::NINETY_KHZ,
                    channels: None,
                    format: FormatParams {
                        packetization_mode: Some(1),
                        profile_level_id: Some(0x42001f),
                        ..Default::default()
                    },
                },
            ),
            PayloadParams::new(
                109.into(),
                Some(114.into()),
                CodecSpec {
                    codec: Codec::H264,
                    clock_rate: Frequency::NINETY_KHZ,
                    channels: None,
                    format: FormatParams {
                        packetization_mode: Some(1),
                        profile_level_id: Some(0x42e01f),
                        ..Default::default()
                    },
                },
            ),
            PayloadParams::new(
                117.into(),
                Some(118.into()),
                CodecSpec {
                    codec: Codec::H264,
                    clock_rate: Frequency::NINETY_KHZ,
                    channels: None,
                    format: FormatParams {
                        packetization_mode: Some(1),
                        profile_level_id: Some(0x4d001f),
                        ..Default::default()
                    },
                },
            ),
        ];

        // Process Chrome's offer - this locks the H264 PTs that Chrome offered
        // Chrome is SendOnly, so from our perspective we're receiving (recvonly)
        config.update_params(&chrome_h264_params, Direction::SendOnly);

        // Verify: Chrome's H264 profiles are locked at their specific PTs
        assert!(
            config.params().iter().any(|p| p.pt() == 103.into()
                && p.spec().codec == Codec::H264
                && p.spec().format.profile_level_id == Some(0x42001f)
                && p.locked),
            "H264 42001f should be locked at PT 103"
        );
        assert!(
            config.params().iter().any(|p| p.pt() == 109.into()
                && p.spec().codec == Codec::H264
                && p.spec().format.profile_level_id == Some(0x42e01f)
                && p.locked),
            "H264 42e01f should be locked at PT 109"
        );
        assert!(
            config.params().iter().any(|p| p.pt() == 117.into()
                && p.spec().codec == Codec::H264
                && p.spec().format.profile_level_id == Some(0x4d001f)
                && p.locked),
            "H264 4d001f should be locked at PT 117"
        );

        // Verify: VP8 at PT 96 is NOT locked (Chrome didn't negotiate it)
        let vp8_param = config
            .params()
            .iter()
            .find(|p| p.spec().codec == Codec::Vp8);
        assert!(vp8_param.is_some(), "VP8 should still exist");
        assert_eq!(vp8_param.unwrap().pt(), 96.into(), "VP8 should be at PT 96");
        assert!(!vp8_param.unwrap().locked, "VP8 should NOT be locked");

        // THE FIX VERIFICATION:
        // H264 64001f was originally at PT 114, but Chrome used PT 114 for RTX
        // update_params should reassign H264 64001f to a PT that avoids BOTH:
        // - Locked PTs (103, 104, 109, 114, 117, 118)
        // - Unlocked PTs already in use (96 for VP8, 97 for VP8 RTX)
        let h264_high_param = config.params().iter().find(|p| {
            p.spec().codec == Codec::H264 && p.spec().format.profile_level_id == Some(0x64001f)
        });
        assert!(h264_high_param.is_some(), "H264 64001f should still exist");
        let h264_pt = h264_high_param.unwrap().pt();

        // Should NOT be at PT 96 (VP8 is there)
        assert_ne!(
            h264_pt,
            96.into(),
            "FIX WORKING: H264 64001f should NOT be at PT 96 (VP8 is there)"
        );

        // Should NOT be at any of the locked PTs
        assert!(
            ![103, 104, 109, 114, 117, 118].contains(&{ *h264_pt }),
            "H264 64001f should not conflict with locked PTs"
        );

        assert!(
            !h264_high_param.unwrap().locked,
            "H264 64001f should NOT be locked (Chrome didn't offer it)"
        );

        // Now simulate creating a new video m-line (for Firefox's camera)
        // This is where the bug manifests: we collect ALL video codecs from config
        let all_video_params: Vec<_> = config.all_for_kind(MediaKind::Video, false).collect();

        // Extract just the PTs (including RTX PTs)
        let mut all_pts = Vec::new();
        for p in all_video_params {
            all_pts.push(p.pt());
            if let Some(rtx) = p.resend() {
                all_pts.push(rtx);
            }
        }

        // THE FIX: With the bug fixed, no PT should appear multiple times
        // Check for duplicate PTs - this should now PASS
        let mut seen = std::collections::HashSet::new();
        let mut duplicates = Vec::new();
        for pt in &all_pts {
            if !seen.insert(pt) {
                duplicates.push(*pt);
            }
        }

        assert!(
            duplicates.is_empty(),
            "FAILED: Found duplicate PTs in same m-line: {:?}\nAll PTs: {:?}\n
            This creates invalid SDP with multiple a=rtpmap lines for the same PT!",
            duplicates,
            all_pts
        );
    }

    // ── H.265 direction-based negotiation tests ──────────────────────────

    /// Helper: build a CodecConfig containing only H.265 at the given PTL.
    fn h265_config(profile_id: u8, tier_flag: u8, level_id: u8) -> CodecConfig {
        let mut config = CodecConfig::empty();
        config.add_h265(
            102.into(),
            Some(103.into()),
            profile_id,
            tier_flag,
            level_id,
        );
        config
    }

    /// Helper: build a single-element remote PayloadParams for H.265 with full PTL.
    fn h265_remote_ptl(profile_id: u8, tier_flag: u8, level_id: u8) -> Vec<PayloadParams> {
        use crate::packet::H265ProfileTierLevel;
        vec![PayloadParams::new(
            102.into(),
            Some(103.into()),
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
            },
        )]
    }

    /// Helper: build a single-element remote PayloadParams for H.265 with profile-id only.
    fn h265_remote_profile_only(profile_id: u32) -> Vec<PayloadParams> {
        vec![PayloadParams::new(
            102.into(),
            Some(103.into()),
            CodecSpec {
                codec: Codec::H265,
                clock_rate: Frequency::NINETY_KHZ,
                channels: None,
                format: FormatParams {
                    profile_id: Some(profile_id),
                    ..Default::default()
                },
            },
        )]
    }

    /// Helper: build a single-element remote PayloadParams for H.265 with no fmtp.
    fn h265_remote_no_fmtp() -> Vec<PayloadParams> {
        vec![PayloadParams::new(
            102.into(),
            Some(103.into()),
            CodecSpec {
                codec: Codec::H265,
                clock_rate: Frequency::NINETY_KHZ,
                channels: None,
                format: FormatParams::default(),
            },
        )]
    }

    /// Helper: extract the H.265 param from a config (panics if missing).
    fn get_h265(config: &CodecConfig) -> &PayloadParams {
        config
            .params()
            .iter()
            .find(|p| p.spec().codec == Codec::H265)
            .expect("H.265 param missing")
    }

    // ── recvonly: remote sends to us ─────────────────────────────────────

    /// Remote sends at Level 5.2, we can receive at Level 6.0.
    /// Negotiated level should narrow to min(6.0, 5.2) = 5.2.
    #[test]
    fn test_h265_recvonly_remote_level_lower() {
        let mut config = h265_config(1, 0, 180); // Main, Main tier, Level 6.0
        let remote = h265_remote_ptl(1, 0, 156); // Level 5.2

        // Direction::SendOnly = remote is sending (we receive)
        config.update_params(&remote, Direction::SendOnly);

        let ptl = get_h265(&config)
            .spec()
            .format
            .h265_profile_tier_level
            .unwrap();
        assert_eq!(
            ptl.level_id(),
            156,
            "Should narrow to min(180,156) = Level 5.2"
        );
        assert_eq!(ptl.profile_id(), 1, "Profile should be preserved");
        assert_eq!(ptl.tier_flag(), 0, "Tier should be preserved");
    }

    /// Remote sends at Level 6.0, we can receive at Level 5.2.
    /// Negotiated level should narrow to min(5.2, 6.0) = 5.2.
    #[test]
    fn test_h265_recvonly_local_level_lower() {
        let mut config = h265_config(1, 0, 156); // Level 5.2
        let remote = h265_remote_ptl(1, 0, 180); // Level 6.0

        config.update_params(&remote, Direction::SendOnly);

        let ptl = get_h265(&config)
            .spec()
            .format
            .h265_profile_tier_level
            .unwrap();
        assert_eq!(
            ptl.level_id(),
            156,
            "Should narrow to min(156,180) = Level 5.2"
        );
    }

    // ── sendrecv: both sides send and receive ────────────────────────────

    /// Both sides sendrecv. Local Level 6.0, remote Level 5.2.
    /// Effective level should narrow to min(6.0, 5.2) = 5.2.
    #[test]
    fn test_h265_sendrecv_remote_level_lower() {
        let mut config = h265_config(1, 0, 180); // Level 6.0
        let remote = h265_remote_ptl(1, 0, 156); // Level 5.2

        config.update_params(&remote, Direction::SendRecv);

        let ptl = get_h265(&config)
            .spec()
            .format
            .h265_profile_tier_level
            .unwrap();
        assert_eq!(ptl.level_id(), 156, "sendrecv: should narrow to Level 5.2");
    }

    /// Both sides sendrecv. Local Level 5.2, remote Level 6.0.
    /// Effective level should narrow to min(5.2, 6.0) = 5.2.
    #[test]
    fn test_h265_sendrecv_local_level_lower() {
        let mut config = h265_config(1, 0, 156); // Level 5.2
        let remote = h265_remote_ptl(1, 0, 180); // Level 6.0

        config.update_params(&remote, Direction::SendRecv);

        let ptl = get_h265(&config)
            .spec()
            .format
            .h265_profile_tier_level
            .unwrap();
        assert_eq!(ptl.level_id(), 156, "sendrecv: should narrow to Level 5.2");
    }

    /// Same level on both sides. Should remain unchanged.
    #[test]
    fn test_h265_sendrecv_same_level() {
        let mut config = h265_config(1, 0, 156); // Level 5.2
        let remote = h265_remote_ptl(1, 0, 156); // Level 5.2

        config.update_params(&remote, Direction::SendRecv);

        let ptl = get_h265(&config)
            .spec()
            .format
            .h265_profile_tier_level
            .unwrap();
        assert_eq!(ptl.level_id(), 156, "Same levels should stay at Level 5.2");
    }

    // ── sendonly: we send, remote receives ───────────────────────────────

    /// Remote declares recvonly at Level 5.2, we offer sendonly at Level 6.0.
    /// Negotiated level should narrow to 5.2 (receiver wins).
    #[test]
    fn test_h265_sendonly_receiver_level_lower() {
        let mut config = h265_config(1, 0, 180); // Level 6.0
        let remote = h265_remote_ptl(1, 0, 156); // Level 5.2

        // Direction::RecvOnly = remote is receiving (we send)
        config.update_params(&remote, Direction::RecvOnly);

        let ptl = get_h265(&config)
            .spec()
            .format
            .h265_profile_tier_level
            .unwrap();
        assert_eq!(
            ptl.level_id(),
            156,
            "sendonly: should narrow to receiver's Level 5.2"
        );
    }

    /// Remote offers only profile-id=1, no tier/level.
    /// Local should drop its full PTL and echo back profile-id only.
    #[test]
    fn test_h265_profile_only_remote_mirrors() {
        let mut config = h265_config(1, 0, 180); // Full PTL
        let remote = h265_remote_profile_only(1); // profile-id only

        config.update_params(&remote, Direction::SendRecv);

        let h265 = get_h265(&config);
        assert!(
            h265.spec().format.h265_profile_tier_level.is_none(),
            "Full PTL should be cleared when remote only offers profile-id"
        );
        assert_eq!(
            h265.spec().format.profile_id,
            Some(1),
            "profile_id should mirror the remote's value"
        );
    }

    // ── no-fmtp remote ──────────────────────────────────────────────────

    /// Remote has no H.265 fmtp at all. Local should clear both fields.
    #[test]
    fn test_h265_no_fmtp_remote_clears() {
        let mut config = h265_config(1, 0, 180); // Full PTL
        let remote = h265_remote_no_fmtp();

        config.update_params(&remote, Direction::SendRecv);

        let h265 = get_h265(&config);
        assert!(
            h265.spec().format.h265_profile_tier_level.is_none(),
            "PTL should be cleared when remote has no fmtp"
        );
        assert!(
            h265.spec().format.profile_id.is_none(),
            "profile_id should be cleared when remote has no fmtp"
        );
    }

    // ── profile_id field cleanup ─────────────────────────────────────────

    /// When remote has full PTL, the `profile_id` field must be None
    /// to avoid duplicate serialization (profile-id appears in both
    /// h265_profile_tier_level AND the VP9-style profile_id field).
    #[test]
    fn test_h265_profile_id_cleared_on_ptl_match() {
        let mut config = CodecConfig::empty();
        // Artificially set both fields to simulate a misconfigured state
        config.add_config(
            102.into(),
            Some(103.into()),
            Codec::H265,
            Frequency::NINETY_KHZ,
            None,
            FormatParams {
                h265_profile_tier_level: Some(
                    crate::packet::H265ProfileTierLevel::new(1, 0, 180).unwrap(),
                ),
                profile_id: Some(1), // should be cleared after negotiation
                ..Default::default()
            },
        );
        let remote = h265_remote_ptl(1, 0, 156);

        config.update_params(&remote, Direction::SendRecv);

        let h265 = get_h265(&config);
        assert!(
            h265.spec().format.profile_id.is_none(),
            "profile_id field must be None when full PTL is present"
        );
        assert!(
            h265.spec().format.h265_profile_tier_level.is_some(),
            "PTL should still be present"
        );
    }

    // ── default level ────────────────────────────────────────────────────

    /// `enable_h265` should produce Level 6.0 (level_id = 180).
    #[test]
    fn test_h265_default_level_is_6_0() {
        let mut config = CodecConfig::empty();
        config.enable_h265(true);

        let h265 = get_h265(&config);
        let ptl = h265.spec().format.h265_profile_tier_level.unwrap();
        assert_eq!(
            ptl.level_id(),
            180,
            "Default H.265 level should be 6.0 (180)"
        );
        assert_eq!(
            ptl.profile_id(),
            1,
            "Default H.265 profile should be Main (1)"
        );
        assert_eq!(ptl.tier_flag(), 0, "Default H.265 tier should be Main (0)");
    }

    // H.266 direction-based negotiation tests.

    /// Helper: build a CodecConfig containing only H.266 at the given PTL.
    fn h266_config(profile_id: u8, tier_flag: u8, level_id: u8) -> CodecConfig {
        let mut config = CodecConfig::empty();
        config.add_h266(PT_H266, Some(PT_H266_RTX), profile_id, tier_flag, level_id);
        config
    }

    /// Helper: build a single-element remote PayloadParams for H.266 with full PTL.
    fn h266_remote_ptl(profile_id: u8, tier_flag: u8, level_id: u8) -> Vec<PayloadParams> {
        vec![PayloadParams::new(
            PT_H266,
            Some(PT_H266_RTX),
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
            },
        )]
    }

    /// Helper: build a single-element remote PayloadParams for H.266 with profile-id only.
    fn h266_remote_profile_only(profile_id: u32) -> Vec<PayloadParams> {
        vec![PayloadParams::new(
            PT_H266,
            Some(PT_H266_RTX),
            CodecSpec {
                codec: Codec::H266,
                clock_rate: Frequency::NINETY_KHZ,
                channels: None,
                format: FormatParams {
                    profile_id: Some(profile_id),
                    ..Default::default()
                },
            },
        )]
    }

    /// Helper: build a single-element remote PayloadParams for H.266 with no fmtp.
    fn h266_remote_no_fmtp() -> Vec<PayloadParams> {
        vec![PayloadParams::new(
            PT_H266,
            Some(PT_H266_RTX),
            CodecSpec {
                codec: Codec::H266,
                clock_rate: Frequency::NINETY_KHZ,
                channels: None,
                format: FormatParams::default(),
            },
        )]
    }

    /// Helper: extract the H.266 param from a config (panics if missing).
    fn get_h266(config: &CodecConfig) -> &PayloadParams {
        config
            .params()
            .iter()
            .find(|p| p.spec().codec == Codec::H266)
            .expect("H.266 param missing")
    }

    // recvonly: remote sends to us.

    /// Remote sends at Level 4.0, we can receive at Level 6.0.
    /// Negotiated level should narrow to min(6.0, 4.0) = 4.0.
    #[test]
    fn test_h266_recvonly_remote_level_lower() {
        let mut config = h266_config(1, 0, 96); // Main 10, Main tier, Level 6.0
        let remote = h266_remote_ptl(1, 0, 64); // Level 4.0

        // Direction::SendOnly = remote is sending (we receive)
        config.update_params(&remote, Direction::SendOnly);

        let ptl = get_h266(&config)
            .spec()
            .format
            .h266_profile_tier_level
            .unwrap();
        assert_eq!(
            ptl.level_id(),
            64,
            "Should narrow to min(96,64) = Level 4.0"
        );
        assert_eq!(ptl.profile_id(), 1, "Profile should be preserved");
        assert_eq!(ptl.tier_flag(), 0, "Tier should be preserved");
    }

    /// Remote sends at Level 6.0, we can receive at Level 4.0.
    /// Negotiated level should narrow to min(4.0, 6.0) = 4.0.
    #[test]
    fn test_h266_recvonly_local_level_lower() {
        let mut config = h266_config(1, 0, 64); // Level 4.0
        let remote = h266_remote_ptl(1, 0, 96); // Level 6.0

        config.update_params(&remote, Direction::SendOnly);

        let ptl = get_h266(&config)
            .spec()
            .format
            .h266_profile_tier_level
            .unwrap();
        assert_eq!(
            ptl.level_id(),
            64,
            "Should narrow to min(64,96) = Level 4.0"
        );
    }

    // sendrecv: both sides send and receive.

    /// Both sides sendrecv. Local Level 6.0, remote Level 4.0.
    /// Effective level should narrow to min(6.0, 4.0) = 4.0.
    #[test]
    fn test_h266_sendrecv_remote_level_lower() {
        let mut config = h266_config(1, 0, 96); // Level 6.0
        let remote = h266_remote_ptl(1, 0, 64); // Level 4.0

        config.update_params(&remote, Direction::SendRecv);

        let ptl = get_h266(&config)
            .spec()
            .format
            .h266_profile_tier_level
            .unwrap();
        assert_eq!(ptl.level_id(), 64, "sendrecv: should narrow to Level 4.0");
    }

    /// Both sides sendrecv. Local Level 4.0, remote Level 6.0.
    /// Effective level should narrow to min(4.0, 6.0) = 4.0.
    #[test]
    fn test_h266_sendrecv_local_level_lower() {
        let mut config = h266_config(1, 0, 64); // Level 4.0
        let remote = h266_remote_ptl(1, 0, 96); // Level 6.0

        config.update_params(&remote, Direction::SendRecv);

        let ptl = get_h266(&config)
            .spec()
            .format
            .h266_profile_tier_level
            .unwrap();
        assert_eq!(ptl.level_id(), 64, "sendrecv: should narrow to Level 4.0");
    }

    /// Same level on both sides. Should remain unchanged.
    #[test]
    fn test_h266_sendrecv_same_level() {
        let mut config = h266_config(1, 0, 64); // Level 4.0
        let remote = h266_remote_ptl(1, 0, 64); // Level 4.0

        config.update_params(&remote, Direction::SendRecv);

        let ptl = get_h266(&config)
            .spec()
            .format
            .h266_profile_tier_level
            .unwrap();
        assert_eq!(ptl.level_id(), 64, "Same levels should stay at Level 4.0");
    }

    // sendonly: we send, remote receives.

    /// Remote declares recvonly at Level 4.0, we offer sendonly at Level 6.0.
    /// Negotiated level should narrow to 4.0 (receiver wins).
    #[test]
    fn test_h266_sendonly_receiver_level_lower() {
        let mut config = h266_config(1, 0, 96); // Level 6.0
        let remote = h266_remote_ptl(1, 0, 64); // Level 4.0

        // Direction::RecvOnly = remote is receiving (we send)
        config.update_params(&remote, Direction::RecvOnly);

        let ptl = get_h266(&config)
            .spec()
            .format
            .h266_profile_tier_level
            .unwrap();
        assert_eq!(
            ptl.level_id(),
            64,
            "sendonly: should narrow to receiver's Level 4.0"
        );
    }

    /// Remote offers only profile-id=1, no tier/level.
    /// Local should drop its full PTL and echo back profile-id only.
    #[test]
    fn test_h266_profile_only_remote_mirrors() {
        let mut config = h266_config(1, 0, 96); // Full PTL
        let remote = h266_remote_profile_only(1); // profile-id only

        config.update_params(&remote, Direction::SendRecv);

        let h266 = get_h266(&config);
        assert!(
            h266.spec().format.h266_profile_tier_level.is_none(),
            "Full PTL should be cleared when remote only offers profile-id"
        );
        assert_eq!(
            h266.spec().format.profile_id,
            Some(1),
            "profile_id should mirror the remote's value"
        );
    }

    // no-fmtp remote.

    /// Remote has no H.266 fmtp at all. Local should clear both fields.
    #[test]
    fn test_h266_no_fmtp_remote_clears() {
        let mut config = h266_config(1, 0, 96); // Full PTL
        let remote = h266_remote_no_fmtp();

        config.update_params(&remote, Direction::SendRecv);

        let h266 = get_h266(&config);
        assert!(
            h266.spec().format.h266_profile_tier_level.is_none(),
            "PTL should be cleared when remote has no fmtp"
        );
        assert!(
            h266.spec().format.profile_id.is_none(),
            "profile_id should be cleared when remote has no fmtp"
        );
    }

    // profile_id field cleanup.

    /// When remote has full PTL, the `profile_id` field must be None
    /// to avoid duplicate serialization (profile-id appears in both
    /// h266_profile_tier_level AND the VP9-style profile_id field).
    #[test]
    fn test_h266_profile_id_cleared_on_ptl_match() {
        let mut config = CodecConfig::empty();
        // Artificially set both fields to simulate a misconfigured state
        config.add_config(
            PT_H266,
            Some(PT_H266_RTX),
            Codec::H266,
            Frequency::NINETY_KHZ,
            None,
            FormatParams {
                h266_profile_tier_level: Some(H266ProfileTierLevel::new(1, 0, 96).unwrap()),
                profile_id: Some(1), // should be cleared after negotiation
                ..Default::default()
            },
        );
        let remote = h266_remote_ptl(1, 0, 64);

        config.update_params(&remote, Direction::SendRecv);

        let h266 = get_h266(&config);
        assert!(
            h266.spec().format.profile_id.is_none(),
            "profile_id field must be None when full PTL is present"
        );
        assert!(
            h266.spec().format.h266_profile_tier_level.is_some(),
            "PTL should still be present"
        );
    }

    // H.266 default level.

    /// `enable_h266(true)` should add H.266 at Main 10 / Main tier / Level 6.0.
    #[test]
    fn test_h266_enable_default_level() {
        let mut config = CodecConfig::empty();
        config.enable_h266(true);

        let h266 = config
            .params()
            .iter()
            .find(|p| p.spec().codec == Codec::H266)
            .expect("H.266 param missing");
        let ptl = h266.spec().format.h266_profile_tier_level.unwrap();
        assert_eq!(
            ptl.profile_id(),
            1,
            "Default H.266 profile should be Main 10 (1)"
        );
        assert_eq!(ptl.tier_flag(), 0, "Default H.266 tier should be Main (0)");
        assert_eq!(ptl.level_id(), 96, "Default H.266 level should be 6.0 (96)");
    }
}
