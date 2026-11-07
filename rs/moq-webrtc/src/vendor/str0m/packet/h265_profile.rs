use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// H.265/HEVC profile, tier, and level combination.
///
/// Represents the three SDP fmtp parameters `profile-id`, `tier-flag`, and `level-id`
/// as defined in RFC 7798 §7.1 and ITU-T H.265 Annex A.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct H265ProfileTierLevel {
    profile: H265Profile,
    tier: H265Tier,
    level: H265Level,
}

impl H265ProfileTierLevel {
    // RFC 7798 §7.1 default values when parameters are absent from SDP:
    // profile-id=1 (Main), tier-flag=0 (Main tier), level-id=93 (Level 3.1).
    // https://www.rfc-editor.org/rfc/rfc7798#section-7.1
    pub(crate) const FALLBACK: Self = Self {
        profile: H265Profile::Main,
        tier: H265Tier::Main,
        level: H265Level::Level3_1,
    };

    /// Construct a new H265ProfileTierLevel from profile_id, tier_flag, and level_id.
    ///
    /// Returns `Some(Self)` only if the provided parameters identify valid values.
    pub(crate) fn new(profile_id: u8, tier_flag: u8, level_id: u8) -> Option<Self> {
        let profile = H265Profile::from_id(profile_id)?;
        let tier = H265Tier::from_flag(tier_flag)?;
        let level = H265Level::from_id(level_id)?;

        Some(Self {
            profile,
            tier,
            level,
        })
    }

    /// Parse H265ProfileTierLevel from SDP fmtp parameters.
    ///
    /// Expects keys: "profile-id", "tier-flag", "level-id"
    /// Returns None if any required parameter is missing or invalid.
    pub(crate) fn from_fmtp(params: &HashMap<String, String>) -> Option<Self> {
        let profile_id: u8 = params.get("profile-id")?.parse().ok()?;
        let tier_flag: u8 = params.get("tier-flag")?.parse().ok()?;
        let level_id: u8 = params.get("level-id")?.parse().ok()?;

        Self::new(profile_id, tier_flag, level_id)
    }

    /// Returns the H.265 profile (Main, Main10, etc.).
    pub fn profile(&self) -> H265Profile {
        self.profile
    }

    /// Returns the H.265 tier (Main or High).
    pub fn tier(&self) -> H265Tier {
        self.tier
    }

    /// Returns the H.265 level (Level 3.1, Level 4.0, etc.).
    pub fn level(&self) -> H265Level {
        self.level
    }

    /// Returns a copy with the level replaced.
    pub fn with_level(self, level: H265Level) -> Self {
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

impl From<(u8, u8, u8)> for H265ProfileTierLevel {
    fn from(value: (u8, u8, u8)) -> Self {
        Self::new(value.0, value.1, value.2).unwrap_or(Self::FALLBACK)
    }
}

/// H.265 profile as defined in ITU-T H.265 Annex A.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum H265Profile {
    /// Main profile (profile_id=1).
    Main,
    /// Main 10 profile (profile_id=2).
    Main10,
    /// Main Still Picture profile (profile_id=3).
    MainStillPicture,
    /// Format Range Extensions profile (profile_id=4).
    FormatRangeExtensions,
    /// High Throughput profile (profile_id=5).
    HighThroughput,
    /// Multiview Main profile (profile_id=6).
    MultiviewMain,
    /// Scalable Main profile (profile_id=7).
    ScalableMain,
    /// 3D Main profile (profile_id=8).
    ThreeDMain,
    /// Screen Content Coding profile (profile_id=9).
    ScreenContentCoding,
    /// Screen Content Coding Extensions profile (profile_id=10).
    ScreenContentCodingExtensions,
    /// High Throughput Screen Content Coding profile (profile_id=11).
    HighThroughputScreenContentCoding,
}

impl H265Profile {
    /// Convert from H.265 profile_id value.
    ///
    /// See ITU-T H.265 (04/2013) Annex A for profile definitions.
    fn from_id(profile_id: u8) -> Option<Self> {
        match profile_id {
            1 => Some(Self::Main),
            2 => Some(Self::Main10),
            3 => Some(Self::MainStillPicture),
            4 => Some(Self::FormatRangeExtensions),
            5 => Some(Self::HighThroughput),
            6 => Some(Self::MultiviewMain),
            7 => Some(Self::ScalableMain),
            8 => Some(Self::ThreeDMain),
            9 => Some(Self::ScreenContentCoding),
            10 => Some(Self::ScreenContentCodingExtensions),
            11 => Some(Self::HighThroughputScreenContentCoding),
            _ => None,
        }
    }

    /// Convert to H.265 profile_id value.
    pub fn to_id(self) -> u8 {
        match self {
            Self::Main => 1,
            Self::Main10 => 2,
            Self::MainStillPicture => 3,
            Self::FormatRangeExtensions => 4,
            Self::HighThroughput => 5,
            Self::MultiviewMain => 6,
            Self::ScalableMain => 7,
            Self::ThreeDMain => 8,
            Self::ScreenContentCoding => 9,
            Self::ScreenContentCodingExtensions => 10,
            Self::HighThroughputScreenContentCoding => 11,
        }
    }
}

/// H.265 tier (Main or High).
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum H265Tier {
    /// Main tier (tier_flag=0).
    Main,
    /// High tier (tier_flag=1).
    High,
}

impl H265Tier {
    /// Convert from H.265 tier_flag value.
    ///
    /// 0 = Main tier, 1 = High tier
    fn from_flag(tier_flag: u8) -> Option<Self> {
        match tier_flag {
            0 => Some(Self::Main),
            1 => Some(Self::High),
            _ => None,
        }
    }

    /// Convert to H.265 tier_flag value.
    pub fn to_flag(self) -> u8 {
        match self {
            Self::Main => 0,
            Self::High => 1,
        }
    }
}

/// H.265 level as defined in ITU-T H.265 Annex A.
///
/// Level IDs are 30× the level number (e.g., Level 3.1 = 93).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
#[rustfmt::skip]
pub enum H265Level {
    /// Level 1.0 (level_id=30).
    Level1   = 30_u8,
    /// Level 2.0 (level_id=60).
    Level2   = 60_u8,
    /// Level 2.1 (level_id=63).
    Level2_1 = 63_u8,
    /// Level 3.0 (level_id=90).
    Level3   = 90_u8,
    /// Level 3.1 (level_id=93).
    Level3_1 = 93_u8,
    /// Level 4.0 (level_id=120).
    Level4   = 120_u8,
    /// Level 4.1 (level_id=123).
    Level4_1 = 123_u8,
    /// Level 5.0 (level_id=150).
    Level5   = 150_u8,
    /// Level 5.1 (level_id=153).
    Level5_1 = 153_u8,
    /// Level 5.2 (level_id=156).
    Level5_2 = 156_u8,
    /// Level 6.0 (level_id=180).
    Level6   = 180_u8,
    /// Level 6.1 (level_id=183).
    Level6_1 = 183_u8,
    /// Level 6.2 (level_id=186).
    Level6_2 = 186_u8,
}

#[cfg(feature = "drv")]
crate::drv_identity_copy!(H265ProfileTierLevel, H265Profile, H265Tier, H265Level);

impl H265Level {
    /// Returns the ordinal position (0-12) representing capability order.
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
        }
    }

    /// Convert from H.265 level_id value.
    fn from_id(level_id: u8) -> Option<Self> {
        use H265Level::*;
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
            _ => None,
        }
    }

    /// Convert to H.265 level_id value.
    pub fn to_id(self) -> u8 {
        self as u8
    }
}

impl PartialOrd for H265Level {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for H265Level {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.ordinal().cmp(&other.ordinal())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_h265_profile_tier_level_new() {
        // Valid combination
        let ptl = H265ProfileTierLevel::new(1, 0, 93);
        assert!(ptl.is_some());
        let ptl = ptl.unwrap();
        assert_eq!(ptl.profile(), H265Profile::Main);
        assert_eq!(ptl.tier(), H265Tier::Main);
        assert_eq!(ptl.level(), H265Level::Level3_1);

        // Invalid profile_id
        let ptl = H265ProfileTierLevel::new(99, 0, 93);
        assert!(ptl.is_none());

        // Invalid tier_flag
        let ptl = H265ProfileTierLevel::new(1, 5, 93);
        assert!(ptl.is_none());

        // Invalid level_id
        let ptl = H265ProfileTierLevel::new(1, 0, 255);
        assert!(ptl.is_none());
    }

    #[test]
    fn test_h265_profile_from_id() {
        assert_eq!(H265Profile::from_id(1), Some(H265Profile::Main));
        assert_eq!(H265Profile::from_id(2), Some(H265Profile::Main10));
        assert_eq!(H265Profile::from_id(99), None);
    }

    #[test]
    fn test_h265_tier_from_flag() {
        assert_eq!(H265Tier::from_flag(0), Some(H265Tier::Main));
        assert_eq!(H265Tier::from_flag(1), Some(H265Tier::High));
        assert_eq!(H265Tier::from_flag(2), None);
    }

    #[test]
    fn test_h265_level_from_id() {
        assert_eq!(H265Level::from_id(30), Some(H265Level::Level1));
        assert_eq!(H265Level::from_id(93), Some(H265Level::Level3_1));
        assert_eq!(H265Level::from_id(186), Some(H265Level::Level6_2));
        assert_eq!(H265Level::from_id(255), None);
    }

    #[test]
    fn test_h265_level_ordering() {
        assert!(H265Level::Level1 < H265Level::Level2);
        assert!(H265Level::Level3_1 < H265Level::Level4);
        assert!(H265Level::Level6_1 < H265Level::Level6_2);
        assert_eq!(H265Level::Level3_1.ordinal(), 4);
    }

    #[test]
    fn test_h265_profile_tier_level_from_fmtp() {
        let mut params = HashMap::new();
        params.insert("profile-id".to_string(), "1".to_string());
        params.insert("tier-flag".to_string(), "0".to_string());
        params.insert("level-id".to_string(), "93".to_string());

        let ptl = H265ProfileTierLevel::from_fmtp(&params);
        assert!(ptl.is_some());
        let ptl = ptl.unwrap();
        assert_eq!(ptl.profile(), H265Profile::Main);
        assert_eq!(ptl.tier(), H265Tier::Main);
        assert_eq!(ptl.level(), H265Level::Level3_1);

        // Missing parameter
        let mut params = HashMap::new();
        params.insert("profile-id".to_string(), "1".to_string());
        params.insert("tier-flag".to_string(), "0".to_string());
        let ptl = H265ProfileTierLevel::from_fmtp(&params);
        assert!(ptl.is_none());
    }

    #[test]
    fn test_fallback_values() {
        let fallback = H265ProfileTierLevel::FALLBACK;
        assert_eq!(fallback.profile(), H265Profile::Main);
        assert_eq!(fallback.tier(), H265Tier::Main);
        assert_eq!(fallback.level(), H265Level::Level3_1);
    }
}
