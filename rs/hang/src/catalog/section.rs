use std::marker::PhantomData;

/// A section definition that pairs a JSON key name with a typed schema.
///
/// Used to register interest in specific catalog sections for reading or writing.
/// Audio and video sections are predefined but not registered by default.
pub struct Section<T> {
	pub name: &'static str,
	_phantom: PhantomData<T>,
}

impl<T> Section<T> {
	pub const fn new(name: &'static str) -> Self {
		Self {
			name,
			_phantom: PhantomData,
		}
	}
}
