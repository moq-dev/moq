use crate::buffer::Buf;
use arrayvec::ArrayVec;
use nom::{IResult, bytes::complete::take, number::complete::be_u16};
use std::{fmt, ops::Range};

pub type ExtensionVec = ArrayVec<Extension, { ExtensionType::supported().len() }>;

#[derive(Debug, PartialEq, Eq, Default)]
pub struct Extension {
    pub extension_type: ExtensionType,
    pub extension_data_range: Range<usize>,
}

impl Extension {
    pub fn parse(input: &[u8], base_offset: usize) -> IResult<&[u8], Extension> {
        let original_input = input;
        let (input, extension_type) = ExtensionType::parse(input)?;
        let (input, extension_length) = be_u16(input)?;
        let (input, extension_data_slice) = if extension_length > 0 {
            take(extension_length)(input)?
        } else {
            (input, &input[0..0])
        };

        // Calculate absolute range in root buffer
        let relative_offset =
            extension_data_slice.as_ptr() as usize - original_input.as_ptr() as usize;
        let start = base_offset + relative_offset;
        let end = start + extension_data_slice.len();

        Ok((
            input,
            Extension {
                extension_type,
                extension_data_range: start..end,
            },
        ))
    }

    pub fn extension_data<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        &buf[self.extension_data_range.clone()]
    }

    pub fn serialize(&self, buf: &[u8], output: &mut Buf) {
        let extension_data = self.extension_data(buf);
        output.extend_from_slice(&self.extension_type.as_u16().to_be_bytes());
        output.extend_from_slice(&(extension_data.len() as u16).to_be_bytes());
        if !extension_data.is_empty() {
            output.extend_from_slice(extension_data);
        }
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ExtensionType(u16);

#[allow(non_upper_case_globals)]
impl ExtensionType {
    pub const ServerName: Self = Self(0x0000);
    pub const MaxFragmentLength: Self = Self(0x0001);
    pub const ClientCertificateUrl: Self = Self(0x0002);
    pub const TrustedCaKeys: Self = Self(0x0003);
    pub const TruncatedHmac: Self = Self(0x0004);
    pub const StatusRequest: Self = Self(0x0005);
    pub const UserMapping: Self = Self(0x0006);
    pub const ClientAuthz: Self = Self(0x0007);
    pub const ServerAuthz: Self = Self(0x0008);
    pub const CertType: Self = Self(0x0009);
    pub const SupportedGroups: Self = Self(0x000A);
    pub const EcPointFormats: Self = Self(0x000B);
    pub const Srp: Self = Self(0x000C);
    pub const SignatureAlgorithms: Self = Self(0x000D);
    pub const UseSrtp: Self = Self(0x000E);
    pub const Heartbeat: Self = Self(0x000F);
    pub const ApplicationLayerProtocolNegotiation: Self = Self(0x0010);
    pub const StatusRequestV2: Self = Self(0x0011);
    pub const SignedCertificateTimestamp: Self = Self(0x0012);
    pub const ClientCertificateType: Self = Self(0x0013);
    pub const ServerCertificateType: Self = Self(0x0014);
    pub const Padding: Self = Self(0x0015);
    pub const EncryptThenMac: Self = Self(0x0016);
    pub const ExtendedMasterSecret: Self = Self(0x0017);
    pub const TokenBinding: Self = Self(0x0018);
    pub const CachedInfo: Self = Self(0x0019);
    pub const SessionTicket: Self = Self(0x0023);
    pub const PreSharedKey: Self = Self(0x0029);
    pub const EarlyData: Self = Self(0x002A);
    pub const SupportedVersions: Self = Self(0x002B);
    pub const Cookie: Self = Self(0x002C);
    pub const PskKeyExchangeModes: Self = Self(0x002D);
    pub const CertificateAuthorities: Self = Self(0x002F);
    pub const OidFilters: Self = Self(0x0030);
    pub const PostHandshakeAuth: Self = Self(0x0031);
    pub const SignatureAlgorithmsCert: Self = Self(0x0032);
    pub const KeyShare: Self = Self(0x0033);
    pub const RenegotiationInfo: Self = Self(0xFF01);

    pub const fn from_u16(value: u16) -> Self {
        Self(value)
    }

    pub const fn as_u16(&self) -> u16 {
        self.0
    }

    const fn is_unknown(&self) -> bool {
        !matches!(
            *self,
            Self(0x0000..=0x0019 | 0x0023 | 0x0029..=0x002D | 0x002F..=0x0033 | 0xFF01)
        )
    }

    pub fn parse(input: &[u8]) -> IResult<&[u8], ExtensionType> {
        let (input, value) = be_u16(input)?;
        Ok((input, ExtensionType::from_u16(value)))
    }

    /// Returns true if this extension type is supported by this implementation.
    pub fn is_supported(&self) -> bool {
        Self::supported().contains(self)
    }

    /// Supported extension types that this implementation handles.
    pub const fn supported() -> &'static [ExtensionType; 8] {
        &[
            ExtensionType::SupportedGroups,
            ExtensionType::EcPointFormats,
            ExtensionType::SignatureAlgorithms,
            ExtensionType::UseSrtp,
            ExtensionType::EncryptThenMac,
            ExtensionType::ExtendedMasterSecret,
            ExtensionType::RenegotiationInfo,
            ExtensionType::SessionTicket,
        ]
    }
}

impl fmt::Debug for ExtensionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_unknown() {
            return f.debug_tuple("Unknown").field(&self.0).finish();
        }

        let name = match *self {
            ExtensionType::ServerName => "ServerName",
            ExtensionType::MaxFragmentLength => "MaxFragmentLength",
            ExtensionType::ClientCertificateUrl => "ClientCertificateUrl",
            ExtensionType::TrustedCaKeys => "TrustedCaKeys",
            ExtensionType::TruncatedHmac => "TruncatedHmac",
            ExtensionType::StatusRequest => "StatusRequest",
            ExtensionType::UserMapping => "UserMapping",
            ExtensionType::ClientAuthz => "ClientAuthz",
            ExtensionType::ServerAuthz => "ServerAuthz",
            ExtensionType::CertType => "CertType",
            ExtensionType::SupportedGroups => "SupportedGroups",
            ExtensionType::EcPointFormats => "EcPointFormats",
            ExtensionType::Srp => "Srp",
            ExtensionType::SignatureAlgorithms => "SignatureAlgorithms",
            ExtensionType::UseSrtp => "UseSrtp",
            ExtensionType::Heartbeat => "Heartbeat",
            ExtensionType::ApplicationLayerProtocolNegotiation => {
                "ApplicationLayerProtocolNegotiation"
            }
            ExtensionType::StatusRequestV2 => "StatusRequestV2",
            ExtensionType::SignedCertificateTimestamp => "SignedCertificateTimestamp",
            ExtensionType::ClientCertificateType => "ClientCertificateType",
            ExtensionType::ServerCertificateType => "ServerCertificateType",
            ExtensionType::Padding => "Padding",
            ExtensionType::EncryptThenMac => "EncryptThenMac",
            ExtensionType::ExtendedMasterSecret => "ExtendedMasterSecret",
            ExtensionType::TokenBinding => "TokenBinding",
            ExtensionType::CachedInfo => "CachedInfo",
            ExtensionType::SessionTicket => "SessionTicket",
            ExtensionType::PreSharedKey => "PreSharedKey",
            ExtensionType::EarlyData => "EarlyData",
            ExtensionType::SupportedVersions => "SupportedVersions",
            ExtensionType::Cookie => "Cookie",
            ExtensionType::PskKeyExchangeModes => "PskKeyExchangeModes",
            ExtensionType::CertificateAuthorities => "CertificateAuthorities",
            ExtensionType::OidFilters => "OidFilters",
            ExtensionType::PostHandshakeAuth => "PostHandshakeAuth",
            ExtensionType::SignatureAlgorithmsCert => "SignatureAlgorithmsCert",
            ExtensionType::KeyShare => "KeyShare",
            ExtensionType::RenegotiationInfo => "RenegotiationInfo",
            _ => unreachable!("known DTLS 1.2 extension type missing Debug label"),
        };

        f.write_str(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buf;

    const MESSAGE: &[u8] = &[
        0x00, 0x0A, // ExtensionType::SupportedGroups
        0x00, 0x08, // Extension length
        0x00, 0x06, 0x00, 0x17, 0x00, 0x18, 0x00, 0x19, // Extension data
    ];

    #[test]
    fn extension_type_newtype_shape() {
        assert_eq!(std::mem::size_of::<ExtensionType>(), 2);
        assert_eq!(ExtensionType::default().as_u16(), 0);
        assert_eq!(ExtensionType::default(), ExtensionType::ServerName);
    }

    #[test]
    fn extension_type_wire_roundtrip() {
        for extension_type in ExtensionType::supported() {
            assert_eq!(
                ExtensionType::from_u16(extension_type.as_u16()),
                *extension_type
            );
            assert!(!extension_type.is_unknown());
        }

        let unknown = ExtensionType::from_u16(0xFFFF);
        assert_eq!(unknown.as_u16(), 0xFFFF);
        assert!(unknown.is_unknown());
    }

    #[test]
    fn extension_type_debug_stays_enum_like() {
        assert_eq!(
            format!("{:?}", ExtensionType::SupportedGroups),
            "SupportedGroups"
        );
        assert_eq!(
            format!("{:?}", ExtensionType::from_u16(0xFFFF)),
            "Unknown(65535)"
        );
    }

    #[test]
    fn roundtrip() {
        // Parse the message with base_offset 0
        let (rest, parsed) = Extension::parse(MESSAGE, 0).unwrap();
        assert!(rest.is_empty());

        // Serialize and compare to MESSAGE
        let mut serialized = Buf::new();
        parsed.serialize(MESSAGE, &mut serialized);
        assert_eq!(&*serialized, MESSAGE);
    }
}
