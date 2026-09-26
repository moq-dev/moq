//! The MoQ Active Count extension (draft-lcurley-moq-active-count-00).
//!
//! A publisher answers SUBSCRIBE_NAMESPACE with a NAMESPACE for every suffix it already
//! advertises, then reports changes on the same stream. Nothing in moq-transport says
//! where the first part ends, so a subscriber can only guess from the stream going
//! quiet. When both endpoints declare the ACTIVE_COUNT option, the publisher puts on its
//! REQUEST_OK how many NAMESPACE messages come before the subscription is caught up.

use super::Version;

/// ACTIVE_COUNT Setup Option: the sender counts the NAMESPACE messages before a
/// subscription is caught up, and reads the peer's count. Even, so the value is a bare varint.
pub const ACTIVE_COUNT: u64 = 0x40B64;

/// ACTIVE_COUNT REQUEST_OK parameter: how many NAMESPACE messages immediately follow
/// before the subscription is caught up. Only answers SUBSCRIBE_NAMESPACE.
/// Even, so the value is a bare varint.
pub const ACTIVE_COUNT_PARAM: u64 = 0x40B66;

/// Whether this version answers SUBSCRIBE_NAMESPACE with NAMESPACE messages. Draft-14/15
/// answer with PUBLISH_NAMESPACE requests instead, which the stream cannot count.
fn supported(version: Version) -> bool {
	!matches!(version, Version::Draft14 | Version::Draft15)
}

/// Whether the peer declared the option. We always declare it, so this alone decides
/// whether the extension is negotiated.
pub fn from_setup(params: &super::Parameters, version: Version) -> bool {
	supported(version) && params.get_varint(super::ParameterVarInt::ActiveCount).is_some()
}

/// Declare that we count and read the peer's count.
pub fn into_setup(params: &mut super::Parameters, version: Version) {
	if supported(version) {
		params.set_varint(super::ParameterVarInt::ActiveCount, 1);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn only_versions_with_namespace_negotiate() {
		for version in [Version::Draft14, Version::Draft15, Version::Draft16, Version::Draft22] {
			let mut params = super::super::Parameters::default();
			assert!(!from_setup(&params, version));
			into_setup(&mut params, version);
			assert_eq!(from_setup(&params, version), supported(version), "{version:?}");
		}
	}
}
