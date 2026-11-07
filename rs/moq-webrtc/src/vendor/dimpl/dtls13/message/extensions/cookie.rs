use nom::Err;
use nom::IResult;
use nom::error::{Error, ErrorKind};
use nom::number::complete::be_u16;

/// Parse the DTLS 1.3 Cookie extension body.
///
/// RFC 8446 defines the extension payload as `cookie<1..2^16-1>`,
/// so the extension body is a u16 length followed by exactly that many bytes.
pub(crate) fn parse_cookie_extension(input: &[u8]) -> IResult<&[u8], &[u8]> {
    if input.len() < 2 {
        return Err(Err::Failure(Error::new(input, ErrorKind::LengthValue)));
    }

    let (rest, cookie_len) = be_u16(input)?;
    if cookie_len == 0 || rest.len() != cookie_len as usize {
        return Err(Err::Failure(Error::new(input, ErrorKind::LengthValue)));
    }

    Ok((&[], rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_cookie_vectors_are_rejected() {
        for input in [
            &[][..],
            &[0x00],
            &[0x00, 0x00],
            &[0x00, 0x02, 0xAA],
            &[0x00, 0x01, 0xAA, 0xBB],
        ] {
            let result = parse_cookie_extension(input);
            assert!(
                matches!(
                    result,
                    Err(nom::Err::Failure(error))
                        if error.code == nom::error::ErrorKind::LengthValue
                ),
                "malformed cookie vector should fail with LengthValue: {input:02x?}"
            );
        }
    }

    #[test]
    fn valid_cookie_vector_is_accepted() {
        let (rest, cookie) = parse_cookie_extension(&[0x00, 0x02, 0xAA, 0xBB]).unwrap();
        assert!(rest.is_empty());
        assert_eq!(cookie, &[0xAA, 0xBB]);
    }
}
