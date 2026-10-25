//! Priority conversion between the IETF wire and the MoQ model.
//!
//! The wire is lower first, so `127 - priority` keeps one urgency with moq-lite's
//! `priority + 128`: the IETF byte is always `255 -` the lite byte. The unset priority 0
//! goes out as 127, one step off the draft's usual 128, to keep that.

/// The IETF priority an omitted SUBSCRIBER_PRIORITY or DEFAULT_PUBLISHER_PRIORITY stands for, as
/// the drafts define both, and the byte for a message that must carry a priority it was not given.
/// It reads as model priority -1, so a relay forwards an omitted priority as 128, not as unset 127.
pub(super) const DEFAULT_WIRE: u8 = 128;

/// Convert an IETF priority into the model's higher-first priority.
pub(super) const fn from_wire(priority: u8) -> i8 {
	(127 - priority as i16) as i8
}

/// Convert the model's higher-first priority into an IETF priority.
pub(super) const fn to_wire(priority: i8) -> u8 {
	(127 - priority as i16) as u8
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn ietf_priority_is_lower_first() {
		assert_eq!(from_wire(0), i8::MAX);
		assert_eq!(from_wire(u8::MAX), i8::MIN);
		assert_eq!(to_wire(i8::MAX), 0);
		assert_eq!(to_wire(i8::MIN), u8::MAX);
		assert!((0u8..u8::MAX).all(|priority| from_wire(priority) > from_wire(priority + 1)));
	}

	/// The unset default goes out as 127, and an omitted priority ranks one step below it.
	#[test]
	fn unset_is_127() {
		assert_eq!(to_wire(0), 127);
		assert_eq!(from_wire(127), 0);
		assert_eq!(from_wire(DEFAULT_WIRE), -1);
	}

	/// One urgency on both wires: the IETF byte mirrors the moq-lite byte.
	#[test]
	fn mirrors_lite() {
		for priority in i8::MIN..=i8::MAX {
			assert_eq!(to_wire(priority), u8::MAX - crate::lite::priority::to_wire(priority));
		}
	}

	#[test]
	fn priority_round_trips() {
		assert!((i8::MIN..=i8::MAX).all(|priority| from_wire(to_wire(priority)) == priority));
		assert!((0u8..=u8::MAX).all(|priority| to_wire(from_wire(priority)) == priority));
	}
}
