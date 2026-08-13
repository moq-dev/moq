//! Small helpers shared by the binding modules.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wasm_bindgen::prelude::*;

/// Map any displayable error into a JS exception.
pub fn js_err(e: impl std::fmt::Display) -> JsValue {
	JsError::new(&e.to_string()).into()
}

/// Holds a value that `&mut self` async methods need exclusive access to.
///
/// wasm-bindgen async methods take `&self` and must produce `'static` futures, so a
/// `RefCell` borrow can't be held across the await. Instead the value is moved out for
/// the duration of the call and moved back after, which also makes a re-entrant call an
/// error rather than an aliasing bug: one in-flight call per handle.
pub struct Exclusive<T> {
	inner: Rc<RefCell<Option<T>>>,

	// Both "busy" and "consumed" read as an empty cell, so the reason is tracked here.
	// Otherwise a call after `abort()` reports "already in progress", pointing a JS
	// developer at concurrency when the real mistake was use-after-consume.
	consumed: Rc<Cell<bool>>,
}

impl<T> Exclusive<T> {
	pub fn new(value: T) -> Self {
		Self {
			inner: Rc::new(RefCell::new(Some(value))),
			consumed: Rc::new(Cell::new(false)),
		}
	}

	fn unavailable(&self, what: &str) -> JsValue {
		if self.consumed.get() {
			js_err(format!("{what}: handle was already consumed"))
		} else {
			js_err(format!("{what} already in progress"))
		}
	}

	/// Run `f` with the value moved out, restoring it when the future completes.
	///
	/// `what` names the operation so a re-entrant call reports which one is already
	/// running.
	pub async fn with<F, Fut, R>(&self, what: &str, f: F) -> Result<R, JsValue>
	where
		F: FnOnce(T) -> Fut,
		Fut: Future<Output = (T, R)>,
	{
		let cell = self.inner.clone();
		let value = cell.borrow_mut().take().ok_or_else(|| self.unavailable(what))?;

		let (value, result) = f(value).await;
		*cell.borrow_mut() = Some(value);
		Ok(result)
	}

	/// Run `f` against the value without awaiting.
	pub fn peek<F, R>(&self, what: &str, f: F) -> Result<R, JsValue>
	where
		F: FnOnce(&mut T) -> R,
	{
		let mut guard = self.inner.borrow_mut();
		let value = guard.as_mut().ok_or_else(|| self.unavailable(what))?;
		Ok(f(value))
	}

	/// Move the value out for good, for a terminal operation that consumes it.
	///
	/// Every later call reports the handle as unusable, which is the closest a
	/// wasm-bindgen handle gets to moq-net's `fn abort(self)`.
	pub fn take(&self, what: &str) -> Result<T, JsValue> {
		let value = self.inner.borrow_mut().take().ok_or_else(|| self.unavailable(what))?;
		self.consumed.set(true);
		Ok(value)
	}
}

impl<T> Clone for Exclusive<T> {
	fn clone(&self) -> Self {
		Self {
			inner: self.inner.clone(),
			consumed: self.consumed.clone(),
		}
	}
}
