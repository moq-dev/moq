//! Tracing helpers shared by model tests.

use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

/// Count WARN events whose `message` field contains `expected_message` while running `f`.
///
/// Uses only the existing `tracing` dependency (no tracing-subscriber).
pub(crate) fn count_drop_warnings(expected_message: &str, f: impl FnOnce()) -> usize {
	struct Count {
		hits: Arc<AtomicUsize>,
		expected: String,
	}

	struct Msg<'a> {
		expected: &'a str,
		matched: bool,
	}

	impl Visit for Msg<'_> {
		fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
			if field.name() == "message" {
				let s = format!("{value:?}");
				if s.contains(self.expected) {
					self.matched = true;
				}
			}
		}

		fn record_str(&mut self, field: &Field, value: &str) {
			if field.name() == "message" && value.contains(self.expected) {
				self.matched = true;
			}
		}
	}

	impl Subscriber for Count {
		fn enabled(&self, metadata: &Metadata<'_>) -> bool {
			*metadata.level() <= Level::WARN
		}

		fn new_span(&self, _span: &Attributes<'_>) -> Id {
			Id::from_u64(1)
		}

		fn record(&self, _span: &Id, _values: &Record<'_>) {}

		fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

		fn event(&self, event: &Event<'_>) {
			let mut msg = Msg {
				expected: &self.expected,
				matched: false,
			};
			event.record(&mut msg);
			if msg.matched {
				self.hits.fetch_add(1, AtomicOrdering::SeqCst);
			}
		}

		fn enter(&self, _span: &Id) {}

		fn exit(&self, _span: &Id) {}
	}

	let hits = Arc::new(AtomicUsize::new(0));
	let expected = expected_message.to_owned();
	tracing::subscriber::with_default(
		Count {
			hits: hits.clone(),
			expected,
		},
		f,
	);
	hits.load(AtomicOrdering::SeqCst)
}
