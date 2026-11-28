//! Metadata subscriptions: a track's group and frame progress without its payload.
//!
//! A metadata subscription is a subscription of its own beside any payload subscription to
//! the same track, with its mode fixed for its life. Where the hop carries the mode (moq-lite-07),
//! it travels upstream as a metadata subscription and no payload crosses that hop for it. Over
//! an older version, which cannot carry the mode, it rides the track's payload subscription and
//! drops the payload, so it never downloads the payload twice.
//!
//! A reader that wants the payload after all opens a payload subscription from the group it
//! reached and skips to its first missing frame ([`group::Consumer::skip_to`]): nothing changes
//! mode in place.

use std::ops::Deref;
use std::task::{Poll, ready};

use crate::{Result, broadcast, frame, group, track};

/// A lazy handle to a track's metadata, from [`broadcast::Consumer::meta`].
///
/// Like [`track::Consumer`], holding it requests nothing: only a [`Self::subscribe`] made from
/// it is demand.
#[derive(Clone)]
pub struct Consumer {
	inner: track::Consumer,
}

impl Consumer {
	pub(super) fn new(inner: track::Consumer) -> Self {
		Self { inner }
	}

	/// The track name this handle is bound to.
	pub fn name(&self) -> &str {
		self.inner.name()
	}

	/// The broadcast this track belongs to, as reached through the handle it came from.
	pub fn broadcast(&self) -> &broadcast::Info {
		self.inner.broadcast()
	}

	/// Open a live metadata subscription, delivering the groups `subscription` covers as
	/// [`track::Consumer::subscribe`] does, each frame as its header alone.
	pub fn subscribe(&self, subscription: impl Into<Option<track::Subscription>>) -> kio::Pending<Subscribing> {
		kio::Pending::new(Subscribing {
			inner: self.inner.subscribe(subscription).into_inner(),
		})
	}
}

/// The pollable state of a [`Consumer::subscribe`]; awaited via the [`kio::Pending`] wrapper.
pub struct Subscribing {
	inner: track::Subscribing,
}

impl Subscribing {
	/// Poll until the publisher confirms the subscription, yielding the [`Subscriber`].
	pub fn poll_ok(&self, waiter: &kio::Waiter) -> Poll<Result<Subscriber>> {
		let inner = ready!(self.inner.poll_ok(waiter))?;
		Poll::Ready(Ok(Subscriber { inner }))
	}

	/// Change the subscription preferences before (or after) it resolves; see
	/// [`track::Subscribing::update`].
	pub fn update(&mut self, subscription: track::Subscription) -> Result<()> {
		self.inner.update(subscription)
	}
}

impl kio::Task for Subscribing {
	type Output = Result<Subscriber>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

/// A live metadata subscription: [`track::Subscriber`] whose groups hold headers alone.
pub struct Subscriber {
	inner: track::Subscriber,
}

impl Subscriber {
	/// The track's [`track::Info`], resolved when the subscription was established.
	pub fn info(&self) -> &track::Info {
		self.inner.info()
	}

	/// The track's name, unique within its broadcast.
	pub fn name(&self) -> &str {
		self.inner.name()
	}

	/// The broadcast this track belongs to, as reached through the handle it came from.
	pub fn broadcast(&self) -> &broadcast::Info {
		self.inner.broadcast()
	}

	/// Create a handle for updating this subscriber's delivery preferences.
	pub fn control(&self) -> track::Control {
		self.inner.control()
	}

	/// Receive the next group, as [`track::Subscriber::poll_recv_group`] does.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_recv_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Group>>> {
		let group = ready!(self.inner.poll_recv_group(waiter))?;
		Poll::Ready(Ok(group.map(Group::new)))
	}

	/// The track's declared end, as [`track::Subscriber::poll_finished`] reports it.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_finished(&mut self, waiter: &kio::Waiter) -> Poll<Result<track::Position>> {
		self.inner.poll_finished(waiter)
	}

	/// Replace this subscriber's delivery preferences; see [`track::Subscriber::update`].
	pub fn update(&mut self, subscription: track::Subscription) -> Result<()> {
		self.inner.update(subscription)
	}
}

/// One group of a metadata subscription, read header by header.
///
/// Dereferences to its [`group::Info`] for the group's start.
pub struct Group {
	inner: group::Consumer,
}

impl Deref for Group {
	type Target = group::Info;

	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}

impl Group {
	fn new(inner: group::Consumer) -> Self {
		// Stored slices carry each frame's plain size beside them, so a header read neither
		// waits for nor inflates a compressed frame.
		Self {
			inner: inner.compressed(),
		}
	}

	/// The index of the next frame this reader returns.
	pub fn index(&self) -> u64 {
		self.inner.index()
	}

	/// The next frame's header: its timestamp and declared plain size, without its payload.
	///
	/// Returns `None` once the group ends. A real empty frame reads as size 0.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_next_header(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<frame::Info>>> {
		let Some(frame) = ready!(self.inner.poll_next_frame(waiter))? else {
			return Poll::Ready(Ok(None));
		};
		// Nothing waits for the payload, so a copy failing partway through it tears nothing.
		self.inner.forget_payload();
		Poll::Ready(Ok(Some(frame::Info {
			size: frame.inflated.unwrap_or(frame.size),
			timestamp: frame.timestamp,
		})))
	}
}

impl std::fmt::Debug for Group {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Group").field("start", &self.start).finish()
	}
}
