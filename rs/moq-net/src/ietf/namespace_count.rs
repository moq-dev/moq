//! The MoQ Namespace Count extension (draft-lcurley-moq-namespace-count-00).
//!
//! moq-transport answers a SUBSCRIBE_NAMESPACE with a REQUEST_OK and then a NAMESPACE per
//! matching namespace, with nothing separating the ones the publisher already had from the
//! ones that showed up a moment later. A subscriber can see that a namespace is present,
//! but never that it is absent, which is the answer `origin::Consumer::request_broadcast`
//! needs before it falls back to a dynamic handler.
//!
//! This extension puts the size of that initial set in the response, so the set is complete
//! once that many NAMESPACE messages have arrived. It is moq-lite's `ANNOUNCE_OK.Active
//! Count` on the moq-transport wire, and it feeds the same [`crate::connecting::Connecting`] handle.
//!
//! Unknown Message Parameters close the session, so unlike [`super::solicit`] this one has
//! to be negotiated: we only send the count to a peer whose SETUP asked for it.
//!
//! The draft requires a subscriber to bound how long it waits for a set to complete, since
//! a peer that counts more than it sends would hold it forever. That bound is the connect
//! deadline (`moq_native::ClientConfig::timeout`, 30s by default), which covers the dial
//! and the whole handshake.

use crate::coding::DecodeError;

use super::Version;

/// NAMESPACE_COUNT Setup Option: whether to report the initial set size. Even, so the
/// value is a bare varint.
pub const NAMESPACE_COUNT_OPTION: u64 = 0x40B5C;

/// NAMESPACE_COUNT Message Parameter: the initial set size, carried on the REQUEST_OK
/// answering a SUBSCRIBE_NAMESPACE. Even, so the value is a bare varint.
pub const NAMESPACE_COUNT_PARAM: u64 = 0x40B5E;

/// Whether this version carries the initial set on the SUBSCRIBE_NAMESPACE response stream
/// at all.
///
/// Draft-14/15 answer with PUBLISH_NAMESPACE requests on streams of their own and have no
/// NAMESPACE message, so there is nothing on the response stream to count. Draft-14 also
/// answers with SUBSCRIBE_NAMESPACE_OK, which carries no parameters.
pub fn supported(version: Version) -> bool {
	!matches!(version, Version::Draft14 | Version::Draft15)
}

/// Whether the peer asked us to report the count.
///
/// Absent or 0 means no, and the two are the same statement: unlike
/// [`super::solicit`], nothing here changes based on whether a peer that wants nothing
/// implements the extension.
pub fn from_setup(params: &super::Parameters, version: Version) -> Result<bool, DecodeError> {
	if !supported(version) {
		return Ok(false);
	}

	Ok(params
		.get_varint(super::ParameterVarInt::NamespaceCount)
		.is_some_and(|value| value != 0))
}

/// Ask the peer to report the count.
///
/// Unconditional on every version that can answer, since we open a SUBSCRIBE_NAMESPACE for
/// every prefix we are allowed to discover and each one wants a boundary.
pub fn into_setup(params: &mut super::Parameters, version: Version) {
	if supported(version) {
		params.set_varint(super::ParameterVarInt::NamespaceCount, 1);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// What we ask for is what a peer reads back, on every version that has a NAMESPACE to
	/// count.
	#[test]
	fn supported_versions_round_trip() {
		for version in [Version::Draft16, Version::Draft17, Version::Draft18, Version::Draft19] {
			let mut params = super::super::Parameters::default();
			into_setup(&mut params, version);
			assert!(from_setup(&params, version).unwrap(), "version {version}");
		}
	}

	/// Draft-14/15 have no NAMESPACE message, so we neither ask nor answer: the option
	/// would promise a count of messages that never arrive on that stream.
	#[test]
	fn pre_namespace_versions_declare_nothing() {
		for version in [Version::Draft14, Version::Draft15] {
			let mut params = super::super::Parameters::default();
			into_setup(&mut params, version);
			assert_eq!(params.get_varint(super::super::ParameterVarInt::NamespaceCount), None);
			assert!(!from_setup(&params, version).unwrap(), "version {version}");
		}
	}

	/// A peer that declared nothing gets nothing, which is what keeps this backward
	/// compatible: sending the parameter unasked would close its session.
	#[test]
	fn absent_asks_for_nothing() {
		let params = super::super::Parameters::default();
		assert!(!from_setup(&params, Version::Draft19).unwrap());
	}

	/// An explicit 0 is the same request as no option at all.
	#[test]
	fn zero_asks_for_nothing() {
		let mut params = super::super::Parameters::default();
		params.set_varint(super::super::ParameterVarInt::NamespaceCount, 0);
		assert!(!from_setup(&params, Version::Draft19).unwrap());
	}

	/// A value this draft doesn't define still asks for the count, so a later revision that
	/// says more can't be read as saying nothing.
	#[test]
	fn any_non_zero_asks_for_the_count() {
		for value in [1, 2, 0x8000] {
			let mut params = super::super::Parameters::default();
			params.set_varint(super::super::ParameterVarInt::NamespaceCount, value);
			assert!(from_setup(&params, Version::Draft19).unwrap(), "value {value}");
		}
	}
}
