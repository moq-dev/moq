use super::ProtocolVersion;
use super::id::Cookie;
use crate::buffer::Buf;
use nom::IResult;
use nom::error::{Error, ErrorKind};

#[derive(Debug, PartialEq, Eq)]
pub struct HelloVerifyRequest {
    pub server_version: ProtocolVersion,
    pub cookie: Cookie,
}

impl HelloVerifyRequest {
    pub fn new(server_version: ProtocolVersion, cookie: Cookie) -> Self {
        HelloVerifyRequest {
            server_version,
            cookie,
        }
    }

    pub fn parse(input: &[u8]) -> IResult<&[u8], HelloVerifyRequest> {
        let (input, server_version) = ProtocolVersion::parse(input)?;
        let (input, cookie) = Cookie::parse(input)?;

        if cookie.is_empty() {
            return Err(nom::Err::Failure(Error::new(input, ErrorKind::LengthValue)));
        }

        Ok((
            input,
            HelloVerifyRequest {
                server_version,
                cookie,
            },
        ))
    }

    pub fn serialize(&self, output: &mut Buf) {
        output.extend_from_slice(&self.server_version.as_u16().to_be_bytes());
        output.push(self.cookie.len() as u8);
        output.extend_from_slice(&self.cookie);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buf;

    const MESSAGE: &[u8] = &[
        0xFE, 0xFD, // ProtocolVersion::DTLS1_2
        0x01, // Cookie length
        0xBB, // Cookie
    ];

    #[test]
    fn roundtrip() {
        let cookie = Cookie::try_new(&[0xBB]).unwrap();

        let hello_verify_request = HelloVerifyRequest::new(ProtocolVersion::DTLS1_2, cookie);

        // Serialize and compare to MESSAGE
        let mut serialized = Buf::new();
        hello_verify_request.serialize(&mut serialized);
        assert_eq!(&*serialized, MESSAGE);

        // Parse and compare with original
        let (rest, parsed) = HelloVerifyRequest::parse(&serialized).unwrap();
        assert_eq!(parsed, hello_verify_request);

        assert!(rest.is_empty());
    }

    #[test]
    fn empty_cookie() {
        let message: &[u8] = &[
            0xFE, 0xFD, // ProtocolVersion::DTLS1_2
            0x00, // Cookie length (0, which is empty)
        ];

        let result = HelloVerifyRequest::parse(message);
        assert!(result.is_err());
    }

    #[test]
    fn cookie_too_long() {
        let mut message = MESSAGE.to_vec();
        message[2] = 0xFF; // Cookie length (255, which is too long)

        let result = HelloVerifyRequest::parse(&message);
        assert!(result.is_err());
    }
}
