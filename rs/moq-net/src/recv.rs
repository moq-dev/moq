//! Receive-side ownership: cancelled handlers explicitly abort unfinished groups and frames.

use std::ops::{Deref, DerefMut};

use crate::{Error, frame, group};

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

/// A received frame that aborts before its borrowed group is released on cancellation.
pub(crate) struct Frame<'a>(Option<frame::Producer<'a>>);

impl<'a> Frame<'a> {
	/// Own a frame for the lifetime of its receive handler.
	pub fn new(producer: frame::Producer<'a>) -> Self {
		Self(Some(producer))
	}

	/// Finish receiving without running cancellation cleanup.
	pub fn finish(mut self) -> Result<(), Error> {
		self.0.take().expect("frame owned until consumed").finish()
	}

	/// Abort with the receive error instead of cancellation.
	pub fn abort(mut self, err: Error) -> Result<(), Error> {
		self.0.take().expect("frame owned until consumed").abort(err)
	}
}

impl<'a> Deref for Frame<'a> {
	type Target = frame::Producer<'a>;

	fn deref(&self) -> &Self::Target {
		self.0.as_ref().expect("frame owned until consumed")
	}
}

impl DerefMut for Frame<'_> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		self.0.as_mut().expect("frame owned until consumed")
	}
}

impl Drop for Frame<'_> {
	fn drop(&mut self) {
		if let Some(producer) = self.0.take() {
			let _ = producer.abort(Error::Cancel);
		}
	}
}
