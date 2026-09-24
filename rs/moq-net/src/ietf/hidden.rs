//! The MoQ Hidden extension (draft-lcurley-moq-hidden-00).
//!
//! A namespace with a segment starting with `.` below the prefix a subscription asked
//! for is hidden: it is left out of discovery unless the subscription opts in. The
//! opt-in is the HIDDEN parameter on SUBSCRIBE_NAMESPACE, and since an unknown
//! parameter fails decoding, it is only sent to a peer whose SETUP carried the HIDDEN
//! option. The rule itself is the one moq-lite applies (see
//! [`crate::origin::Consumer::with_hidden`]).

use super::Version;

/// HIDDEN Setup Option: the sender understands the HIDDEN parameter. Even, so the value
/// is a bare varint.
pub const HIDDEN: u64 = 0x40B5C;

/// HIDDEN SUBSCRIBE_NAMESPACE parameter: also advertise hidden namespaces. Even, so the
/// value is a bare varint.
pub const HIDDEN_PARAM: u64 = 0x40B5E;

/// Whether the peer understands the HIDDEN parameter.
pub fn from_setup(params: &super::Parameters, _version: Version) -> bool {
	params.get_varint(super::ParameterVarInt::Hidden).is_some()
}

/// Declare that we understand the HIDDEN parameter.
pub fn into_setup(params: &mut super::Parameters, _version: Version) {
	params.set_varint(super::ParameterVarInt::Hidden, 1);
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn every_version_round_trips() {
		for version in [Version::Draft14, Version::Draft16, Version::Draft17, Version::Draft22] {
			let mut params = super::super::Parameters::default();
			assert!(!from_setup(&params, version));
			into_setup(&mut params, version);
			assert!(from_setup(&params, version));
		}
	}
}
