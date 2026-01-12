use std::fmt;

/// A named tuple of a producer and consumer for convenience.
///
/// The producer and consumer may each be cloned as many times as you want.
/// However when the number of references reaches zero, the other will receive a signal to close.
/// A new consumer may be created at any time by calling the producer's `consume()` method.
#[derive(Clone)]
pub struct Produce<P, C> {
	pub producer: P,
	pub consumer: C,
}

impl<P, C> Produce<P, C> {
	pub fn new(producer: P, consumer: C) -> Self {
		Self { producer, consumer }
	}
}

impl<P: fmt::Debug, C: fmt::Debug> fmt::Debug for Produce<P, C> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Produce")
			.field("producer", &self.producer)
			.field("consumer", &self.consumer)
			.finish()
	}
}
