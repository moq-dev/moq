//! A relay's NEW_GROUP_REQUEST forwarding (moq-transport draft-15 section 9.2.1.12 through
//! draft-22 section 9.20.19, unchanged across them).
//!
//! Downstream subscribers' requests reach one upstream subscription as the highest one any
//! of them holds. On an Established subscription to a track with DYNAMIC_GROUPS, that goes
//! upstream in a REQUEST_UPDATE unless it names a group the relay already has, or a request
//! still outstanding covers it. A request stays outstanding until the Largest Group passes
//! where it stood when the request was sent: the publisher acknowledging the update says
//! nothing about whether a new group exists.

/// The NEW_GROUP_REQUEST outstanding on one upstream subscription. Dropped with it, so a
/// reconnect or another route starts with none.
#[derive(Debug, Default)]
pub(super) struct Outstanding(Option<Sent>);

#[derive(Debug, Clone, Copy)]
struct Sent {
	value: u64,
	/// The Largest Group when it was sent; the request is answered once that grows.
	largest: Option<u64>,
}

impl Outstanding {
	/// Record the request a SUBSCRIBE carried, which the draft has a relay without an
	/// Established subscription include unconditionally. `largest` is what its
	/// SUBSCRIBE_OK said, the first Largest Group this subscription knows.
	pub fn subscribed(&mut self, value: u64, largest: Option<u64>) {
		self.0 = Some(Sent { value, largest });
	}

	/// Whether a request for `value` goes upstream now, given the track's `largest` Group,
	/// recording it as outstanding when it does. The caller checked the track supports
	/// dynamic groups.
	pub fn forward(&mut self, value: u64, largest: Option<u64>) -> bool {
		if self.0.is_some_and(|sent| largest > sent.largest) {
			self.0 = None;
		}
		// A nonzero value at or below the Largest Group names a group that already exists.
		if value != 0 && largest.is_some_and(|largest| value <= largest) {
			return false;
		}
		let request = Sent { value, largest };
		if self.0.is_some_and(|sent| sent.reach() >= request.reach()) {
			return false;
		}
		self.0 = Some(request);
		true
	}
}

impl Sent {
	/// The highest request this one covers. A 0 asks for any group past the Largest Group
	/// it was sent at, the same as naming the one after it.
	fn reach(self) -> u64 {
		match (self.value, self.largest) {
			(0, Some(largest)) => largest.saturating_add(1),
			(value, _) => value,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_request_past_the_largest_group_is_forwarded() {
		let mut outstanding = Outstanding::default();
		assert!(outstanding.forward(6, Some(5)));
	}

	#[test]
	fn zero_is_forwarded_whatever_the_largest_group() {
		assert!(Outstanding::default().forward(0, None));
		assert!(Outstanding::default().forward(0, Some(5)));
	}

	#[test]
	fn a_request_for_an_existing_group_is_stale() {
		let mut outstanding = Outstanding::default();
		assert!(!outstanding.forward(5, Some(5)));
		assert!(!outstanding.forward(1, Some(5)));
		// Nothing was sent, so nothing is outstanding.
		assert!(outstanding.forward(6, Some(5)));
	}

	#[test]
	fn an_outstanding_request_covers_lower_and_equal_ones() {
		let mut outstanding = Outstanding::default();
		assert!(outstanding.forward(6, Some(5)));
		assert!(!outstanding.forward(6, Some(5)));
		assert!(!outstanding.forward(0, Some(5)));
	}

	#[test]
	fn zero_covers_the_group_after_the_largest() {
		let mut outstanding = Outstanding::default();
		assert!(outstanding.forward(0, Some(5)));
		assert!(!outstanding.forward(6, Some(5)));
		assert!(outstanding.forward(7, Some(5)));
	}

	#[test]
	fn a_higher_request_is_forwarded_past_an_outstanding_one() {
		let mut outstanding = Outstanding::default();
		assert!(outstanding.forward(6, Some(5)));
		assert!(outstanding.forward(7, Some(5)));
		assert!(!outstanding.forward(7, Some(5)));
	}

	#[test]
	fn a_new_group_rearms_requests() {
		let mut outstanding = Outstanding::default();
		assert!(outstanding.forward(0, Some(5)));
		assert!(!outstanding.forward(0, Some(5)));
		// The Largest Group grew, so the request was answered.
		assert!(outstanding.forward(0, Some(6)));
		assert!(outstanding.forward(8, Some(6)));
	}

	#[test]
	fn the_first_group_answers_a_request_sent_before_any() {
		let mut outstanding = Outstanding::default();
		assert!(outstanding.forward(0, None));
		assert!(!outstanding.forward(0, None));
		assert!(outstanding.forward(0, Some(0)));
	}

	#[test]
	fn a_stale_subscribe_request_covers_no_zero() {
		let mut outstanding = Outstanding::default();
		// SUBSCRIBE_OK showed group 5 already existed, so the request asked for nothing.
		outstanding.subscribed(5, Some(7));
		assert!(outstanding.forward(0, Some(7)));
	}

	#[test]
	fn a_subscribe_request_is_outstanding_from_its_answer() {
		let mut outstanding = Outstanding::default();
		outstanding.subscribed(0, Some(5));
		assert!(!outstanding.forward(0, Some(5)));
		assert!(outstanding.forward(0, Some(6)));
	}
}
