use std::num::NonZero;

use crate::Error;

// Massive overkill, but it's fun.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Id(NonZero<u32>);

impl std::fmt::Display for Id {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}", self.0.get())
	}
}

// We purposely don't return 0 for slab IDs.
pub(crate) struct NonZeroSlab<T>(slab::Slab<T>);

impl<T> NonZeroSlab<T> {
	pub fn insert(&mut self, value: T) -> Id {
		let id = self.0.insert(value) + 1;
		let id = u32::try_from(id).expect("u32 overflow");
		Id(unsafe { NonZero::new_unchecked(id) })
	}

	pub fn get(&self, id: Id) -> Option<&T> {
		let id = (id.0.get() - 1) as usize;
		self.0.get(id)
	}

	pub fn get_mut(&mut self, id: Id) -> Option<&mut T> {
		let id = (id.0.get() - 1) as usize;
		self.0.get_mut(id)
	}

	pub fn remove(&mut self, id: Id) -> Option<T> {
		let id = (id.0.get() - 1) as usize;
		self.0.try_remove(id)
	}
}

impl TryFrom<i32> for Id {
	type Error = Error;

	fn try_from(value: i32) -> Result<Self, Self::Error> {
		Self::try_from(u32::try_from(value).map_err(|_| Error::InvalidId)?)
	}
}

impl TryFrom<u32> for Id {
	type Error = Error;

	fn try_from(value: u32) -> Result<Self, Self::Error> {
		NonZero::try_from(value).map(Id).map_err(|_| Error::InvalidId)
	}
}

impl From<Id> for u32 {
	fn from(value: Id) -> Self {
		value.0.get()
	}
}

impl TryFrom<Id> for i32 {
	type Error = Error;

	fn try_from(value: Id) -> Result<Self, Self::Error> {
		i32::try_from(u32::from(value)).map_err(|_| Error::InvalidId)
	}
}

impl<T> Default for NonZeroSlab<T> {
	fn default() -> Self {
		Self(slab::Slab::new())
	}
}
