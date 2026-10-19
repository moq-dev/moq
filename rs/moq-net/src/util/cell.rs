/// A value whose native thread-safety proof is checked once at construction.
///
/// Recursive channel graphs otherwise repeat the entire auto-trait proof at every
/// caller. The private field keeps construction behind these bounds; wasm keeps
/// its single-threaded, non-Send transport values.
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

#[cfg(not(target_family = "wasm"))]
impl<T: Send + Sync> SendCell<T> {
	pub fn new(value: T) -> Self {
		Self(value)
	}
}

#[cfg(target_family = "wasm")]
impl<T> SendCell<T> {
	pub fn new(value: T) -> Self {
		Self(value)
	}
}

// SAFETY: the only native constructor requires T: Send + Sync. The private
// field cannot be replaced with another type; cloning preserves T's bounds.
#[cfg(not(target_family = "wasm"))]
unsafe impl<T> Send for SendCell<T> {}

// SAFETY: construction proves T: Sync before any shared reference can escape.
#[cfg(not(target_family = "wasm"))]
unsafe impl<T> Sync for SendCell<T> {}

impl<T> std::ops::Deref for SendCell<T> {
	type Target = T;

	fn deref(&self) -> &T {
		&self.0
	}
}
