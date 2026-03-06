use std::sync::{LazyLock, Mutex};

use crate::{Error, Id};

pub static RUNTIME: LazyLock<Mutex<tokio::runtime::Handle>> = LazyLock::new(|| {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let handle = runtime.handle().clone();

	std::thread::Builder::new()
		.name("moq-ffi".into())
		.spawn(move || {
			runtime.block_on(std::future::pending::<()>());
		})
		.expect("failed to spawn runtime thread");

	Mutex::new(handle)
});

/// Wrapper for Rust callback closures.
pub struct OnStatus {
	rust_fn: Box<dyn FnMut(i32) + Send>,
}

impl OnStatus {
	/// Create a callback wrapper from a Rust closure.
	pub fn from_fn(callback: impl FnMut(i32) + Send + 'static) -> Self {
		Self {
			rust_fn: Box::new(callback),
		}
	}

	/// Invoke the callback with a result code.
	pub fn call<C: ReturnCode>(&mut self, ret: C) {
		(self.rust_fn)(ret.code());
	}
}

/// Types that can be converted to i32 status codes.
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
