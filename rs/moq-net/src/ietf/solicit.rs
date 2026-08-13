//! The MoQ Solicit extension (draft-lcurley-moq-solicit-00).
//!
//! moq-transport carries no statement of what an endpoint intends to do with a session,
//! so neither side can tell whether the other will announce, ask, both, or neither. An
//! unsolicited PUBLISH_NAMESPACE is therefore a guess: either the only thing that will
//! ever tell the peer what we have, or noise it never wanted.
//!
//! This extension lets an endpoint declare, via the SOLICIT Setup Option, that
//! advertisements to it must be solicited first. Declaring nothing means "no requirement,
//! tell me unasked", which is what every peer that has never heard of this extension
//! implicitly says, so the default is the chatty, interoperable behavior.
//!
//! Settling that exposes a second gap the same draft closes. Once advertisements arrive as
//! NAMESPACE messages on a stream we opened, nothing separates the ones the publisher
//! already had from the ones that showed up a moment later, so a subscriber can see that a
//! namespace is present but never that it is absent. The NAMESPACE_COUNT Setup Option asks
//! for a NAMESPACE_COUNT parameter on each SUBSCRIBE_NAMESPACE response reporting the size
//! of that initial set. It is moq-lite's `ANNOUNCE_OK.Active Count` on this wire, and it
//! feeds the same [`crate::connecting::Connecting`] handle.
//!
//! The two are separate code points because a peer that implements only SOLICIT has never
//! heard of the parameter, and an unknown Message Parameter closes its session. That is
//! also why the count is negotiated at all, where the declaration above is not: a
//! declaration only ever asks for less.
//!
//! Both are plain Setup Options, so they ride every draft we speak rather than needing the
//! unified SETUP the MoQ Cluster extension does.

use crate::coding::DecodeError;

use super::Version;

/// SOLICIT Setup Option: whether advertisements to the sender must be solicited. Even, so
/// the value is a bare varint.
pub const SOLICIT: u64 = 0x40B5A;

/// What the peer declared, if anything.
///
/// The three states are distinct, and the difference between the last two is what makes
/// the requirement enforceable:
///
/// - `None`: no option at all. The peer has never heard of the extension, so it cannot
///   have honored ours and an unsolicited advertisement from it is expected.
/// - `Some(false)`: an explicit 0. No requirement of its own, but writing the option at
///   all proves it implements this, so it is held to ours.
/// - `Some(true)`: advertisements to it must be solicited, and likewise held to ours.
pub fn from_setup(params: &super::Parameters, _version: Version) -> Result<Option<bool>, DecodeError> {
	Ok(params
		.get_varint(super::ParameterVarInt::Solicit)
		.map(|value| value != 0))
}

/// Declare what we want: solicited advertisements, and a count on each response.
///
/// Both are unconditional and true of every session we open or accept. We send
/// SUBSCRIBE_NAMESPACE for every prefix we are allowed to discover, so there is nothing an
/// unsolicited PUBLISH_NAMESPACE could tell us that we will not have asked for, and every
/// one of those asks wants a boundary. A peer that honors them stops guessing which of the
/// two mechanisms we expect, which is the whole point.
pub fn into_setup(params: &mut super::Parameters, version: Version) {
	params.set_varint(super::ParameterVarInt::Solicit, 1);
	if count_supported(version) {
		params.set_varint(super::ParameterVarInt::NamespaceCount, 1);
	}
}

/// NAMESPACE_COUNT Setup Option: whether to report the size of each initial set. Even, so
/// the value is a bare varint.
pub const NAMESPACE_COUNT: u64 = 0x40B5C;

/// NAMESPACE_COUNT Message Parameter: the initial set size, carried on the REQUEST_OK
/// answering a SUBSCRIBE_NAMESPACE. Even, so the value is a bare varint.
pub const NAMESPACE_COUNT_PARAM: u64 = 0x40B5E;

/// Whether this version carries the initial set on the SUBSCRIBE_NAMESPACE response stream
/// at all.
///
/// Draft-14/15 answer with PUBLISH_NAMESPACE requests on streams of their own and have no
/// NAMESPACE message, so there is nothing on the response stream to count. Draft-14 also
/// answers with SUBSCRIBE_NAMESPACE_OK, which carries no parameters.
pub fn count_supported(version: Version) -> bool {
	!matches!(version, Version::Draft14 | Version::Draft15)
}

/// Whether the peer asked us to report the count.
///
/// Absent and 0 are the same request, unlike the declaration above: nothing here turns on
/// whether a peer that wants no count implements this.
pub fn count_from_setup(params: &super::Parameters, version: Version) -> Result<bool, DecodeError> {
	if !count_supported(version) {
		return Ok(false);
	}

	Ok(params
		.get_varint(super::ParameterVarInt::NamespaceCount)
		.is_some_and(|value| value != 0))
}

#[cfg(test)]
mod tests {
	use super::*;

	const VERSION: Version = Version::Draft19;

	/// What we declare is what a peer reads back, on every draft that carries Setup
	/// Options: an old peer can opt out too.
	#[test]
	fn every_version_round_trips() {
		for version in [Version::Draft14, Version::Draft15, Version::Draft16, Version::Draft19] {
			let mut params = super::super::Parameters::default();
			into_setup(&mut params, version);
			assert_eq!(from_setup(&params, version).unwrap(), Some(true));
		}
	}

	/// We ask for the count on every version that has a NAMESPACE to count, and on no
	/// other: draft-14/15 answer a SUBSCRIBE_NAMESPACE with PUBLISH_NAMESPACE requests, so
	/// the option would promise a count of messages that never arrive on that stream.
	#[test]
	fn the_count_rides_only_where_namespace_does() {
		for version in [Version::Draft16, Version::Draft17, Version::Draft18, Version::Draft19] {
			let mut params = super::super::Parameters::default();
			into_setup(&mut params, version);
			assert!(count_from_setup(&params, version).unwrap(), "version {version}");
		}

		for version in [Version::Draft14, Version::Draft15] {
			let mut params = super::super::Parameters::default();
			into_setup(&mut params, version);
			assert_eq!(params.get_varint(super::super::ParameterVarInt::NamespaceCount), None);
			assert!(!count_from_setup(&params, version).unwrap(), "version {version}");
		}
	}

	/// A peer that never heard of it asks for nothing, and must keep getting a response
	/// without the parameter: an unknown Message Parameter closes its session. An explicit
	/// 0 is the same request, since nothing here distinguishes the two.
	#[test]
	fn an_absent_or_zero_count_option_asks_for_nothing() {
		let params = super::super::Parameters::default();
		assert!(!count_from_setup(&params, VERSION).unwrap());

		let mut zero = super::super::Parameters::default();
		zero.set_varint(super::super::ParameterVarInt::NamespaceCount, 0);
		assert!(!count_from_setup(&zero, VERSION).unwrap());
	}

	/// A value this draft doesn't define still asks for the count, so a later revision that
	/// says more can't be read as saying nothing.
	#[test]
	fn any_non_zero_count_option_asks_for_the_count() {
		for value in [1, 2, 0x8000] {
			let mut params = super::super::Parameters::default();
			params.set_varint(super::super::ParameterVarInt::NamespaceCount, value);
			assert!(count_from_setup(&params, VERSION).unwrap(), "value {value}");
		}
	}

	/// A peer that declared nothing wants to be told unasked, which is what keeps this
	/// backward compatible with every implementation that has never heard of it.
	#[test]
	fn absent_declares_nothing() {
		let params = super::super::Parameters::default();
		assert_eq!(from_setup(&params, VERSION).unwrap(), None);
	}

	/// An explicit 0 asks for the same treatment an absent option does, but it is not the
	/// same statement: writing the option proves the peer implements this, which is what
	/// lets us hold it to our own declaration.
	#[test]
	fn zero_is_a_declaration_not_an_absence() {
		let mut params = super::super::Parameters::default();
		params.set_varint(super::super::ParameterVarInt::Solicit, 0);

		assert_eq!(from_setup(&params, VERSION).unwrap(), Some(false));
	}

	/// A value this draft doesn't define still means "ask me first", so a later revision
	/// that says more can't be read as saying nothing.
	#[test]
	fn any_non_zero_requires_solicitation() {
		for value in [1, 2, 0x8000] {
			let mut params = super::super::Parameters::default();
			params.set_varint(super::super::ParameterVarInt::Solicit, value);

			assert_eq!(from_setup(&params, VERSION).unwrap(), Some(true), "value {value}");
		}
	}
}
