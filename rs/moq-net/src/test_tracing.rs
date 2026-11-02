//! One tracing subscriber shared by all unit tests.

use std::cell::RefCell;
use std::fmt::Write;
use std::sync::Once;

use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Interest;
use tracing::{Event, Level, Metadata, Subscriber};

/// Only the current thread's events at or below `level` are captured.
struct Capture {
	level: Level,
	events: Vec<String>,
}

thread_local! {
	static CAPTURE: RefCell<Option<Capture>> = const { RefCell::new(None) };
}

/// Clears this thread's capture on unwind so the next one is not nested into a stale slot.
struct Clear;

impl Drop for Clear {
	fn drop(&mut self) {
		let _ = CAPTURE.take();
	}
}

/// Capture this thread's events while running `f`, including their formatted fields.
///
/// All tests share one global subscriber so parallel captures cannot compete to install it
/// or disable each other's callsites. The thread-local slot isolates each capture.
pub(crate) fn capture(level: Level, f: impl FnOnce()) -> Vec<String> {
	static INSTALL: Once = Once::new();
	INSTALL.call_once(|| {
		tracing::subscriber::set_global_default(Events).expect("no other global subscriber");
	});
	// A callsite first reached while the install was still publishing may have cached no interest.
	tracing::callsite::rebuild_interest_cache();

	let prev = CAPTURE.replace(Some(Capture {
		level,
		events: Vec::new(),
	}));
	assert!(prev.is_none(), "tracing capture does not nest");
	let _clear = Clear;
	f();
	CAPTURE.take().expect("capture still installed").events
}

/// Count this thread's warnings containing `expected_message` while running `f`.
pub(crate) fn count_drop_warnings(expected_message: &str, f: impl FnOnce()) -> usize {
	capture(Level::WARN, f)
		.iter()
		.filter(|event| event.contains(expected_message))
		.count()
}

/// Routes events to the capture on the emitting thread, if any.
struct Events;

impl Subscriber for Events {
	fn register_callsite(&self, _metadata: &'static Metadata<'static>) -> Interest {
		// Capture level and presence vary by thread, so neither is a callsite-wide decision.
		Interest::sometimes()
	}

	fn enabled(&self, metadata: &Metadata<'_>) -> bool {
		CAPTURE.with_borrow(|capture| {
			capture
				.as_ref()
				.is_some_and(|capture| *metadata.level() <= capture.level)
		})
	}

	fn max_level_hint(&self) -> Option<LevelFilter> {
		Some(LevelFilter::TRACE)
	}

	fn new_span(&self, _span: &Attributes<'_>) -> Id {
		Id::from_u64(1)
	}

	fn record(&self, _span: &Id, _values: &Record<'_>) {}

	fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

	fn event(&self, event: &Event<'_>) {
		CAPTURE.with_borrow_mut(|capture| {
			let Some(capture) = capture else { return };
			if *event.metadata().level() > capture.level {
				return;
			}
			let mut fields = Fields(String::new());
			event.record(&mut fields);
			capture.events.push(fields.0);
		});
	}

	fn enter(&self, _span: &Id) {}

	fn exit(&self, _span: &Id) {}
}

struct Fields(String);

impl Visit for Fields {
	fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
		write!(self.0, "{}={value:?} ", field.name()).unwrap();
	}
}

mod tests {
	use super::{capture, count_drop_warnings};
	use tracing::Level;

	fn probe() {
		tracing::warn!("test_tracing probe");
	}

	/// Captures on separate threads share callsite interest, but not events or level filters.
	#[test]
	fn counts_only_this_threads_warns() {
		let warns = count_drop_warnings("test_tracing probe", || {
			std::thread::spawn(|| {
				let events = capture(Level::TRACE, || {
					tracing::trace!(answer = 42, "trace fields");
					probe();
				});
				assert_eq!(events.len(), 2);
				assert!(events[0].contains("answer=42"));
				assert!(events[0].contains("trace fields"));
			})
			.join()
			.unwrap();
			tracing::trace!("test_tracing probe");
			probe();
		});
		assert_eq!(warns, 1);
	}

	/// A panic inside the capture must clear the slot, or the next capture on this thread looks nested.
	#[test]
	fn an_unwind_clears_the_capture() {
		let panicked = std::panic::catch_unwind(|| {
			count_drop_warnings("test_tracing probe", || panic!("unwind the capture"));
		});
		assert!(panicked.is_err());

		let warns = count_drop_warnings("test_tracing probe", probe);
		assert_eq!(warns, 1);
	}
}
