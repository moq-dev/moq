/// A named tuple of a producer and consumer for convenience.
///
/// The producer and consumer may each be cloned as many times as you want.
/// However when the number of references reaches zero, the other will receive a signal to close.
/// A new consumer may be created at any time by calling [T::consume].
#[derive(Clone)]
pub struct Produce<P, C> {
	pub producer: P,
	pub consumer: C,
}
