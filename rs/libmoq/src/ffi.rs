use std::ffi::{c_char, c_void};

use url::Url;

use crate::{Error, Id};

/// Wrapper for C callback functions with user data.
///
/// Stores a function pointer and user data pointer to call C callbacks
/// from async Rust code.
pub struct OnStatus {
	user_data: *mut c_void,
	on_status: Option<extern "C" fn(user_data: *mut c_void, code: i32)>,
}

impl OnStatus {
	/// Create a new callback wrapper.
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
	/// Using &mut avoids the need for Sync.
	pub fn call<C: ReturnCode>(&mut self, ret: C) {
		if let Some(on_status) = &self.on_status {
			on_status(self.user_data, ret.code());
		}
	}
}

unsafe impl Send for OnStatus {}

/// Types that can be converted to C-compatible return codes.
pub trait ReturnCode {
	/// Convert to an i32 status code.
	fn code(&self) -> i32;
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
}

impl ReturnCode for Result<usize, Error> {
	fn code(&self) -> i32 {
		match self {
			Ok(code) => i32::try_from(*code).unwrap_or_else(|_| Error::InvalidCode.code()),
			Err(e) => e.code(),
		}
	}
}

impl ReturnCode for Result<Id, Error> {
	fn code(&self) -> i32 {
		match self {
			Ok(id) => i32::try_from(*id).unwrap_or_else(|_| Error::InvalidCode.code()),
			Err(e) => e.code(),
		}
	}
}

impl ReturnCode for Result<(), Error> {
	fn code(&self) -> i32 {
		match self {
			Ok(()) => 0,
			Err(e) => e.code(),
		}
	}
}

impl ReturnCode for usize {
	fn code(&self) -> i32 {
		i32::try_from(*self).unwrap_or_else(|_| Error::InvalidCode.code())
	}
}

impl ReturnCode for Id {
	fn code(&self) -> i32 {
		i32::try_from(*self).unwrap_or_else(|_| Error::InvalidCode.code())
	}
}

/// Parse an i32 handle into an Id.
pub fn parse_id(id: i32) -> Result<Id, Error> {
	Id::try_from(id)
}

/// Parse an optional i32 handle (0 = None) into an Option<Id>.
pub fn parse_id_optional(id: i32) -> Result<Option<Id>, Error> {
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
	let slice = parse_slice(cstr as *const u8, cstr_len)?;
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
