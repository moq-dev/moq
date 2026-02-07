use crate::coding;

pub const ALPN_14: &str = "moq-00";
pub const ALPN_15: &str = "moqt-15";

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u64)]
pub enum Version {
	Draft14 = 0xff00000e,
	Draft15 = 0xff00000f,
}

impl TryFrom<coding::Version> for Version {
	type Error = ();

	fn try_from(value: coding::Version) -> Result<Self, Self::Error> {
		if value == Self::Draft14.into() {
			Ok(Self::Draft14)
		} else if value == Self::Draft15.into() {
			Ok(Self::Draft15)
		} else {
			Err(())
		}
	}
}

impl From<Version> for coding::Version {
	fn from(value: Version) -> Self {
		Self(value as u64)
	}
}
