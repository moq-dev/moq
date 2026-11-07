use nom::bytes::complete::take;
use nom::error::{Error, ErrorKind};
use nom::number::complete::be_u8;
use nom::{Err, IResult};
use std::fmt;
use std::ops::Deref;

pub struct InvalidLength(&'static str, IdType, usize);

impl fmt::Debug for InvalidLength {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self)
    }
}

impl std::error::Error for InvalidLength {}

impl fmt::Display for InvalidLength {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.1 {
            IdType::Fixed(len) => write!(
                f,
                "Incorrect fixed ID ({}) length: {} should be {}",
                self.0, self.2, len
            ),
            IdType::Variable(min, max) => write!(
                f,
                "Incorrect variable ID ({}) length: {} <= {} <= {}",
                self.0, min, self.2, max,
            ),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IdType {
    #[allow(unused)]
    Fixed(usize),
    Variable(usize, usize),
}

macro_rules! var_array {
    ($name:ident, $min:expr, $max:expr) => {
        #[derive(Clone, Copy)]
        pub struct $name([u8; $max], usize);

        impl $name {
            pub fn try_new(data: &[u8]) -> Result<Self, InvalidLength> {
                #[allow(unused_comparisons)]
                if data.len() < $min || data.len() > $max {
                    return Err(InvalidLength(
                        stringify!($name),
                        IdType::Variable($min, $max),
                        data.len(),
                    ));
                }
                let mut array = [0; $max];
                array[..data.len()].copy_from_slice(data);
                Ok($name(array, data.len()))
            }

            pub fn empty() -> Self {
                if $min > 0 {
                    panic!("{}::empty() is not valid", stringify!($name));
                }
                Self([0; $max], 0)
            }

            pub fn parse(input: &[u8]) -> IResult<&[u8], Self> {
                let (input, len) = be_u8(input)?;
                if len < $min as u8 || len > $max as u8 {
                    return Err(Err::Failure(Error::new(input, ErrorKind::LengthValue)));
                }
                let (input, data) = take(len as usize)(input)?;
                // unwrap() is ok because we check the size above.
                let instance = Self::try_new(data).unwrap();
                Ok((input, instance))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({:02x?})", stringify!($name), &self.0[..self.1])
            }
        }

        impl PartialEq for $name {
            fn eq(&self, other: &Self) -> bool {
                self.deref() == other.deref()
            }
        }

        impl Eq for $name {}

        impl Deref for $name {
            type Target = [u8];

            fn deref(&self) -> &Self::Target {
                &self.0[..self.1]
            }
        }

        impl<'a> TryFrom<&'a [u8]> for $name {
            type Error = InvalidLength;

            fn try_from(value: &'a [u8]) -> Result<Self, Self::Error> {
                Self::try_new(value)
            }
        }

        impl<'a> TryFrom<&'a str> for $name {
            type Error = InvalidLength;

            fn try_from(value: &'a str) -> Result<Self, Self::Error> {
                Self::try_new(value.as_bytes())
            }
        }
    };
}

var_array!(SessionId, 0, 32);
var_array!(Cookie, 0, 255);
