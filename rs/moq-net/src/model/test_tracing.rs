//! Tracing helpers shared by model tests.

use std::cell::RefCell;
use std::sync::Once;

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

/// The capture active on this thread: the message to match and how many WARNs matched.
struct Capture {
	expected: String,
	hits: usize,
}

thread_local! {
	static CAPTURE: RefCell<Option<Capture>> = const { RefCell::new(None) };
}

/// Count WARN events emitted on this thread whose `message` field contains `expected_message`
/// while running `f`.
///
/// The subscriber is process-global rather than scoped with `with_default`: tracing caches each
/// callsite's interest process-wide, so a parallel test reaching the WARN first on a thread
/// without a scoped subscriber disables it for every thread. Counting per thread keeps another
/// test's WARNs out.
pub(crate) fn count_drop_warnings(expected_message: &str, f: impl FnOnce()) -> usize {
	static INSTALL: Once = Once::new();
	INSTALL.call_once(|| tracing::subscriber::set_global_default(Warns).expect("no other global subscriber"));
	// A callsite first reached on another thread while the install above was still publishing
	// sees no subscriber and is cached as disabled. Every capture starts after the install has
	// completed, so rebuilding here re-enables it against the now-global subscriber.
	tracing::callsite::rebuild_interest_cache();

	let capture = Capture {
		expected: expected_message.to_owned(),
		hits: 0,
	};
	let prev = CAPTURE.replace(Some(capture));
	assert!(prev.is_none(), "count_drop_warnings does not nest");

	f();

	CAPTURE.take().expect("capture still installed").hits
}

/// Routes every WARN to the capture on the emitting thread, if any.
struct Warns;

impl Subscriber for Warns {
	fn enabled(&self, metadata: &Metadata<'_>) -> bool {
		*metadata.level() == Level::WARN
	}

	fn new_span(&self, _span: &Attributes<'_>) -> Id {
		Id::from_u64(1)
	}

	fn record(&self, _span: &Id, _values: &Record<'_>) {}

	fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

	fn event(&self, event: &Event<'_>) {
		CAPTURE.with_borrow_mut(|capture| {
			let Some(capture) = capture else { return };
			let mut msg = Msg {
				expected: &capture.expected,
				matched: false,
			};
			event.record(&mut msg);
			if msg.matched {
				capture.hits += 1;
			}
		});
	}

	fn enter(&self, _span: &Id) {}

	fn exit(&self, _span: &Id) {}
}

struct Msg<'a> {
	expected: &'a str,
	matched: bool,
}

impl Visit for Msg<'_> {
	fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
		if field.name() == "message" && format!("{value:?}").contains(self.expected) {
			self.matched = true;
		}
	}

	fn record_str(&mut self, field: &Field, value: &str) {
		if field.name() == "message" && value.contains(self.expected) {
			self.matched = true;
		}
	}
}

mod tests {
	use super::count_drop_warnings;

	fn probe() {
		tracing::warn!("test_tracing probe");
	}

	/// Another thread without a capture reaching the WARN first must neither disable it for this
	/// thread nor be counted here.
	#[test]
	fn counts_only_this_threads_warns() {
		let warns = count_drop_warnings("test_tracing probe", || {
			std::thread::spawn(probe).join().unwrap();
			probe();
		});
		assert_eq!(warns, 1);
	}
}
