use super::Dtls12CipherSuite;
use crate::buffer::Buf;
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

    pub fn parse(input: &[u8], cipher_suite: Dtls12CipherSuite) -> IResult<&[u8], Finished> {
        let verify_data_length = cipher_suite.verify_data_length();
        let (rest, verify_data_slice) = take(verify_data_length)(input)?;

        // Calculate range relative to input buffer without unsafe code
        // verify_data_slice is a sub-slice of input from nom's take combinator
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
    use super::Dtls12CipherSuite;
    use super::*;
    use crate::buffer::Buf;

    #[test]
    fn roundtrip() {
        let verify_data = vec![
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C,
        ];

        // Parse the data
        let (rest, parsed) = Finished::parse(
            &verify_data,
            Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256,
        )
        .unwrap();
        assert!(rest.is_empty());

        // Serialize and compare to original
        let mut serialized = Buf::new();
        parsed.serialize(&verify_data, &mut serialized);
        assert_eq!(&*serialized, &verify_data[..]);
    }
}
