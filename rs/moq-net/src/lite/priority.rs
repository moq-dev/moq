//! Priority conversion between the moq-lite wire and the MoQ model.
//!
//! The wire carries `priority + 128` (the top bit flipped): one-to-one and order
//! preserving, so a byte means what it always meant and the unset priority 0 is byte 128.

/// Convert a moq-lite priority byte into the model's priority.
pub(crate) const fn from_wire(priority: u8) -> i8 {
	(priority ^ 0x80) as i8
}

/// Convert the model's priority into a moq-lite priority byte.
pub(crate) const fn to_wire(priority: i8) -> u8 {
	(priority as u8) ^ 0x80
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn lite_priority_is_offset() {
		assert_eq!(to_wire(0), 128);
		assert_eq!(to_wire(i8::MIN), 0);
		assert_eq!(to_wire(i8::MAX), u8::MAX);
		assert_eq!(from_wire(128), 0);
		assert!((i8::MIN..i8::MAX).all(|priority| to_wire(priority) < to_wire(priority + 1)));
	}

	#[test]
	fn priority_round_trips() {
		assert!((i8::MIN..=i8::MAX).all(|priority| from_wire(to_wire(priority)) == priority));
		assert!((0u8..=u8::MAX).all(|priority| to_wire(from_wire(priority)) == priority));
	}
}
