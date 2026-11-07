use super::{param_header::*, param_type::*, *};

use alloc::vec::Vec;
use bytes::BufMut;

///The sender of INIT uses this parameter to list all the address types
///it can support. Each entry is the type value of the corresponding
///address TLV, that is, `ParamType::Ipv4Addr`, `ParamType::Ipv6Addr`
///or `ParamType::HostNameAddr`.
///
/// 0                   1                   2                   3
/// 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
///+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///|          Type = 12            |          Length               |
///+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///|        Address Type #1        |        Address Type #2        |
///+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///|                            ......                             |
///+-+-+-+-+-+-+-+-+-+-+-+-+-+-++-+-+-+-+-+-+-+-+-+-+-+-+-+-++-+-+-+

#[derive(Default, Debug, Clone, PartialEq)]
pub(crate) struct ParamSupportedAddressTypes {
    pub(crate) address_types: Vec<ParamType>,
}

impl fmt::Display for ParamSupportedAddressTypes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.header())?;
        for t in &self.address_types {
            write!(f, " {t}")?;
        }
        Ok(())
    }
}

impl Param for ParamSupportedAddressTypes {
    fn header(&self) -> ParamHeader {
        ParamHeader {
            typ: ParamType::SupportedAddrTypes,
            value_length: self.value_length() as u16,
        }
    }

    fn unmarshal(raw: &Bytes) -> Result<Self> {
        let header = ParamHeader::unmarshal(raw)?;

        if header.value_length() % 2 != 0 {
            return Err(Error::ErrSupportedAddressTypesParamInvalidLength);
        }

        let reader =
            &mut raw.slice(PARAM_HEADER_LENGTH..PARAM_HEADER_LENGTH + header.value_length());
        let mut address_types = Vec::with_capacity(reader.remaining() / 2);
        while reader.has_remaining() {
            address_types.push(ParamType::from(reader.get_u16()));
        }

        Ok(ParamSupportedAddressTypes { address_types })
    }

    fn marshal_to(&self, buf: &mut BytesMut) -> Result<usize> {
        self.header().marshal_to(buf)?;
        for t in &self.address_types {
            buf.put_u16(u16::from(*t));
        }
        Ok(buf.len())
    }

    fn value_length(&self) -> usize {
        self.address_types.len() * 2
    }

    fn clone_to(&self) -> Box<dyn Param + Send + Sync> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }
}
