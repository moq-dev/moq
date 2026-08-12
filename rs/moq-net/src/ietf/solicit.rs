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
//! The option is a plain Setup Option, so it rides every draft we speak rather than
//! needing the unified SETUP the MoQ Cluster extension does.

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

/// Declare that advertisements to us must be solicited.
///
/// Unconditional, and true of every session we open or accept: we send
/// SUBSCRIBE_NAMESPACE for every prefix we are allowed to discover, so there is nothing an
/// unsolicited PUBLISH_NAMESPACE could tell us that we will not have asked for. A peer
/// that honors it stops guessing which of the two we expect, which is the whole point.
pub fn into_setup(params: &mut super::Parameters, _version: Version) {
	params.set_varint(super::ParameterVarInt::Solicit, 1);
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
