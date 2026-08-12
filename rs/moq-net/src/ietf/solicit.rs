//! The MoQ Solicit extension (draft-lcurley-moq-solicit-00).
//!
//! moq-transport carries no statement of what an endpoint intends to do with a session,
//! so neither side can tell whether the other will announce, ask, both, or neither. An
//! unsolicited PUBLISH_NAMESPACE is therefore a guess: either the only thing that will
//! ever tell the peer what we have, or noise it never wanted.
//!
//! This extension lets each endpoint declare, via the SOLICIT Setup Option, that
//! advertisements to it must be solicited first. Declaring nothing means "no
//! requirements, tell me unasked", which is what every peer that has never heard of this
//! extension implicitly says, so the default is the chatty, interoperable behavior.
//!
//! The option is a plain Setup Option, so it rides every draft we speak rather than
//! needing the unified SETUP the MoQ Cluster extension does.

use crate::coding::DecodeError;
use crate::origin;

use super::Version;

/// SOLICIT Setup Option: the sender's solicitation requirements. Even, so the value is a
/// bare varint.
pub const SOLICIT: u64 = 0x40B5A;

/// ANNOUNCE: advertisements to the sender must be solicited, since it asks for what it
/// wants with SUBSCRIBE_NAMESPACE.
const ANNOUNCE: u64 = 0x1;

/// What an endpoint requires to be solicited.
///
/// Defaults to `false`, which is what an endpoint that declared nothing gets: no
/// requirements, so tell it everything. The flag is advisory, so sending a message it
/// asked to be spared is rude rather than fatal, and a receiver handles one normally.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Solicit {
	/// Advertisements must be solicited: this endpoint asks for what it wants with
	/// SUBSCRIBE_NAMESPACE, so an unsolicited PUBLISH_NAMESPACE is unwanted. A relay
	/// declares this; so does an endpoint that can do nothing with an announcement.
	pub announce: bool,
}

impl Solicit {
	/// The flags as they go on the wire. Unknown bits are never set by us and are
	/// ignored on the way in, so a future flag stays additive.
	fn bits(&self) -> u64 {
		let mut bits = 0;
		if self.announce {
			bits |= ANNOUNCE;
		}
		bits
	}

	fn from_bits(bits: u64) -> Self {
		Self {
			announce: bits & ANNOUNCE != 0,
		}
	}
}

/// What we require of the peer, given the half of the session that could use an
/// advertisement.
///
/// Exactly what we could do nothing with: with no subscribe half an announcement is
/// useless to us. A relay has one, so it requires nothing and stays as talkative as every
/// peer expects.
pub fn from_subscribe(subscribe: Option<&origin::Producer>) -> Solicit {
	Solicit {
		announce: subscribe.is_none_or(|origin| origin.allowed().next().is_none()),
	}
}

/// Read the SOLICIT Setup Option out of a decoded SETUP parameter block.
pub fn from_setup(params: &super::Parameters, _version: Version) -> Result<Solicit, DecodeError> {
	Ok(params
		.get_varint(super::ParameterVarInt::Solicit)
		.map(Solicit::from_bits)
		.unwrap_or_default())
}

/// Write our SOLICIT Setup Option into a SETUP parameter block.
///
/// No requirements is the absent case, so it writes no option at all rather than a zero
/// a peer would have to decode.
pub fn into_setup(params: &mut super::Parameters, solicit: Solicit, _version: Version) {
	let bits = solicit.bits();
	if bits != 0 {
		params.set_varint(super::ParameterVarInt::Solicit, bits);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const VERSION: Version = Version::Draft19;

	fn round_trip(solicit: Solicit, version: Version) -> Solicit {
		let mut params = super::super::Parameters::default();
		into_setup(&mut params, solicit, version);
		from_setup(&params, version).unwrap()
	}

	#[test]
	fn flags_round_trip() {
		for announce in [false, true] {
			let solicit = Solicit { announce };
			assert_eq!(round_trip(solicit, VERSION), solicit);
		}
	}

	/// A peer that declared nothing wants everything, which is what keeps this
	/// backward compatible with every implementation that has never heard of it.
	#[test]
	fn absent_requires_nothing() {
		let params = super::super::Parameters::default();
		assert_eq!(from_setup(&params, VERSION).unwrap(), Solicit::default());
	}

	/// No requirements must not write the option, or every session would carry a zero
	/// that says exactly what its absence already says.
	#[test]
	fn empty_writes_no_option() {
		let mut params = super::super::Parameters::default();
		into_setup(&mut params, Solicit::default(), VERSION);
		assert_eq!(params.get_varint(super::super::ParameterVarInt::Solicit), None);
	}

	/// A flag we don't know yet must not disturb the ones we do.
	#[test]
	fn unknown_bits_are_ignored() {
		let mut params = super::super::Parameters::default();
		params.set_varint(super::super::ParameterVarInt::Solicit, ANNOUNCE | 0x8000);

		assert_eq!(from_setup(&params, VERSION).unwrap(), Solicit { announce: true });
	}

	/// What we declare comes from the session itself, so nothing has to be configured:
	/// a relay wired up to subscribe stays as talkative as every peer expects, while a
	/// publish-only client opts out of the advertisements it could never use.
	#[test]
	fn declaration_follows_the_wired_halves() {
		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();

		assert_eq!(
			from_subscribe(Some(&origin)),
			Solicit::default(),
			"a relay requires nothing"
		);

		assert_eq!(
			from_subscribe(None),
			Solicit { announce: true },
			"nothing to subscribe with: don't announce at me"
		);

		// A half that permits no prefix at all is the same as an absent one, which is
		// what a session fills an unset half with.
		let none = origin::Producer::empty(crate::Origin::random());
		assert_eq!(
			from_subscribe(Some(&none)),
			Solicit { announce: true },
			"a scope that permits nothing can't use an announcement either"
		);
	}

	/// Every draft we speak carries Setup Options, so the option rides all of them:
	/// an old peer that wants to opt out still can.
	#[test]
	fn every_version_round_trips() {
		let solicit = Solicit { announce: true };

		for version in [Version::Draft14, Version::Draft15, Version::Draft16, Version::Draft19] {
			assert_eq!(round_trip(solicit, version), solicit);
		}
	}
}
