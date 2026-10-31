use std::{
	cell::RefCell,
	ffi::{CString, c_char, c_void},
	sync::LazyLock,
};

use url::Url;

use crate::{Error, Id, moq_protocol_error};

/// A callback receiving a positive handle/value, zero on clean completion, or a negative error.
///
/// Runs on moq-c's runtime thread. The callback and `user_data` must be safe to use on that thread.
#[expect(non_camel_case_types, reason = "C ABI type names match moq.h")]
pub type moq_status_callback = Option<extern "C" fn(user_data: *mut c_void, code: i32)>;

/// A callback receiving each session lifecycle change; the `MOQ_CONNECTION_PHASE_CLOSED` one is terminal.
///
/// Runs on moq-c's runtime thread. The callback and `user_data` must be safe to use on that thread.
#[expect(non_camel_case_types, reason = "C ABI type names match moq.h")]
pub type moq_state_callback = Option<extern "C" fn(user_data: *mut c_void, state: *const crate::moq_connection_state)>;

pub static RUNTIME: LazyLock<tokio::runtime::Handle> = LazyLock::new(|| {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let handle = runtime.handle().clone();

	std::thread::Builder::new()
		.name("moq-c".into())
		.spawn(move || {
			runtime.block_on(std::future::pending::<()>());
		})
		.expect("failed to spawn runtime thread");

	handle
});

/// Runs the provided function in the runtime context.
/// Additionally, we convert the return code to a C-compatible return value.
///
/// Callers run concurrently: entering a handle only sets a thread-local, so
/// nothing here is shared between threads. Tokio's requirement that the guards
/// be dropped in LIFO order is per-thread too, and this one lives and dies in
/// this frame, so a nested call nests rather than crosses.
pub fn enter<C: ReturnCode, F: FnOnce() -> C>(f: F) -> i32 {
	let _guard = RUNTIME.enter();

	match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
		Ok(ret) => {
			record_error(&ret);
			ret.code()
		}
		Err(_) => {
			record_error(&Error::Panic);
			Error::Panic.code()
		}
	}
}

/// Run a background operation before its terminal callback and resource cleanup.
pub async fn run<T>(future: impl std::future::Future<Output = Result<T, Error>>) -> Result<T, Error> {
	moq_tokio::panic::isolate(future).await.unwrap_or(Err(Error::Panic))
}

/// Completion of an owned background task, shared by the object's host operations.
#[derive(Clone, Default)]
pub struct Background {
	result: Option<kio::Consumer<Option<Result<(), Error>>>>,
}

impl Background {
	/// Spawn an owned operation and retain its completion.
	pub fn new(future: impl std::future::Future<Output = Result<(), Error>> + Send + 'static) -> Self {
		let result = kio::Producer::new(None);
		let receiver = result.consume();
		RUNTIME.spawn(async move {
			let completed = run(future).await;
			*result
				.write()
				.unwrap_or_else(|_| unreachable!("only the background task closes its completion")) = Some(completed);
		});
		Self { result: Some(receiver) }
	}

	/// Own a model driver until its completion has been reported.
	pub fn driver(mut driver: impl moq_net::time::Driver + Send + 'static) -> Self {
		let result = kio::Producer::new(None);
		let receiver = result.consume();
		RUNTIME.spawn(async move {
			// Keep the driver outside the unwind, so its channels close after panic publication.
			let completed = run(async {
				match moq_net::time::run(Borrowed(&mut driver)).await {
					moq_net::Error::Closed => Ok(()),
					error => Err(error.into()),
				}
			})
			.await;
			if matches!(&completed, Err(Error::Panic)) {
				*result
					.write()
					.unwrap_or_else(|_| unreachable!("only the task closes its completion")) = Some(completed.clone());
			}
			let completed = run(async move {
				drop(driver);
				completed
			})
			.await;
			*result
				.write()
				.unwrap_or_else(|_| unreachable!("only the background task closes its completion")) = Some(completed);
		});
		Self { result: Some(receiver) }
	}

	/// Return a completed background failure, if any.
	pub fn check(&self) -> Result<(), Error> {
		self.result
			.as_ref()
			.map(|result| result.read().clone().unwrap_or(Ok(())))
			.unwrap_or(Ok(()))
	}

	fn poll_finished(&self, waiter: &kio::Waiter) -> std::task::Poll<Result<(), Error>> {
		let Some(result) = &self.result else {
			return std::task::Poll::Ready(Ok(()));
		};
		match std::task::ready!(result.poll(waiter, |state| match &**state {
			Some(result) => std::task::Poll::Ready(result.clone()),
			None => std::task::Poll::Pending,
		})) {
			Ok(result) => std::task::Poll::Ready(result),
			Err(_) => std::task::Poll::Ready(Err(Error::Panic)),
		}
	}

	/// Wait until the background operation finishes and return its result.
	pub async fn finished(&self) -> Result<(), Error> {
		kio::wait(|waiter| self.poll_finished(waiter)).await
	}

	/// Fail an operation when its owned background task fails.
	pub async fn watch<T>(&self, future: impl std::future::Future<Output = Result<T, Error>>) -> Result<T, Error> {
		self.check()?;
		if self.result.is_none() {
			return future.await;
		}
		tokio::pin!(future);
		let result = tokio::select! {
			biased;
			result = self.finished() => { result?; future.await }
			result = &mut future => result,
		};
		self.check()?;
		result
	}
}

#[cfg(test)]
pub(crate) fn panicking_origin() -> (moq_net::origin::Producer, Background, kio::Producer<bool>) {
	struct Driver {
		inner: moq_net::origin::Driver,
		failed: kio::Consumer<bool>,
	}
	impl moq_net::time::Driver for Driver {
		fn poll(
			&mut self,
			now: moq_net::time::Instant,
			waiter: &kio::Waiter,
		) -> Result<Option<moq_net::time::Instant>, moq_net::Error> {
			let _ = self.failed.poll(waiter, |state| {
				assert!(!**state, "injected origin driver panic");
				std::task::Poll::<()>::Pending
			});
			self.inner.poll(now, waiter)
		}
	}
	let (origin, inner) = moq_net::origin::Producer::new(Default::default());
	let failed = kio::Producer::new(false);
	let driver = Background::driver(Driver {
		inner,
		failed: failed.consume(),
	});
	(origin, driver, failed)
}

struct Borrowed<'a, D>(&'a mut D);

impl<D: moq_net::time::Driver> moq_net::time::Driver for Borrowed<'_, D> {
	fn poll(
		&mut self,
		now: moq_net::time::Instant,
		waiter: &kio::Waiter,
	) -> Result<Option<moq_net::time::Instant>, moq_net::Error> {
		self.0.poll(now, waiter)
	}
}

/// Wrapper for C callback functions with user data.
///
/// Stores a function pointer and user data pointer to call C callbacks
/// from async Rust code.
#[derive(Clone, Copy)]
pub struct OnStatus {
	user_data: *mut c_void,
	on_status: extern "C" fn(user_data: *mut c_void, code: i32),
}

impl OnStatus {
	/// Create a new callback wrapper from a C function pointer.
	///
	/// # Safety
	/// - The caller must ensure user_data remains valid for the callback's lifetime.
	/// - The callback function pointer must be valid if provided.
	/// - The callback and `user_data` must be safe to use on moq-c's runtime thread.
	pub unsafe fn new(user_data: *mut c_void, on_status: moq_status_callback) -> Result<Self, Error> {
		Ok(Self {
			user_data,
			on_status: on_status.ok_or(Error::InvalidPointer)?,
		})
	}

	/// Invoke the callback with a result code.
	///
	/// We record the reason before invoking the callback (on the same thread)
	/// so a callback receiving a negative code can read `moq_error()` for it.
	pub fn call<C: ReturnCode>(&self, ret: C) {
		record_error(&ret);
		let code = ret.code();
		(self.on_status)(self.user_data, code);
	}
}

// SAFETY: The constructor requires the callback and user_data to remain valid and
// safe to use on moq-c's runtime thread for the callback's lifetime.
unsafe impl Send for OnStatus {}

/// A [moq_state_callback] and its user data.
#[derive(Clone, Copy)]
pub struct OnState {
	user_data: *mut c_void,
	on_state: extern "C" fn(user_data: *mut c_void, state: *const crate::moq_connection_state),
}

impl OnState {
	/// Wrap a C state callback.
	///
	/// # Safety
	/// - The caller must keep `user_data` valid until the terminal callback.
	/// - The callback and `user_data` must be safe to use on moq-c's runtime thread.
	pub unsafe fn new(user_data: *mut c_void, on_state: moq_state_callback) -> Result<Self, Error> {
		Ok(Self {
			user_data,
			on_state: on_state.ok_or(Error::InvalidPointer)?,
		})
	}

	/// Deliver one snapshot, recording its failure first so the callback can read `moq_error()`.
	pub fn call(&self, state: &crate::session::Lifecycle) {
		if let Some(failure) = &state.failure {
			record(failure);
		}
		let state = state.to_c();
		(self.on_state)(self.user_data, &state);
	}
}

// SAFETY: The constructor requires the callback and user_data to remain valid and
// safe to use on moq-c's runtime thread until the terminal callback.
unsafe impl Send for OnState {}

/// Types that can be converted to C-compatible return codes.
pub trait ReturnCode {
	/// Convert to an i32 status code.
	fn code(&self) -> i32;

	/// The error this carries, if any, so the boundary can record its reason
	/// for `moq_error`. Defaults to none for non-fallible return types.
	fn error(&self) -> Option<&Error> {
		None
	}
}

impl ReturnCode for () {
	fn code(&self) -> i32 {
		0
	}
}

impl ReturnCode for i32 {
	fn code(&self) -> i32 {
		*self
	}
}

impl ReturnCode for Result<i32, Error> {
	fn code(&self) -> i32 {
		match self {
			Ok(code) if *code < 0 => Error::InvalidCode.code(),
			Ok(code) => *code,
			Err(e) => e.code(),
		}
	}

	fn error(&self) -> Option<&Error> {
		self.as_ref().err()
	}
}

impl ReturnCode for Result<usize, Error> {
	fn code(&self) -> i32 {
		match self {
			Ok(code) => i32::try_from(*code).unwrap_or_else(|_| Error::InvalidCode.code()),
			Err(e) => e.code(),
		}
	}

	fn error(&self) -> Option<&Error> {
		self.as_ref().err()
	}
}

impl ReturnCode for Result<Id, Error> {
	fn code(&self) -> i32 {
		match self {
			Ok(id) => i32::from(*id),
			Err(e) => e.code(),
		}
	}

	fn error(&self) -> Option<&Error> {
		self.as_ref().err()
	}
}

impl ReturnCode for Result<(), Error> {
	fn code(&self) -> i32 {
		match self {
			Ok(()) => 0,
			Err(e) => e.code(),
		}
	}

	fn error(&self) -> Option<&Error> {
		self.as_ref().err()
	}
}

impl ReturnCode for usize {
	fn code(&self) -> i32 {
		i32::try_from(*self).unwrap_or_else(|_| Error::InvalidCode.code())
	}
}

impl ReturnCode for Id {
	fn code(&self) -> i32 {
		i32::from(*self)
	}
}

struct LastError {
	message: CString,
	protocol: Option<moq_protocol_error>,
}

thread_local! {
	/// Reason for the most recent error returned on this thread. FFI functions
	/// hand back only a numeric code, so we stash the human-readable message
	/// (and protocol details, when the failure is a session or stream code)
	/// here for `moq_error` / `moq_error_protocol` to retrieve.
	static LAST_ERROR: RefCell<Option<LastError>> = const { RefCell::new(None) };
}

/// Record the reason for an error return into this thread's `moq_error` slot.
///
/// Called at the FFI boundary (sync return and callback dispatch) right before
/// the numeric code is produced, so the conversion in `code()` stays pure.
fn record_error<C: ReturnCode>(ret: &C) {
	if let Some(err) = ret.error() {
		record(err);
	}
}

fn record(err: &Error) {
	// CString::new fails only on an interior NUL, which our messages never
	// contain; skip storing rather than truncating if it ever happens.
	if let Ok(msg) = CString::new(err.to_string()) {
		LAST_ERROR.with(|cell| {
			*cell.borrow_mut() = Some(LastError {
				message: msg,
				protocol: err.protocol(),
			});
		});
	}
}

/// Pointer to this thread's last error message, or null if none was recorded.
///
/// The pointer is valid until the next moq-c call on the same thread.
pub fn last_error_ptr() -> *const c_char {
	LAST_ERROR.with(|cell| {
		cell.borrow()
			.as_ref()
			.map_or(std::ptr::null(), |err| err.message.as_ptr())
	})
}

/// Copy this thread's last protocol error into `out`.
///
/// Returns true when the last error was a protocol failure and `out` was written.
pub fn last_protocol(out: &mut moq_protocol_error) -> bool {
	LAST_ERROR.with(|cell| match cell.borrow().as_ref().and_then(|err| err.protocol) {
		Some(protocol) => {
			*out = protocol;
			true
		}
		None => false,
	})
}

/// Parse an i32 handle into an Id.
pub fn parse_id(id: u32) -> Result<Id, Error> {
	Id::try_from(id)
}

/// Parse an optional i32 handle (0 = None) into an Option<Id>.
pub fn parse_id_optional(id: u32) -> Result<Option<Id>, Error> {
	match id {
		0 => Ok(None),
		id => Ok(Some(parse_id(id)?)),
	}
}

/// Parse a C string pointer into a Url.
pub fn parse_url(url: *const c_char, url_len: usize) -> Result<Url, Error> {
	let url = unsafe { parse_str(url, url_len)? };
	Ok(Url::parse(url)?)
}

/// Parse a C string pointer into a &str.
///
/// Returns an empty string if the pointer is null.
///
/// # Safety
/// The caller must ensure that cstr is valid for 'a.
pub unsafe fn parse_str<'a>(cstr: *const c_char, cstr_len: usize) -> Result<&'a str, Error> {
	let slice = unsafe { parse_slice(cstr.cast::<u8>(), cstr_len)? };
	let string = std::str::from_utf8(slice)?;
	Ok(string)
}

/// Parse an optional C string, where a NULL or empty value means "unset".
///
/// Config setters use this so one function both sets and clears a knob, rather than
/// needing a paired `moq_client_clear_*` for every optional field.
///
/// # Safety
/// The caller must ensure that cstr is valid for 'a.
pub unsafe fn parse_str_optional<'a>(cstr: *const c_char, cstr_len: usize) -> Result<Option<&'a str>, Error> {
	if cstr.is_null() {
		return Ok(None);
	}

	let string = unsafe { parse_str(cstr, cstr_len)? };
	Ok((!string.is_empty()).then_some(string))
}

/// Parse a C array of [`crate::moq_string`] into owned strings.
///
/// A NULL array is only valid when `count` is zero, which yields an empty list.
///
/// # Safety
/// The caller must ensure that items is valid for count elements, and that each
/// element points to its own length in bytes.
pub unsafe fn parse_strings(items: *const crate::moq_string, count: usize) -> Result<Vec<String>, Error> {
	if items.is_null() {
		if count == 0 {
			return Ok(Vec::new());
		}

		return Err(Error::InvalidPointer);
	}

	let items = unsafe { std::slice::from_raw_parts(items, count) };
	items
		.iter()
		.map(|item| Ok(unsafe { parse_str(item.data, item.len)? }.to_string()))
		.collect()
}

/// Parse a raw pointer and size into a byte slice.
///
/// Returns an empty slice if both pointer and size are zero.
///
/// # Safety
/// The caller must ensure that data is valid for 'a.
pub unsafe fn parse_slice<'a>(data: *const u8, size: usize) -> Result<&'a [u8], Error> {
	if data.is_null() {
		if size == 0 {
			return Ok(&[]);
		}

		return Err(Error::InvalidPointer);
	}

	let data = unsafe { std::slice::from_raw_parts(data, size) };
	Ok(data)
}

#[cfg(test)]
mod tests {

	#[tokio::test]
	async fn a_driver_teardown_panic_reaches_its_owner() {
		struct Driver;
		impl moq_net::time::Driver for Driver {
			fn poll(
				&mut self,
				_: moq_net::time::Instant,
				_: &kio::Waiter,
			) -> Result<Option<moq_net::time::Instant>, moq_net::Error> {
				Err(moq_net::Error::Closed)
			}
		}
		impl Drop for Driver {
			fn drop(&mut self) {
				panic!("injected driver teardown panic");
			}
		}
		let driver = crate::ffi::Background::driver(Driver);
		assert!(matches!(driver.finished().await, Err(Error::Panic)));
	}
	use super::*;

	#[tokio::test]
	async fn an_owned_driver_panic_wins_over_its_dropped_channels() {
		let driver = Background::new(async { panic!("injected origin driver panic") });
		assert!(matches!(
			driver.watch(std::future::pending::<Result<(), Error>>()).await,
			Err(Error::Panic)
		));
		assert!(matches!(
			driver
				.watch(async { Err::<(), _>(moq_net::Error::Cancel.into()) })
				.await,
			Err(Error::Panic)
		));
	}

	struct PanickingDriver {
		dropped: Option<tokio::sync::oneshot::Sender<()>>,
		observed: std::sync::mpsc::Receiver<()>,
	}

	impl moq_net::time::Driver for PanickingDriver {
		fn poll(
			&mut self,
			_: moq_net::time::Instant,
			_: &kio::Waiter,
		) -> Result<Option<moq_net::time::Instant>, moq_net::Error> {
			panic!("injected driver poll panic");
		}
	}

	impl Drop for PanickingDriver {
		fn drop(&mut self) {
			let _ = self.dropped.take().unwrap().send(());
			// Hold teardown until the foreign host has classified the dropped channel.
			let _ = self.observed.recv();
		}
	}

	#[tokio::test]
	async fn a_driver_panic_is_recorded_before_teardown_wakes_the_host() {
		let (dropped, closed) = tokio::sync::oneshot::channel();
		let (observed, ack) = std::sync::mpsc::channel();
		let driver = Background::driver(PanickingDriver {
			dropped: Some(dropped),
			observed: ack,
		});
		let result = driver
			.watch(async {
				let _ = closed.await;
				Err::<(), _>(moq_net::Error::Cancel.into())
			})
			.await;
		let _ = observed.send(());
		assert!(matches!(driver.finished().await, Err(Error::Panic)));
		assert!(
			matches!(result, Err(Error::Panic)),
			"host saw teardown before panic: {result:?}"
		);
	}
}
