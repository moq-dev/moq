use crate::buffer::Buf;
use crate::types::Dtls13CipherSuite;
use nom::IResult;
use nom::bytes::complete::take;
use std::ops::Range;

#[derive(Debug, PartialEq, Eq)]
pub struct Finished {
    pub verify_data_range: Range<usize>,
}

impl Finished {
    pub fn verify_data<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        &buf[self.verify_data_range.clone()]
    }

    pub fn parse(input: &[u8], cipher_suite: Dtls13CipherSuite) -> IResult<&[u8], Finished> {
        let verify_data_length = cipher_suite.verify_data_length();
        let (rest, verify_data_slice) = take(verify_data_length)(input)?;

        // Calculate range relative to input buffer
        let start = verify_data_slice.as_ptr() as usize - input.as_ptr() as usize;
        let end = start + verify_data_slice.len();

        Ok((
            rest,
            Finished {
                verify_data_range: start..end,
            },
        ))
    }

    pub fn serialize(&self, buf: &[u8], output: &mut Buf) {
        output.extend_from_slice(self.verify_data(buf));
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::buffer::Buf;

    #[test]
    fn roundtrip() {
        // SHA-256 verify_data is 32 bytes
        let verify_data: Vec<u8> = (0..32).collect();

        let (rest, parsed) =
            Finished::parse(&verify_data, Dtls13CipherSuite::AES_128_GCM_SHA256).unwrap();
        assert!(rest.is_empty());

        let mut serialized = Buf::new();
        parsed.serialize(&verify_data, &mut serialized);
        assert_eq!(&*serialized, &verify_data[..]);
    }
}
