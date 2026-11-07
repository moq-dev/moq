use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// H.266/VVC profile, tier, and level combination.
///
/// Represents the three SDP fmtp parameters `profile-id`, `tier-flag`, and
/// `level-id` as defined in RFC 9328 §7.2 and ITU-T H.266 Annex A.
///
/// Note: the parameter *names* are identical to H.265 (RFC 7798), but the
/// value spaces differ. In particular the level-id encodings are disjoint:
/// H.265 uses `30 × level` (e.g. 3.1 → 93) while H.266 uses
/// `16 × major + 3 × minor` (e.g. 3.1 → 51), which is what allows SDP
/// parsing to tell the two codecs' fmtp lines apart.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct H266ProfileTierLevel {
    profile: H266Profile,
    tier: H266Tier,
    level: H266Level,
}

impl H266ProfileTierLevel {
    // Inferred values mandated by RFC 9328 §7.2 when parameters are
    // absent from SDP: profile-id => 1 (Main 10), tier-flag => 0,
    // level-id => 51 (Level 3.1).
    pub(crate) const FALLBACK: Self = Self {
        profile: H266Profile::Main10,
        tier: H266Tier::Main,
        level: H266Level::Level3_1,
    };

    /// Construct a new H266ProfileTierLevel from profile_id, tier_flag, and level_id.
    ///
    /// Returns `Some(Self)` only if the provided parameters identify valid values.
    pub(crate) fn new(profile_id: u8, tier_flag: u8, level_id: u8) -> Option<Self> {
        let profile = H266Profile::from_id(profile_id)?;
        let tier = H266Tier::from_flag(tier_flag)?;
        let level = H266Level::from_id(level_id)?;

        Some(Self {
            profile,
            tier,
            level,
        })
    }

    /// Parse H266ProfileTierLevel from SDP fmtp parameters.
    ///
    /// Expects keys: "profile-id", "tier-flag", "level-id"
    /// Returns None if any required parameter is missing or invalid.
    pub(crate) fn from_fmtp(params: &HashMap<String, String>) -> Option<Self> {
        let profile_id: u8 = params.get("profile-id")?.parse().ok()?;
        let tier_flag: u8 = params.get("tier-flag")?.parse().ok()?;
        let level_id: u8 = params.get("level-id")?.parse().ok()?;

        Self::new(profile_id, tier_flag, level_id)
    }

    /// Returns the H.266 profile (Main 10, Main 10 4:4:4, etc.).
    pub fn profile(&self) -> H266Profile {
        self.profile
    }

    /// Returns the H.266 tier (Main or High).
    pub fn tier(&self) -> H266Tier {
        self.tier
    }

    /// Returns the H.266 level (Level 3.1, Level 4.0, etc.).
    pub fn level(&self) -> H266Level {
        self.level
    }

    /// Returns a copy with the level replaced.
    pub fn with_level(self, level: H266Level) -> Self {
        Self { level, ..self }
    }

    /// Returns the numeric profile_id value for SDP serialization.
    pub fn profile_id(&self) -> u8 {
        self.profile.to_id()
    }

    /// Returns the numeric tier_flag value for SDP serialization.
    pub fn tier_flag(&self) -> u8 {
        self.tier.to_flag()
    }

    /// Returns the numeric level_id value for SDP serialization.
    pub fn level_id(&self) -> u8 {
        self.level.to_id()
    }
}

impl From<(u8, u8, u8)> for H266ProfileTierLevel {
    fn from(value: (u8, u8, u8)) -> Self {
        Self::new(value.0, value.1, value.2).unwrap_or(Self::FALLBACK)
    }
}

/// H.266 profile as defined in ITU-T H.266 Annex A
/// (`general_profile_idc` values, version 1 profiles).
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum H266Profile {
    /// Main 10 profile (profile_id=1).
    Main10,
    /// Multilayer Main 10 profile (profile_id=17).
    MultilayerMain10,
    /// Main 10 4:4:4 profile (profile_id=33).
    Main10_444,
    /// Multilayer Main 10 4:4:4 profile (profile_id=49).
    MultilayerMain10_444,
    /// Main 10 Still Picture profile (profile_id=65).
    Main10StillPicture,
    /// Multilayer Main 10 Still Picture profile (profile_id=81).
    MultilayerMain10StillPicture,
    /// Main 10 4:4:4 Still Picture profile (profile_id=97).
    Main10_444StillPicture,
    /// Multilayer Main 10 4:4:4 Still Picture profile (profile_id=113).
    MultilayerMain10_444StillPicture,
}

impl H266Profile {
    /// Convert from H.266 profile_id (general_profile_idc) value.
    ///
    /// See ITU-T H.266 Annex A for profile definitions.
    fn from_id(profile_id: u8) -> Option<Self> {
        match profile_id {
            1 => Some(Self::Main10),
            17 => Some(Self::MultilayerMain10),
            33 => Some(Self::Main10_444),
            49 => Some(Self::MultilayerMain10_444),
            65 => Some(Self::Main10StillPicture),
            81 => Some(Self::MultilayerMain10StillPicture),
            97 => Some(Self::Main10_444StillPicture),
            113 => Some(Self::MultilayerMain10_444StillPicture),
            _ => None,
        }
    }

    /// Convert to H.266 profile_id (general_profile_idc) value.
    pub fn to_id(self) -> u8 {
        match self {
            Self::Main10 => 1,
            Self::MultilayerMain10 => 17,
            Self::Main10_444 => 33,
            Self::MultilayerMain10_444 => 49,
            Self::Main10StillPicture => 65,
            Self::MultilayerMain10StillPicture => 81,
            Self::Main10_444StillPicture => 97,
            Self::MultilayerMain10_444StillPicture => 113,
        }
    }
}

/// H.266 tier (Main or High).
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum H266Tier {
    /// Main tier (tier_flag=0).
    Main,
    /// High tier (tier_flag=1).
    High,
}

impl H266Tier {
    /// Convert from H.266 tier_flag value.
    ///
    /// 0 = Main tier, 1 = High tier
    fn from_flag(tier_flag: u8) -> Option<Self> {
        match tier_flag {
            0 => Some(Self::Main),
            1 => Some(Self::High),
            _ => None,
        }
    }

    /// Convert to H.266 tier_flag value.
    pub fn to_flag(self) -> u8 {
        match self {
            Self::Main => 0,
            Self::High => 1,
        }
    }
}

/// H.266 level as defined in ITU-T H.266 Annex A.
///
/// Level IDs (`general_level_idc`) encode as `16 × major + 3 × minor`
/// (e.g., Level 3.1 = 16×3 + 3×1 = 51).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
#[rustfmt::skip]
pub enum H266Level {
    /// Level 1.0 (level_id=16).
    Level1   = 16_u8,
    /// Level 2.0 (level_id=32).
    Level2   = 32_u8,
    /// Level 2.1 (level_id=35).
    Level2_1 = 35_u8,
    /// Level 3.0 (level_id=48).
    Level3   = 48_u8,
    /// Level 3.1 (level_id=51).
    Level3_1 = 51_u8,
    /// Level 4.0 (level_id=64).
    Level4   = 64_u8,
    /// Level 4.1 (level_id=67).
    Level4_1 = 67_u8,
    /// Level 5.0 (level_id=80).
    Level5   = 80_u8,
    /// Level 5.1 (level_id=83).
    Level5_1 = 83_u8,
    /// Level 5.2 (level_id=86).
    Level5_2 = 86_u8,
    /// Level 6.0 (level_id=96).
    Level6   = 96_u8,
    /// Level 6.1 (level_id=99).
    Level6_1 = 99_u8,
    /// Level 6.2 (level_id=102).
    Level6_2 = 102_u8,
    /// Level 6.3 (level_id=105).
    Level6_3 = 105_u8,
    /// Level 15.5 (level_id=255) — the special maximum level used by the
    /// still-picture profiles (16*15 + 3*5 = 255).
    Level15_5 = 255_u8,
}

impl H266Level {
    /// Returns the ordinal position (0-14) representing capability order.
    pub fn ordinal(self) -> usize {
        match self {
            Self::Level1 => 0,
            Self::Level2 => 1,
            Self::Level2_1 => 2,
            Self::Level3 => 3,
            Self::Level3_1 => 4,
            Self::Level4 => 5,
            Self::Level4_1 => 6,
            Self::Level5 => 7,
            Self::Level5_1 => 8,
            Self::Level5_2 => 9,
            Self::Level6 => 10,
            Self::Level6_1 => 11,
            Self::Level6_2 => 12,
            Self::Level6_3 => 13,
            Self::Level15_5 => 14,
        }
    }

    /// Convert from H.266 level_id value.
    fn from_id(level_id: u8) -> Option<Self> {
        use H266Level::*;
        match level_id {
            x if (Level1 as u8) == x => Some(Level1),
            x if (Level2 as u8) == x => Some(Level2),
            x if (Level2_1 as u8) == x => Some(Level2_1),
            x if (Level3 as u8) == x => Some(Level3),
            x if (Level3_1 as u8) == x => Some(Level3_1),
            x if (Level4 as u8) == x => Some(Level4),
            x if (Level4_1 as u8) == x => Some(Level4_1),
            x if (Level5 as u8) == x => Some(Level5),
            x if (Level5_1 as u8) == x => Some(Level5_1),
            x if (Level5_2 as u8) == x => Some(Level5_2),
            x if (Level6 as u8) == x => Some(Level6),
            x if (Level6_1 as u8) == x => Some(Level6_1),
            x if (Level6_2 as u8) == x => Some(Level6_2),
            x if (Level6_3 as u8) == x => Some(Level6_3),
            x if (Level15_5 as u8) == x => Some(Level15_5),
            _ => None,
        }
    }

    /// Convert to H.266 level_id value.
    pub fn to_id(self) -> u8 {
        self as u8
    }
}

impl PartialOrd for H266Level {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for H266Level {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.ordinal().cmp(&other.ordinal())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_h266_profile_tier_level_new() {
        // Valid combination
        let ptl = H266ProfileTierLevel::new(1, 0, 51);
        assert!(ptl.is_some());
        let ptl = ptl.unwrap();
        assert_eq!(ptl.profile(), H266Profile::Main10);
        assert_eq!(ptl.tier(), H266Tier::Main);
        assert_eq!(ptl.level(), H266Level::Level3_1);

        // Invalid profile_id
        let ptl = H266ProfileTierLevel::new(99, 0, 51);
        assert!(ptl.is_none());

        // Invalid tier_flag
        let ptl = H266ProfileTierLevel::new(1, 5, 51);
        assert!(ptl.is_none());

        // Invalid level_id (93 is an H.265 level encoding, not H.266)
        let ptl = H266ProfileTierLevel::new(1, 0, 93);
        assert!(ptl.is_none());
    }

    #[test]
    fn test_h266_profile_from_id() {
        assert_eq!(H266Profile::from_id(1), Some(H266Profile::Main10));
        assert_eq!(H266Profile::from_id(33), Some(H266Profile::Main10_444));
        assert_eq!(
            H266Profile::from_id(65),
            Some(H266Profile::Main10StillPicture)
        );
        assert_eq!(
            H266Profile::from_id(81),
            Some(H266Profile::MultilayerMain10StillPicture)
        );
        assert_eq!(
            H266Profile::from_id(113),
            Some(H266Profile::MultilayerMain10_444StillPicture)
        );
        assert_eq!(H266Profile::from_id(2), None);
    }

    #[test]
    fn test_h266_tier_from_flag() {
        assert_eq!(H266Tier::from_flag(0), Some(H266Tier::Main));
        assert_eq!(H266Tier::from_flag(1), Some(H266Tier::High));
        assert_eq!(H266Tier::from_flag(2), None);
    }

    #[test]
    fn test_h266_level_from_id() {
        assert_eq!(H266Level::from_id(16), Some(H266Level::Level1));
        assert_eq!(H266Level::from_id(51), Some(H266Level::Level3_1));
        assert_eq!(H266Level::from_id(105), Some(H266Level::Level6_3));
        assert_eq!(H266Level::from_id(255), Some(H266Level::Level15_5));
        // H.265 encodings must NOT parse as H.266 levels.
        assert_eq!(H266Level::from_id(93), None);
        assert_eq!(H266Level::from_id(180), None);
    }

    #[test]
    fn test_h266_level_ordering() {
        assert!(H266Level::Level1 < H266Level::Level2);
        assert!(H266Level::Level3_1 < H266Level::Level4);
        assert!(H266Level::Level6_2 < H266Level::Level6_3);
        assert!(H266Level::Level6_3 < H266Level::Level15_5);
        assert_eq!(H266Level::Level3_1.ordinal(), 4);
    }

    #[test]
    fn test_h266_profile_tier_level_from_fmtp() {
        let mut params = HashMap::new();
        params.insert("profile-id".to_string(), "1".to_string());
        params.insert("tier-flag".to_string(), "0".to_string());
        params.insert("level-id".to_string(), "51".to_string());

        let ptl = H266ProfileTierLevel::from_fmtp(&params);
        assert!(ptl.is_some());
        let ptl = ptl.unwrap();
        assert_eq!(ptl.profile(), H266Profile::Main10);
        assert_eq!(ptl.tier(), H266Tier::Main);
        assert_eq!(ptl.level(), H266Level::Level3_1);

        // Missing parameter
        let mut params = HashMap::new();
        params.insert("profile-id".to_string(), "1".to_string());
        params.insert("tier-flag".to_string(), "0".to_string());
        let ptl = H266ProfileTierLevel::from_fmtp(&params);
        assert!(ptl.is_none());
    }

    #[test]
    fn test_fallback_values() {
        let fallback = H266ProfileTierLevel::FALLBACK;
        assert_eq!(fallback.profile(), H266Profile::Main10);
        assert_eq!(fallback.tier(), H266Tier::Main);
        assert_eq!(fallback.level(), H266Level::Level3_1);
    }

    #[test]
    fn test_h265_h266_level_spaces_disjoint() {
        // The disambiguation in SDP parsing relies on this property.
        let h265_levels = [30, 60, 63, 90, 93, 120, 123, 150, 153, 156, 180, 183, 186];
        for id in h265_levels {
            assert_eq!(
                H266Level::from_id(id),
                None,
                "level-id {id} must not be valid H.266"
            );
        }
    }
}
