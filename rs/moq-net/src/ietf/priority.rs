//! Priority conversion between the IETF wire and the MoQ model.
//!
//! The wire is lower first, so `127 - priority` keeps one urgency with moq-lite's
//! `priority + 128`: the IETF byte is always `255 -` the lite byte. The unset priority 0
//! goes out as 127, one step off the draft's usual 128, to keep that.

/// The model priority an absent subscriber priority stands for: the unset default.
pub(super) const ABSENT: i8 = 0;

/// The wire byte of [`ABSENT`], for a message that must carry a priority it was not given.
pub(super) const ABSENT_WIRE: u8 = to_wire(ABSENT);

/// The publisher priority of a track whose publisher omits DEFAULT_PUBLISHER_PRIORITY
/// (draft-16 section 11.1.1.1 on). Reading it as [`ABSENT`] would forward that track at 127.
pub(super) const DEFAULT_PUBLISHER_WIRE: u8 = 128;

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

	/// The unset default goes out as 127, and an absent priority decodes to it.
	#[test]
	fn unset_is_127() {
		assert_eq!(to_wire(0), 127);
		assert_eq!(from_wire(127), 0);
		assert_eq!(ABSENT, 0);
		assert_eq!(ABSENT_WIRE, 127);
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
