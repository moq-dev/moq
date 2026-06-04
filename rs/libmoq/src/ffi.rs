use std::{
	cell::RefCell,
	ffi::{CString, c_char, c_void},
	sync::{LazyLock, Mutex},
};

use url::Url;

use crate::{Error, Id};

pub static RUNTIME: LazyLock<Mutex<tokio::runtime::Handle>> = LazyLock::new(|| {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let handle = runtime.handle().clone();

	std::thread::Builder::new()
		.name("libmoq".into())
		.spawn(move || {
			runtime.block_on(std::future::pending::<()>());
		})
		.expect("failed to spawn runtime thread");

	Mutex::new(handle)
});

/// Runs the provided function in the runtime context.
/// Additionally, we convert the return code to a C-compatible return value.
///
/// Uses a mutex to ensure Handle::enter() guards are dropped in LIFO order,
/// as required by tokio to avoid panics in multi-threaded FFI contexts.
pub fn enter<C: ReturnCode, F: FnOnce() -> C>(f: F) -> i32 {
	// NOTE: I think we need a mutex because Handle::enter() needs to be dropped in LIFO order.
	// If this starts to become a bottleneck, we might have to rethink our runtime model.
	let handle = RUNTIME.lock().unwrap();
	let _guard = handle.enter();

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

/// Wrapper for C callback functions with user data.
///
/// Stores a function pointer and user data pointer to call C callbacks
/// from async Rust code.
#[derive(Clone, Copy)]
pub struct OnStatus {
	user_data: *mut c_void,
	on_status: Option<extern "C" fn(user_data: *mut c_void, code: i32)>,
}

impl OnStatus {
	/// Create a new callback wrapper from a C function pointer.
	///
	/// # Safety
	/// - The caller must ensure user_data remains valid for the callback's lifetime.
	/// - The callback function pointer must be valid if provided.
	pub unsafe fn new(
		user_data: *mut c_void,
		on_status: Option<extern "C" fn(user_data: *mut c_void, code: i32)>,
	) -> Self {
		Self { user_data, on_status }
	}

	/// Invoke the callback with a result code.
	///
	/// We record the reason before invoking the callback (on the same thread)
	/// so a callback receiving a negative code can read `moq_error()` for it.
	pub fn call<C: ReturnCode>(&self, ret: C) {
		record_error(&ret);
		let code = ret.code();
		if let Some(on_status) = &self.on_status {
			on_status(self.user_data, code);
		}
	}
}

unsafe impl Send for OnStatus {}

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

thread_local! {
	/// Reason for the most recent error returned on this thread. FFI functions
	/// hand back only a numeric code, so we stash the human-readable message
	/// here for `moq_error` to retrieve.
	static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

/// Record the reason for an error return into this thread's `moq_error` slot.
///
/// Called at the FFI boundary (sync return and callback dispatch) right before
/// the numeric code is produced, so the conversion in `code()` stays pure.
fn record_error<C: ReturnCode>(ret: &C) {
	let Some(err) = ret.error() else { return };
	// CString::new fails only on an interior NUL, which our messages never
	// contain; skip storing rather than truncating if it ever happens.
	if let Ok(msg) = CString::new(err.to_string()) {
		LAST_ERROR.with(|cell| *cell.borrow_mut() = Some(msg));
	}
}

/// Pointer to this thread's last error message, or null if none was recorded.
///
/// The pointer is valid until the next libmoq call on the same thread.
pub fn last_error_ptr() -> *const c_char {
	LAST_ERROR.with(|cell| cell.borrow().as_ref().map_or(std::ptr::null(), |msg| msg.as_ptr()))
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
	let slice = unsafe { parse_slice(cstr as *const u8, cstr_len)? };
	let string = std::str::from_utf8(slice)?;
	Ok(string)
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
