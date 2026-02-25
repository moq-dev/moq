use crate::coding;

/// The ALPN string for the Lite protocol.
///
/// NOTE: The version is still negotiated via the setup message.
/// In the future we'll use ALPN instead.
pub const ALPN: &str = "moql";

/// The ALPN string for Draft03, which uses ALPN-based version negotiation.
pub const ALPN_03: &str = "moq-lite-03";

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u64)]
pub enum Version {
	Draft01 = 0xff0dad01,
	Draft02 = 0xff0dad02,
	Draft03 = 0xff0dad03,
}

impl TryFrom<coding::Version> for Version {
	type Error = ();

	fn try_from(value: coding::Version) -> Result<Self, Self::Error> {
		if value == Self::Draft01.coding() {
			Ok(Self::Draft01)
		} else if value == Self::Draft02.coding() {
			Ok(Self::Draft02)
		} else if value == Self::Draft03.coding() {
			Ok(Self::Draft03)
		} else {
			Err(())
		}
	}
}

impl From<Version> for coding::Version {
	fn from(value: Version) -> Self {
		value.coding()
	}
}

impl Version {
	pub const fn coding(self) -> coding::Version {
		coding::Version(self as u64)
	}
}
