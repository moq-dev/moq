use crate::limits::DATAGRAM_WINDOW;

/// Datagram duplicate suppression: a 1024-bit sliding bitmask below the greatest opened sequence.
///
/// Only successful opens are marked, so a forged datagram never burns a real one.
/// Sequences below the window are unknown, not duplicates: an operational gap.
#[derive(Default)]
pub(crate) struct DatagramWindow {
	highest: Option<u64>,
	bits: [u64; DATAGRAM_WINDOW as usize / 64],
}

impl DatagramWindow {
	pub fn is_duplicate(&self, sequence: u64) -> bool {
		let Some(highest) = self.highest else {
			return false;
		};
		sequence <= highest && highest - sequence < DATAGRAM_WINDOW && self.bit(sequence)
	}

	pub fn mark(&mut self, sequence: u64) {
		if let Some(highest) = self.highest {
			if sequence > highest {
				// Slide forward, clearing the slots the new sequences will reuse.
				for cleared in highest + 1..=sequence.min(highest + DATAGRAM_WINDOW) {
					self.set(cleared, false);
				}
				self.highest = Some(sequence);
			} else if highest - sequence >= DATAGRAM_WINDOW {
				return;
			}
		} else {
			self.highest = Some(sequence);
		}
		self.set(sequence, true);
	}

	fn bit(&self, sequence: u64) -> bool {
		let slot = (sequence % DATAGRAM_WINDOW) as usize;
		self.bits[slot / 64] & (1 << (slot % 64)) != 0
	}

	fn set(&mut self, sequence: u64, value: bool) {
		let slot = (sequence % DATAGRAM_WINDOW) as usize;
		let mask = 1u64 << (slot % 64);
		if value {
			self.bits[slot / 64] |= mask;
		} else {
			self.bits[slot / 64] &= !mask;
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn slides_below_the_greatest_opened() {
		let mut window = DatagramWindow::default();
		assert!(!window.is_duplicate(0));
		window.mark(0);
		assert!(window.is_duplicate(0));
		for seq in 1..DATAGRAM_WINDOW {
			window.mark(seq);
		}
		assert!(window.is_duplicate(0));
		window.mark(DATAGRAM_WINDOW);
		assert!(!window.is_duplicate(0));
		assert!(window.is_duplicate(1));
		assert!(window.is_duplicate(DATAGRAM_WINDOW));
	}

	#[test]
	fn jump_clears_reused_slots() {
		let mut window = DatagramWindow::default();
		window.mark(5);
		window.mark(5 + DATAGRAM_WINDOW * 3);
		assert!(!window.is_duplicate(5));
		assert!(!window.is_duplicate(5 + DATAGRAM_WINDOW));
		assert!(!window.is_duplicate(5 + DATAGRAM_WINDOW * 2));
		assert!(window.is_duplicate(5 + DATAGRAM_WINDOW * 3));
	}

	#[test]
	fn late_within_window_marks() {
		let mut window = DatagramWindow::default();
		window.mark(100);
		assert!(!window.is_duplicate(90));
		window.mark(90);
		assert!(window.is_duplicate(90));
		window.mark(100 + DATAGRAM_WINDOW);
		window.mark(90);
		assert!(!window.is_duplicate(90));
	}
}
