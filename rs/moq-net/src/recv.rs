//! Receive-side ownership: a cancelled handler explicitly aborts its unfinished group.
//!
//! Frames need no guard here: `frame::Raw`'s own `Drop` already aborts the group when a
//! frame dies before its declared size is written, so a partial frame is never silent.

use std::ops::{Deref, DerefMut};

use crate::{Error, group};

/// A received group that aborts if its handler is cancelled before finishing or handing it off.
pub(crate) struct Group(Option<group::Producer>);

impl Group {
	/// Own a group for the lifetime of its receive handler.
	pub fn new(producer: group::Producer) -> Self {
		Self(Some(producer))
	}

	/// Hand ownership to the next receive stage without closing the group.
	pub fn into_inner(mut self) -> group::Producer {
		self.0.take().expect("group owned until consumed")
	}

	/// Finish receiving without running cancellation cleanup.
	pub fn finish(mut self) -> Result<(), Error> {
		self.deref_mut().finish()?;
		self.0.take();
		Ok(())
	}

	/// Abort with the receive error instead of cancellation.
	pub fn abort(self, err: Error) -> Result<(), Error> {
		self.into_inner().abort(err)
	}
}

impl Deref for Group {
	type Target = group::Producer;

	fn deref(&self) -> &Self::Target {
		self.0.as_ref().expect("group owned until consumed")
	}
}

impl DerefMut for Group {
	fn deref_mut(&mut self) -> &mut Self::Target {
		self.0.as_mut().expect("group owned until consumed")
	}
}

impl Drop for Group {
	fn drop(&mut self) {
		if let Some(producer) = self.0.take() {
			let _ = producer.abort(Error::Cancel);
		}
	}
}
