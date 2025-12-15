use std::ffi::{c_char, c_void, CStr};

use url::Url;

use crate::{Error, Id};

pub struct OnStatus {
	user_data: *mut c_void,
	on_status: Option<extern "C" fn(user_data: *mut c_void, code: i32)>,
}

impl OnStatus {
	pub unsafe fn new(
		user_data: *mut c_void,
		on_status: Option<extern "C" fn(user_data: *mut c_void, code: i32)>,
	) -> Self {
		Self { user_data, on_status }
	}

	// &mut avoids the need for Sync
	pub fn call<C: ReturnCode>(&mut self, ret: C) {
		if let Some(on_status) = &self.on_status {
			on_status(self.user_data, ret.code());
		}
	}
}

unsafe impl Send for OnStatus {}

pub fn return_code<C: ReturnCode, F: FnOnce() -> C>(f: F) -> i32 {
	match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
		Ok(ret) => ret.code(),
		Err(_) => Error::Panic.code(),
	}
}

pub trait ReturnCode {
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

pub fn parse_id(id: i32) -> Result<Id, Error> {
	Id::try_from(id)
}

pub fn parse_id_optional(id: i32) -> Result<Option<Id>, Error> {
	match id {
		0 => Ok(None),
		id => Ok(Some(parse_id(id)?)),
	}
}

pub fn parse_url(url: *const c_char) -> Result<Url, Error> {
	if url.is_null() {
		return Err(Error::InvalidPointer);
	}

	let url = unsafe { CStr::from_ptr(url) };
	let url = url.to_str()?;
	Ok(Url::parse(url)?)
}

/// # Safety
///
/// The caller must ensure that cstr is valid for 'a.
pub unsafe fn parse_str<'a>(cstr: *const c_char) -> Result<&'a str, Error> {
	if cstr.is_null() {
		return Ok("");
	}

	let string = unsafe { CStr::from_ptr(cstr) };
	Ok(string.to_str()?)
}

/// # Safety
///
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
