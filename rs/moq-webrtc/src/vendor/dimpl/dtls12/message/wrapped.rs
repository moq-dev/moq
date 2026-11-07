use nom::Err;
use nom::IResult;
use nom::bytes::complete::take;
use nom::error::{Error, ErrorKind};
use nom::number::complete::{be_u16, be_u24};
use std::ops::Range;

macro_rules! wrapped_slice {
    ($name:ident, $length_parser:path, $min:expr) => {
        #[derive(Debug, Clone, PartialEq, Eq, Default)]
        pub struct $name(pub Range<usize>);

        impl $name {
            pub fn parse(input: &[u8], base_offset: usize) -> IResult<&[u8], Self> {
                let original_input = input;
                let (input, len) = $length_parser(input)?;
                #[allow(unused_comparisons)]
                if len < $min {
                    return Err(Err::Failure(Error::new(input, ErrorKind::LengthValue)));
                }
                let (input, data_slice) = take(len)(input)?;

                // Calculate absolute range in root buffer
                let relative_offset =
                    data_slice.as_ptr() as usize - original_input.as_ptr() as usize;
                let start = base_offset + relative_offset;
                let end = start + data_slice.len();

                Ok((input, $name(start..end)))
            }

            pub fn as_slice<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
                &buf[self.0.clone()]
            }
        }
    };
}

wrapped_slice!(Asn1Cert, be_u24, 0);
wrapped_slice!(DistinguishedName, be_u16, 1);
