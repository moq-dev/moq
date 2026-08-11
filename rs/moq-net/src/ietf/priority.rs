//! Priority conversion between the IETF wire and the MoQ model.

/// Convert an IETF subscriber priority into the model's higher-first priority.
pub(super) const fn from_wire(priority: u8) -> u8 {
	u8::MAX - priority
}

/// Convert the model's higher-first priority into an IETF subscriber priority.
pub(super) const fn to_wire(priority: u8) -> u8 {
	u8::MAX - priority
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn ietf_priority_is_lower_first() {
		assert_eq!(from_wire(0), u8::MAX);
		assert_eq!(from_wire(u8::MAX), 0);
		assert_eq!(to_wire(u8::MAX), 0);
		assert_eq!(to_wire(0), u8::MAX);
		assert!((0u8..u8::MAX).all(|priority| from_wire(priority) > from_wire(priority + 1)));
	}

	#[test]
	fn priority_round_trips() {
		assert!((0u8..=u8::MAX).all(|priority| from_wire(to_wire(priority)) == priority));
	}
}
