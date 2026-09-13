use std::collections::HashSet;

use crate::error::{Error, Result};
use crate::limits::{DATAGRAM_DUPLICATE_WINDOW, GROUP_DUPLICATE_GROUPS};

/// Bounded grouped-frame duplicate window: the current group plus the previous group.
#[derive(Default)]
pub(crate) struct GroupWindow {
	slots: Vec<GroupFrames>,
}

struct GroupFrames {
	sequence: u64,
	frames: HashSet<u32>,
}

impl GroupWindow {
	pub fn check(&mut self, group: u64, frame: u32) -> Result<()> {
		if let Some(slot) = self.slots.iter_mut().find(|slot| slot.sequence == group) {
			if !slot.frames.insert(frame) {
				return Err(Error::Duplicate);
			}
			return Ok(());
		}
		if self.slots.len() == GROUP_DUPLICATE_GROUPS {
			self.slots.remove(0);
		}
		self.slots.push(GroupFrames {
			sequence: group,
			frames: HashSet::from([frame]),
		});
		Ok(())
	}
}

/// Bounded datagram duplicate window: a 1024-sequence sliding window.
#[derive(Default)]
pub(crate) struct DatagramWindow {
	highest: Option<u64>,
	seen: HashSet<u64>,
}

impl DatagramWindow {
	pub fn is_duplicate(&self, sequence: u64) -> bool {
		if let Some(highest) = self.highest {
			if highest >= DATAGRAM_DUPLICATE_WINDOW as u64 && sequence <= highest - DATAGRAM_DUPLICATE_WINDOW as u64 {
				// Outside the retained window: operational gap, not a cryptographic event.
				return false;
			}
			if self.seen.contains(&sequence) {
				return true;
			}
		}
		false
	}

	pub fn mark(&mut self, sequence: u64) {
		self.seen.insert(sequence);
		self.highest = Some(self.highest.map_or(sequence, |h| h.max(sequence)));
		if let Some(highest) = self.highest
			&& highest >= DATAGRAM_DUPLICATE_WINDOW as u64
		{
			let floor = highest - DATAGRAM_DUPLICATE_WINDOW as u64;
			self.seen.retain(|&s| s > floor);
		}
	}

	#[cfg(test)]
	pub fn check(&mut self, sequence: u64) -> Result<()> {
		if self.is_duplicate(sequence) {
			return Err(Error::Duplicate);
		}
		self.mark(sequence);
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn group_window_current_and_previous() {
		let mut window = GroupWindow::default();
		window.check(1, 0).unwrap();
		window.check(1, 1).unwrap();
		window.check(2, 0).unwrap();
		assert_eq!(window.check(1, 0).unwrap_err().code(), "duplicate");
		window.check(1, 2).unwrap();
		window.check(3, 0).unwrap();
		window.check(1, 0).unwrap();
	}

	#[test]
	fn datagram_window_slides() {
		let mut window = DatagramWindow::default();
		window.check(0).unwrap();
		assert_eq!(window.check(0).unwrap_err().code(), "duplicate");
		for seq in 1..=DATAGRAM_DUPLICATE_WINDOW as u64 {
			window.check(seq).unwrap();
		}
		window.check(0).unwrap();
	}
}
