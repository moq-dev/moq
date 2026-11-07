use serde::{Deserialize, Serialize};
use std::fmt;

use super::Codec;
use crate::sdp::FormatParam;

/// Codec specific format parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FormatParams {
    /// Opus specific parameter.
    ///
    /// The minimum duration of media represented by a packet.
    pub min_p_time: Option<u8>,

    /// Opus specific parameter.
    ///
    /// Decoder-side preference for stereo audio. Default is mono per
    /// RFC 7587 Section 7.1: <https://datatracker.ietf.org/doc/html/rfc7587#section-7.1>
    pub stereo: Option<bool>,

    /// Opus specific parameter.
    ///
    /// Sender-side stereo hint for Opus. When set, it signals the intent to
    /// transmit stereo from the sender, matching the symmetric m-line model.
    pub sprop_stereo: Option<bool>,

    /// Opus specific parameter.
    ///
    /// Specifies that the decoder can do Opus in-band FEC
    pub use_inband_fec: Option<bool>,

    /// Opus specific parameter.
    ///
    /// Specifies that the decoder prefers DTX (Discontinuous Transmission) such that
    /// the packet rate is greatly lowered during periods of silence.
    pub use_dtx: Option<bool>,

    /// Whether h264 sending media encoded at a different level in the offerer-to-answerer
    /// direction than the level in the answerer-to-offerer direction, is allowed.
    pub level_asymmetry_allowed: Option<bool>,

    /// What h264 packetization mode is used.
    ///
    /// * 0 - single nal.
    /// * 1 - STAP-A, FU-A is allowed. Non-interleaved.
    pub packetization_mode: Option<u8>,

    /// H264 profile level.
    ///
    /// * 42 00 1f - 4200=baseline (B)              1f=level 3.1
    /// * 42 e0 1f - 42e0=constrained baseline (CB) 1f=level 3.1
    /// * 4d 00 1f - 4d00=main (M)                  1f=level 3.1
    /// * 64 00 1f - 6400=high (H)                  1f=level 3.1
    pub profile_level_id: Option<u32>,

    /// VP9 profile id.
    pub profile_id: Option<u32>,

    /// AV1 profile.
    ///
    /// Indicates the highest AV1 profile that may have been used to generate
    /// the bitstream or that the receiver supports. The range of possible values
    /// is identical to the seq_profile syntax element specified in AV1. If the
    /// parameter is not present, it MUST be inferred to be 0 ("Main" profile).
    ///
    /// 0  8-bit or 10-bit 4:2:0
    /// 1  8-bit or 10-bit 4:4:4
    /// 2  8-bit or 10-bit 4:2:2
    /// 2  12-bit          4:2:0, 4:2:2, 4:4:4
    pub profile: Option<u8>,

    /// AV1 level-idx.
    ///
    /// Indicates the highest AV1 level that may have been used to generate the
    /// bitstream or that the receiver supports. The range of possible values
    /// is identical to the seq_level_idx syntax element specified in AV1. If
    /// the parameter is not present, it MUST be inferred to be 5 (level 3.1).
    pub level_idx: Option<u8>,

    /// AV1 tier.
    ///
    /// Indicates the highest tier that may have been used to generate the  bitstream
    /// or that the receiver supports. The range of possible values is identical
    /// to the seq_tier syntax element specified in AV1. If the parameter is not
    /// present, the tier MUST be inferred to be 0.
    pub tier: Option<u8>,

    /// H.265/HEVC profile, tier, and level.
    ///
    /// Contains:
    /// * profile: Main, Main10, Main Still Picture, etc.
    /// * tier: Main or High.
    /// * level: 3.1, 4.0, 5.0, etc.
    ///
    /// See ITU-T H.265 Annex A for complete definitions.
    pub h265_profile_tier_level: Option<crate::packet::H265ProfileTierLevel>,

    /// H.266/VVC profile, tier, and level.
    ///
    /// Contains:
    /// * profile: Main 10, Main 10 4:4:4, etc.
    /// * tier: Main or High.
    /// * level: 3.1, 4.0, 5.0, etc.
    ///
    /// See ITU-T H.266 Annex A and RFC 9328 §7.2 for complete definitions.
    pub h266_profile_tier_level: Option<crate::packet::H266ProfileTierLevel>,

    /// H.265 sprop-max-don-diff parameter (RFC 7798 §7.1).
    /// When > 0, DONL fields are included in H.265 RTP packets to support
    /// out-of-order NAL unit decoding.
    pub sprop_max_don_diff: Option<u16>,

    /// Highest event code in the inclusive `0-X` telephone-event SDP range.
    ///
    /// Only contiguous ranges starting at zero are supported. `None` uses the
    /// library default `0-16`. Negotiation takes the smaller maximum per payload
    /// type across the session.
    pub telephone_event_max: Option<u8>,

    /// AMR-WB `octet-align`. `Some(true)` selects the octet-aligned payload
    /// layout, otherwise bandwidth-efficient is used.
    pub octet_align: Option<bool>,

    /// AMR-WB `mode-set`, a bitmask of the allowed speech modes 0..=8.
    ///
    /// A mode set from either peer applies to both directions, so only write
    /// frames in these modes.
    pub mode_set: Option<u16>,
}

#[cfg(feature = "drv")]
crate::drv_identity_copy!(FormatParams);

impl FormatParams {
    pub(crate) const DEFAULT_TELEPHONE_EVENT_MAX: u8 = 16;

    pub(crate) fn from_sdp_fmtp<'a>(
        codec: Codec,
        mut lines: impl Iterator<Item = &'a [FormatParam]>,
    ) -> Option<Self> {
        let mut params = Self::default();
        if codec.is_tele() {
            let max = match lines.next() {
                None => Self::DEFAULT_TELEPHONE_EVENT_MAX,
                Some([FormatParam::BareRange(max) | FormatParam::TelephoneEvents(max)]) => *max,
                _ => return None,
            };
            if lines.next().is_some() {
                return None;
            }
            params.telephone_event_max = Some(max);
        } else {
            for line in lines {
                for param in line {
                    // Decline AMR-WB payload types that need features str0m lacks.
                    if codec == Codec::AmrWb && *param == FormatParam::AmrWbUnsupported {
                        return None;
                    }
                    if !matches!(param, FormatParam::TelephoneEvents(_)) {
                        params.set_param(param);
                    }
                }
            }
        }
        Some(params)
    }

    /// Parse an fmtp line to create a FormatParams.
    ///
    /// Example `minptime=10;useinbandfec=1`, or `0-16` for telephone events.
    pub fn parse_line(line: &str) -> Self {
        let mut p = FormatParams::default();
        let key_vals: Vec<_> = line
            .split(';')
            .filter_map(|pair| {
                let mut kv = pair.split('=');
                match (kv.next(), kv.next()) {
                    (Some(k), Some(v)) => Some((k.trim().to_string(), v.trim().to_string())),
                    (Some(value), None) => {
                        let value = value.trim();
                        let param = FormatParam::parse_telephone_events(value);
                        if param == FormatParam::Unknown && !value.is_empty() {
                            debug!("Ignoring unsupported fmtp value: {value}");
                        }
                        p.set_param(&param);
                        None
                    }
                    (None, _) => None,
                }
            })
            .collect();

        for param in FormatParam::parse_pairs(key_vals) {
            p.set_param(&param);
        }

        p
    }

    pub(crate) fn set_param(&mut self, param: &FormatParam) {
        use FormatParam::*;
        match param {
            MinPTime(v) => self.min_p_time = Some(*v),
            Stereo(v) => self.stereo = Some(*v),
            SpropStereo(v) => self.sprop_stereo = Some(*v),
            UseInbandFec(v) => self.use_inband_fec = Some(*v),
            UseDtx(v) => self.use_dtx = Some(*v),
            LevelAsymmetryAllowed(v) => self.level_asymmetry_allowed = Some(*v),
            PacketizationMode(v) => self.packetization_mode = Some(*v),
            ProfileLevelId(v) => self.profile_level_id = Some(*v),
            ProfileId(v) => self.profile_id = Some(*v),
            Profile(v) => self.profile = Some(*v),
            LevelIdx(v) => self.level_idx = Some(*v),
            Tier(v) => self.tier = Some(*v),
            H265ProfileTierLevel(v) => self.h265_profile_tier_level = Some(*v),
            H266ProfileTierLevel(v) => self.h266_profile_tier_level = Some(*v),
            SpropMaxDonDiff(v) => self.sprop_max_don_diff = Some(*v),
            TelephoneEvents(v) => self.telephone_event_max = Some(*v),
            BareRange(_) | BarePtList(_) => {}
            OctetAlign(v) => self.octet_align = Some(*v),
            ModeSet(v) => self.mode_set = Some(*v),
            AmrWbUnsupported => {}
            Apt(_) => {}
            Red(_) => {}
            Unknown => {}
        }
    }

    pub(crate) fn to_format_param(self) -> Vec<FormatParam> {
        use FormatParam::*;
        let mut r = Vec::with_capacity(5);

        if let Some(v) = self.min_p_time {
            r.push(MinPTime(v));
        }
        if let Some(v) = self.stereo {
            r.push(Stereo(v));
        }
        if let Some(v) = self.sprop_stereo {
            r.push(SpropStereo(v));
        }
        if let Some(v) = self.use_inband_fec {
            r.push(UseInbandFec(v));
        }
        if let Some(v) = self.use_dtx {
            r.push(UseDtx(v));
        }
        if let Some(v) = self.level_asymmetry_allowed {
            r.push(LevelAsymmetryAllowed(v));
        }
        if let Some(v) = self.packetization_mode {
            r.push(PacketizationMode(v));
        }
        if let Some(v) = self.profile_level_id {
            r.push(ProfileLevelId(v));
        }
        if let Some(v) = self.profile_id {
            r.push(ProfileId(v));
        }
        if let Some(v) = self.profile {
            r.push(Profile(v));
        }
        if let Some(v) = self.level_idx {
            r.push(LevelIdx(v));
        }
        if let Some(v) = self.tier {
            r.push(Tier(v));
        }
        // H.265 Profile/Tier/Level: Keep as composite, it serializes all three params
        if let Some(ptl) = self.h265_profile_tier_level {
            r.push(H265ProfileTierLevel(ptl));
        }
        // H.266 Profile/Tier/Level: serialized as one composite (all three fmtp params).
        if let Some(ptl) = self.h266_profile_tier_level {
            r.push(H266ProfileTierLevel(ptl));
        }
        if let Some(v) = self.sprop_max_don_diff {
            r.push(SpropMaxDonDiff(v));
        }
        if let Some(v) = self.telephone_event_max {
            r.push(TelephoneEvents(v));
        }
        if let Some(v) = self.octet_align {
            r.push(OctetAlign(v));
        }
        if let Some(v) = self.mode_set {
            r.push(ModeSet(v));
        }

        r
    }
}

impl fmt::Display for FormatParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self
            .to_format_param()
            .into_iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
            .join(";");
        write!(f, "{s}")
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn telephone_event_ranges_roundtrip() {
        for max in [0, 7, 15, 16, 255] {
            let value = format!("0-{max}");
            let params = FormatParams::parse_line(&value);
            assert_eq!(params.telephone_event_max, Some(max));
            assert_eq!(params.to_string(), value);
        }
        for value in ["", "0-", "0-256", "15-0", "1-15", "0-15,16", "0-+15"] {
            assert_eq!(
                FormatParam::parse_telephone_events(value),
                FormatParam::Unknown
            );
        }
    }

    #[test]
    fn sprop_max_don_diff_roundtrip() {
        // Test that sprop-max-don-diff parameter round-trips correctly
        let f = FormatParams {
            sprop_max_don_diff: Some(32),
            ..Default::default()
        };

        // Serialize to string
        let fmtp_str = f.to_string();
        assert_eq!(fmtp_str, "sprop-max-don-diff=32");

        // Parse back
        let parsed = FormatParams::parse_line(&fmtp_str);
        assert_eq!(parsed.sprop_max_don_diff, Some(32));
    }

    #[test]
    fn sprop_stereo_roundtrip() {
        let f = FormatParams {
            sprop_stereo: Some(true),
            ..Default::default()
        };

        let fmtp_str = f.to_string();
        assert_eq!(fmtp_str, "sprop-stereo=1");

        let parsed = FormatParams::parse_line(&fmtp_str);
        assert_eq!(parsed.sprop_stereo, Some(true));
    }

    #[test]
    fn h265_combined_params_roundtrip() {
        use crate::packet::H265ProfileTierLevel;

        // Test H.265 with both profile/tier/level and sprop-max-don-diff
        let ptl = H265ProfileTierLevel::new(1, 0, 93).unwrap();
        let f = FormatParams {
            h265_profile_tier_level: Some(ptl),
            sprop_max_don_diff: Some(32),
            ..Default::default()
        };

        // Serialize to string
        let fmtp_str = f.to_string();
        assert!(fmtp_str.contains("profile-id=1"));
        assert!(fmtp_str.contains("tier-flag=0"));
        assert!(fmtp_str.contains("level-id=93"));
        assert!(fmtp_str.contains("sprop-max-don-diff=32"));

        // Parse back
        let parsed = FormatParams::parse_line(&fmtp_str);
        assert_eq!(parsed.h265_profile_tier_level, Some(ptl));
        assert_eq!(parsed.sprop_max_don_diff, Some(32));
    }

    #[test]
    fn h266_combined_params_roundtrip() {
        use crate::packet::H266ProfileTierLevel;

        // Test H.266 with both profile/tier/level and sprop-max-don-diff
        let ptl = H266ProfileTierLevel::new(1, 0, 51).unwrap();
        let f = FormatParams {
            h266_profile_tier_level: Some(ptl),
            sprop_max_don_diff: Some(32),
            ..Default::default()
        };

        // Serialize to string
        let fmtp_str = f.to_string();
        assert!(fmtp_str.contains("profile-id=1"));
        assert!(fmtp_str.contains("tier-flag=0"));
        assert!(fmtp_str.contains("level-id=51"));
        assert!(fmtp_str.contains("sprop-max-don-diff=32"));

        // Parse back
        let parsed = FormatParams::parse_line(&fmtp_str);
        assert_eq!(parsed.h266_profile_tier_level, Some(ptl));
        assert_eq!(parsed.sprop_max_don_diff, Some(32));
    }

    #[test]
    fn amr_wb_fmtp() {
        let f = FormatParams::parse_line("octet-align=1; mode-set=0, 1,2;max-red=220");
        assert_eq!(f.to_string(), "octet-align=1;mode-set=0,1,2");
        assert_eq!(
            FormatParams::parse_line("mode-set=0,9"),
            FormatParams::default()
        );

        let declined = |codec, k: &str, v: &str| {
            let line = [FormatParam::parse(k, v)];
            FormatParams::from_sdp_fmtp(codec, [&line[..]].into_iter()).is_none()
        };
        for (k, v) in [
            ("crc", "1"),
            ("robust-sorting", "1"),
            ("interleaving", "30"),
            ("mode-change-period", "2"),
        ] {
            assert!(declined(Codec::AmrWb, k, v), "{k}={v}");
            assert!(!declined(Codec::Opus, k, v), "{k}={v}");
        }
        for (k, v) in [
            ("crc", "0"),
            ("robust-sorting", "0"),
            ("mode-change-period", "1"),
        ] {
            assert!(!declined(Codec::AmrWb, k, v), "{k}={v}");
        }
    }
}
