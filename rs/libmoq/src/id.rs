use std::collections::HashMap;
use std::num::NonZero;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::Error;

/// Opaque resource identifier returned to C code.
///
/// Non-zero u32 value that uniquely identifies resources like sessions,
/// origins, broadcasts, tracks, etc. Zero is reserved to indicate "none"
/// or optional parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Id(NonZero<u32>);

impl std::fmt::Display for Id {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}", self.0.get())
	}
}

/// Global monotonic counter so IDs are never reused across the process lifetime.
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

/// Map that assigns globally unique, never-reused IDs.
///
/// Unlike a slab, freed IDs are not recycled. This avoids races where
/// parallel consumers of the same global state accidentally alias each
/// other's resources.
pub(crate) struct NonZeroSlab<T> {
	map: HashMap<Id, T>,
}

impl<T> NonZeroSlab<T> {
	pub fn insert(&mut self, value: T) -> Result<Id, Error> {
		let raw = NEXT_ID.fetch_add(1, Ordering::Relaxed);
		let id = Id(NonZero::new(raw).ok_or(Error::IdOverflow)?);
		self.map.insert(id, value);
		Ok(id)
	}

	pub fn get(&self, id: Id) -> Option<&T> {
		self.map.get(&id)
	}

	pub fn get_mut(&mut self, id: Id) -> Option<&mut T> {
		self.map.get_mut(&id)
	}

	pub fn remove(&mut self, id: Id) -> Option<T> {
		self.map.remove(&id)
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
		Self { map: HashMap::new() }
	}
}
