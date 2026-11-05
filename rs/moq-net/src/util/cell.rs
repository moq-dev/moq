/// A value whose thread-safety proof is checked once at construction.
///
/// Recursive channel graphs otherwise repeat the entire auto-trait proof at every
/// caller. The private field keeps construction behind these bounds.
///
/// The include stays at item level: in statement position it fails to parse,
/// which would pass these tests for the wrong reason.
///
/// ```compile_fail,E0277
/// include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/util/cell.rs"));
/// fn main() {
///     let _ = SendCell::new(std::rc::Rc::new(()));
/// }
/// ```
///
/// ```compile_fail,E0277
/// include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/util/cell.rs"));
/// fn main() {
///     let _ = SendCell::new(std::cell::Cell::new(0));
/// }
/// ```
#[derive(Clone, Debug)]
pub(crate) struct SendCell<T>(T);

impl<T: Send + Sync> SendCell<T> {
	pub fn new(value: T) -> Self {
		Self(value)
	}
}

// SAFETY: the only constructor requires T: Send + Sync. The private
// field cannot be replaced with another type; cloning preserves T's bounds.
unsafe impl<T> Send for SendCell<T> {}

// SAFETY: construction proves T: Sync before any shared reference can escape.
unsafe impl<T> Sync for SendCell<T> {}

impl<T> std::ops::Deref for SendCell<T> {
	type Target = T;

	fn deref(&self) -> &T {
		&self.0
	}
}
