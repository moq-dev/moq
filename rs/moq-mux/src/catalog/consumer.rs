//! Unified catalog consumer.
//!
//! Subscribes to whichever catalog track ([`hang`] or [`msf`]) the broadcast
//! advertises and yields [`Catalog<E>`](super::hang::Catalog) snapshots so callers
//! and exporters only deal with one shape.

use std::task::{Poll, ready};

use super::hang::{Catalog, CatalogExt};
use super::{CatalogFormat, Stream};

/// A catalog stream sourced from a [`moq_net::broadcast::Consumer`].
///
/// Both encodings emit [`Catalog<E>`](super::hang::Catalog), extension included:
/// MSF carries the same root sections as the hang catalog. Wrap with
/// [`Select`](super::Select) to narrow the rendition set before handing the
/// stream to an exporter.
///
/// Reaching EOF releases the subscription and decoder while retaining its format.
///
/// Drive it through the [`Stream`] trait; its encoding and ended state stay private.
///
/// ```compile_fail
/// use moq_mux::catalog::{CatalogFormat, Consumer};
/// let consumer = Consumer::<()>::Closed(CatalogFormat::Hang);
/// ```
pub struct Consumer<E: CatalogExt = ()> {
	inner: State<E>,
}

#[allow(
	clippy::large_enum_variant,
	reason = "one per catalog subscription, and the live variant is the working state; it is only large when a workspace build unifies extra features"
)]
enum State<E: CatalogExt> {
	Hang(super::hang::Consumer<E>),
	Msf(super::msf::Consumer<E>),
	Closed(CatalogFormat),
}

impl<E: CatalogExt> Consumer<E> {
	pub(crate) fn format(&self) -> CatalogFormat {
		match &self.inner {
			State::Hang(_) => CatalogFormat::Hang,
			State::Msf(_) => CatalogFormat::Msf,
			State::Closed(format) => *format,
		}
	}

	/// Subscribe to the catalog track advertised by `format`.
	#[cfg(feature = "async")]
	pub async fn new(broadcast: &moq_net::broadcast::Consumer, format: CatalogFormat) -> Result<Self, crate::Error> {
		let subscribing = Subscribing::new(broadcast, format);
		kio::wait(|waiter| subscribing.poll_ok(waiter)).await
	}
}

/// A catalog subscription awaiting its track: [`Consumer::new`] for a caller that polls.
pub(crate) struct Subscribing<E: CatalogExt> {
	format: CatalogFormat,
	track: Result<moq_net::track::Subscribing, crate::Error>,
	_ext: std::marker::PhantomData<E>,
}

impl<E: CatalogExt> Subscribing<E> {
	/// Subscribe to the catalog track advertised by `format`.
	pub(crate) fn new(broadcast: &moq_net::broadcast::Consumer, format: CatalogFormat) -> Self {
		let track = match format {
			CatalogFormat::Hang => broadcast
				.track(hang::Catalog::DEFAULT_NAME)
				.map(|track| track.subscribe(hang::Catalog::default_subscription())),
			CatalogFormat::Msf => broadcast
				.track(moq_msf::DEFAULT_NAME)
				.map(|track| track.subscribe(None)),
		};
		Self {
			format,
			track: track.map(kio::Pending::into_inner).map_err(Into::into),
			_ext: std::marker::PhantomData,
		}
	}

	/// The catalog consumer, once the track subscription is confirmed.
	pub(crate) fn poll_ok(&self, waiter: &kio::Waiter) -> Poll<Result<Consumer<E>, crate::Error>> {
		let track = match &self.track {
			Ok(track) => ready!(track.poll_ok(waiter))?,
			Err(err) => return Poll::Ready(Err(err.clone())),
		};
		let inner = match self.format {
			CatalogFormat::Hang => State::Hang(super::hang::Consumer::new(track)),
			CatalogFormat::Msf => State::Msf(super::msf::Consumer::new(track)),
		};
		Poll::Ready(Ok(Consumer { inner }))
	}
}

impl<E: CatalogExt> From<super::hang::Consumer<E>> for Consumer<E> {
	fn from(consumer: super::hang::Consumer<E>) -> Self {
		Self {
			inner: State::Hang(consumer),
		}
	}
}

impl<E: CatalogExt> Stream for Consumer<E> {
	type Ext = E;

	fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Catalog<E>>>> {
		let next = match &mut self.inner {
			State::Hang(consumer) => consumer.poll_next(waiter),
			State::Msf(consumer) => consumer.poll_next(waiter),
			State::Closed(_) => return Poll::Ready(Ok(None)),
		};
		if matches!(&next, Poll::Ready(Ok(None))) {
			// A follower still needs the format, but no ended subscription or decoder.
			self.inner = State::Closed(self.format());
		}
		next
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test(start_paused = true)]
	async fn eof_releases_each_catalog_subscription() {
		for format in [CatalogFormat::Hang, CatalogFormat::Msf] {
			let name = match format {
				CatalogFormat::Hang => hang::Catalog::DEFAULT_NAME,
				CatalogFormat::Msf => moq_msf::DEFAULT_NAME,
			};
			let broadcast = moq_net::broadcast::Info::new().produce();
			let track = broadcast
				.create_track(name, hang::Catalog::default_track_info())
				.unwrap();
			let mut consumer = Consumer::<()>::new(&broadcast.consume(), format).await.unwrap();
			assert!(track.subscription().is_some(), "{format:?}: subscribed");
			track.finish().unwrap();
			assert!(consumer.next().await.unwrap().is_none());
			assert!(
				track.subscription().is_none(),
				"{format:?}: EOF must release its subscriber"
			);
			assert_eq!(consumer.format(), format);
			assert!(consumer.next().await.unwrap().is_none());
		}
	}
}
