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

/// Whether the peer requires advertisements to be solicited, from its SETUP.
///
/// Absent is the same as 0, which is what a peer unaware of the extension declares: no
/// requirement, so we advertise unasked. Any non-zero value means it will ask.
pub fn from_setup(params: &super::Parameters, _version: Version) -> Result<bool, DecodeError> {
	Ok(params
		.get_varint(super::ParameterVarInt::Solicit)
		.is_some_and(|value| value != 0))
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
			assert!(from_setup(&params, version).unwrap());
		}
	}

	/// A peer that declared nothing wants to be told unasked, which is what keeps this
	/// backward compatible with every implementation that has never heard of it.
	#[test]
	fn absent_requires_nothing() {
		let params = super::super::Parameters::default();
		assert!(!from_setup(&params, VERSION).unwrap());
	}

	/// An explicit 0 says exactly what an absent option says.
	#[test]
	fn zero_requires_nothing() {
		let mut params = super::super::Parameters::default();
		params.set_varint(super::super::ParameterVarInt::Solicit, 0);

		assert!(!from_setup(&params, VERSION).unwrap());
	}

	/// A value this draft doesn't define still means "ask me first", so a later revision
	/// that says more can't be read as saying nothing.
	#[test]
	fn any_non_zero_requires_solicitation() {
		for value in [1, 2, 0x8000] {
			let mut params = super::super::Parameters::default();
			params.set_varint(super::super::ParameterVarInt::Solicit, value);

			assert!(from_setup(&params, VERSION).unwrap(), "value {value}");
		}
	}
}
