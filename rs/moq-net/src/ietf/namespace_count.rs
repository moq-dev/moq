//! The MoQ Namespace Count extension (draft-lcurley-moq-namespace-count-00).
//!
//! Nothing in moq-transport says where the NAMESPACE messages answering a
//! SUBSCRIBE_NAMESPACE stop replaying what already existed and start reporting changes,
//! so a subscriber can only guess from the stream going quiet. When both endpoints
//! declare the NAMESPACE_COUNT option, the publisher puts the size of that initial set on
//! its REQUEST_OK, the way moq-lite's ANNOUNCE_OK does.

use super::Version;

/// NAMESPACE_COUNT Setup Option: the sender counts its initial NAMESPACE messages and
/// reads the peer's count. Even, so the value is a bare varint.
pub const NAMESPACE_COUNT: u64 = 0x40B64;

/// NAMESPACE_COUNT REQUEST_OK parameter: how many NAMESPACE messages immediately follow.
/// Even, so the value is a bare varint.
pub const NAMESPACE_COUNT_PARAM: u64 = 0x40B66;

/// Whether this version answers SUBSCRIBE_NAMESPACE with NAMESPACE messages. Draft-14/15
/// answer with PUBLISH_NAMESPACE requests instead, which the stream cannot count.
fn supported(version: Version) -> bool {
	!matches!(version, Version::Draft14 | Version::Draft15)
}

/// Whether the peer declared the option. We always declare it, so this alone decides
/// whether the extension is negotiated.
pub fn from_setup(params: &super::Parameters, version: Version) -> bool {
	supported(version) && params.get_varint(super::ParameterVarInt::NamespaceCount).is_some()
}

/// Declare that we count our initial set and read the peer's.
pub fn into_setup(params: &mut super::Parameters, version: Version) {
	if supported(version) {
		params.set_varint(super::ParameterVarInt::NamespaceCount, 1);
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
