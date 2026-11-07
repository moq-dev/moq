use crate::SrtpProfile;
use crate::buffer::Buf;
use arrayvec::ArrayVec;
use nom::bytes::complete::take;
use nom::error::{Error, ErrorKind};
use nom::number::complete::{be_u8, be_u16};
use nom::{Err, IResult};

/// DTLS-SRTP protection profile identifiers
/// From RFC 5764 Section 4.1.2
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(non_camel_case_types)]
pub enum SrtpProfileId {
    #[default]
    SRTP_AES128_CM_SHA1_80 = 0x0001,
    SRTP_AEAD_AES_128_GCM = 0x0007,
    SRTP_AEAD_AES_256_GCM = 0x0008,
}

impl SrtpProfileId {
    pub fn parse(input: &[u8]) -> IResult<&[u8], SrtpProfileId> {
        let (input, value) = be_u16(input)?;
        let profile = match value {
            0x0001 => SrtpProfileId::SRTP_AES128_CM_SHA1_80,
            0x0007 => SrtpProfileId::SRTP_AEAD_AES_128_GCM,
            0x0008 => SrtpProfileId::SRTP_AEAD_AES_256_GCM,
            _ => {
                return Err(nom::Err::Error(nom::error::Error::new(
                    input,
                    nom::error::ErrorKind::Switch,
                )));
            }
        };
        Ok((input, profile))
    }

    pub fn as_u16(&self) -> u16 {
        *self as u16
    }

    /// All recognized SRTP profile IDs (every non-`Unknown` variant).
    pub const fn all() -> &'static [SrtpProfileId; 3] {
        &[
            SrtpProfileId::SRTP_AES128_CM_SHA1_80,
            SrtpProfileId::SRTP_AEAD_AES_128_GCM,
            SrtpProfileId::SRTP_AEAD_AES_256_GCM,
        ]
    }

    /// Supported SRTP profile IDs.
    pub const fn supported() -> &'static [SrtpProfileId; 3] {
        Self::all()
    }
}

impl From<SrtpProfile> for SrtpProfileId {
    fn from(profile: SrtpProfile) -> Self {
        match profile {
            SrtpProfile::AES128_CM_SHA1_80 => SrtpProfileId::SRTP_AES128_CM_SHA1_80,
            SrtpProfile::AEAD_AES_128_GCM => SrtpProfileId::SRTP_AEAD_AES_128_GCM,
            SrtpProfile::AEAD_AES_256_GCM => SrtpProfileId::SRTP_AEAD_AES_256_GCM,
        }
    }
}

impl From<SrtpProfileId> for SrtpProfile {
    fn from(profile: SrtpProfileId) -> Self {
        match profile {
            SrtpProfileId::SRTP_AES128_CM_SHA1_80 => SrtpProfile::AES128_CM_SHA1_80,
            SrtpProfileId::SRTP_AEAD_AES_128_GCM => SrtpProfile::AEAD_AES_128_GCM,
            SrtpProfileId::SRTP_AEAD_AES_256_GCM => SrtpProfile::AEAD_AES_256_GCM,
        }
    }
}

/// UseSrtp extension as defined in RFC 5764
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseSrtpExtension {
    pub profiles: ArrayVec<SrtpProfileId, 3>,
    pub mki: ArrayVec<u8, 255>, // MKI value (usually empty)
}

impl UseSrtpExtension {
    pub fn new(profiles: ArrayVec<SrtpProfileId, 3>, mki: ArrayVec<u8, 255>) -> Self {
        UseSrtpExtension { profiles, mki }
    }

    /// Create a default UseSrtpExtension with standard profiles
    pub fn default() -> Self {
        let mut profiles = ArrayVec::new();
        // Add profiles in order of preference (most secure first)
        profiles.push(SrtpProfileId::SRTP_AEAD_AES_256_GCM);
        profiles.push(SrtpProfileId::SRTP_AEAD_AES_128_GCM);
        profiles.push(SrtpProfileId::SRTP_AES128_CM_SHA1_80);

        // MKI is typically empty as per RFC 5764
        UseSrtpExtension {
            profiles,
            mki: ArrayVec::new(),
        }
    }

    pub fn parse(input: &[u8]) -> IResult<&[u8], UseSrtpExtension> {
        let (input, profiles_length) = be_u16(input)?;
        if profiles_length == 0 {
            return Err(Err::Failure(Error::new(input, ErrorKind::LengthValue)));
        }
        let (input, profiles_data) = take(profiles_length)(input)?;

        // Parse the profiles (ignore unknown profile IDs instead of failing)
        let mut profiles: ArrayVec<SrtpProfileId, 3> = ArrayVec::new();
        let mut profiles_rest = profiles_data;

        while profiles_rest.len() >= 2 {
            let profile_input = profiles_rest;
            let (rest, value) = be_u16(profile_input)?;
            profiles_rest = rest;
            match value {
                0x0001 => profiles
                    .try_push(SrtpProfileId::SRTP_AES128_CM_SHA1_80)
                    .map_err(|_| Err::Failure(Error::new(profile_input, ErrorKind::LengthValue)))?,
                0x0007 => profiles
                    .try_push(SrtpProfileId::SRTP_AEAD_AES_128_GCM)
                    .map_err(|_| Err::Failure(Error::new(profile_input, ErrorKind::LengthValue)))?,
                0x0008 => profiles
                    .try_push(SrtpProfileId::SRTP_AEAD_AES_256_GCM)
                    .map_err(|_| Err::Failure(Error::new(profile_input, ErrorKind::LengthValue)))?,
                _ => {
                    // Unknown SRTP profile: skip
                }
            }
        }
        if !profiles_rest.is_empty() {
            return Err(Err::Failure(Error::new(
                profiles_rest,
                ErrorKind::LengthValue,
            )));
        }

        // Parse MKI
        let (input, mki_length) = be_u8(input)?;
        let (input, mki) = take(mki_length)(input)?;
        if !input.is_empty() {
            return Err(Err::Failure(Error::new(input, ErrorKind::LengthValue)));
        }

        Ok((
            input,
            UseSrtpExtension {
                profiles,
                mki: ArrayVec::try_from(mki).unwrap_or_default(),
            },
        ))
    }

    pub fn serialize(&self, output: &mut Buf) {
        // Length of all profiles (2 bytes per profile)
        output.extend_from_slice(&((self.profiles.len() * 2) as u16).to_be_bytes());

        // Write each profile
        for profile in &self.profiles {
            output.extend_from_slice(&profile.as_u16().to_be_bytes());
        }

        // MKI length and data
        output.push(self.mki.len() as u8);
        output.extend_from_slice(&self.mki);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buf;

    #[test]
    fn test_use_srtp_extension() {
        let mut profiles = ArrayVec::new();
        profiles.push(SrtpProfileId::SRTP_AEAD_AES_256_GCM);
        profiles.push(SrtpProfileId::SRTP_AEAD_AES_128_GCM);
        profiles.push(SrtpProfileId::SRTP_AES128_CM_SHA1_80);

        let mut mki = ArrayVec::new();
        mki.push(1);
        mki.push(2);
        mki.push(3);

        let ext = UseSrtpExtension::new(profiles, mki.clone());

        let mut serialized = Buf::new();
        ext.serialize(&mut serialized);

        let expected = [
            0x00, 0x06, // Profiles length (6 bytes)
            0x00, 0x08, // SRTP_AEAD_AES_256_GCM (0x0008)
            0x00, 0x07, // SRTP_AEAD_AES_128_GCM (0x0007)
            0x00, 0x01, // SRTP_AES128_CM_SHA1_80 (0x0001)
            0x03, // MKI length (3 bytes)
            0x01, 0x02, 0x03, // MKI
        ];

        assert_eq!(&*serialized, expected);

        let (_, parsed) = UseSrtpExtension::parse(&serialized).unwrap();

        assert_eq!(parsed.profiles.as_slice(), ext.profiles.as_slice());
        assert_eq!(parsed.mki, mki);
    }

    #[test]
    fn test_use_srtp_parse_provided_bytes() {
        let bytes = [0, 8, 0, 7, 0, 8, 0, 1, 0, 2, 0];

        let (_, parsed) = UseSrtpExtension::parse(&bytes).expect("parse UseSrtpExtension");

        assert_eq!(
            parsed.profiles.as_slice(),
            &[
                SrtpProfileId::SRTP_AEAD_AES_128_GCM,
                SrtpProfileId::SRTP_AEAD_AES_256_GCM,
                SrtpProfileId::SRTP_AES128_CM_SHA1_80
            ]
        );
        assert_eq!(parsed.mki, ArrayVec::<u8, 255>::new());
    }

    #[test]
    fn capacity_matches_profile_count() {
        let ext = UseSrtpExtension::default();
        assert_eq!(
            ext.profiles.capacity(),
            SrtpProfileId::supported().len(),
            "UseSrtpExtension capacity must match supported profile IDs"
        );
    }

    #[test]
    fn too_many_supported_srtp_profiles_are_rejected() {
        let bytes = [
            0x00, 0x08, // Four profile IDs.
            0x00, 0x01, // SRTP_AES128_CM_SHA1_80.
            0x00, 0x01, // SRTP_AES128_CM_SHA1_80.
            0x00, 0x01, // SRTP_AES128_CM_SHA1_80.
            0x00, 0x01, // SRTP_AES128_CM_SHA1_80.
            0x00, // Empty MKI.
        ];

        let err = UseSrtpExtension::parse(&bytes).unwrap_err();

        assert!(matches!(
            err,
            Err::Failure(Error {
                code: ErrorKind::LengthValue,
                ..
            })
        ));
    }

    #[test]
    fn odd_srtp_profile_vector_is_rejected() {
        let bytes = [
            0x00, 0x03, // Odd profile vector length.
            0x00, 0x01, // SRTP_AES128_CM_SHA1_80.
            0x00, // Dangling byte in profile vector.
            0x00, // Empty MKI.
        ];

        let err = UseSrtpExtension::parse(&bytes).unwrap_err();

        assert!(matches!(
            err,
            Err::Failure(Error {
                code: ErrorKind::LengthValue,
                ..
            })
        ));
    }

    #[test]
    fn trailing_mki_bytes_are_rejected() {
        let bytes = [
            0x00, 0x02, // One profile ID.
            0x00, 0x01, // SRTP_AES128_CM_SHA1_80.
            0x00, // Empty MKI.
            0xFF, // Trailing extension-body byte.
        ];

        let err = UseSrtpExtension::parse(&bytes).unwrap_err();

        assert!(matches!(
            err,
            Err::Failure(Error {
                code: ErrorKind::LengthValue,
                ..
            })
        ));
    }
}
