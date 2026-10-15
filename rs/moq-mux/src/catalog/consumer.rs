//! Unified catalog consumer.
//!
//! Subscribes to whichever catalog track ([`hang`] or [`msf`]) the broadcast
//! advertises and yields [`Catalog<E>`](super::hang::Catalog) snapshots so callers
//! and exporters only deal with one shape.

use std::task::Poll;

use super::hang::{Catalog, CatalogExt};
use super::{CatalogFormat, Stream};

/// A catalog stream sourced from a [`moq_net::broadcast::Consumer`].
///
/// Both variants emit [`Catalog<E>`](super::hang::Catalog), extension included:
/// MSF carries the same root sections as the hang catalog. Wrap with
/// [`Select`](super::Select) to narrow the rendition set before handing the
/// stream to an exporter.
///
/// Reaching EOF releases the subscription and decoder while retaining its format.
///
/// The variants are an implementation detail: drive it through the [`Stream`]
/// trait rather than matching on them. New catalog encodings may be added.
#[non_exhaustive]
pub enum Consumer<E: CatalogExt = ()> {
	#[doc(hidden)]
	Hang(super::hang::Consumer<E>),
	#[doc(hidden)]
	Msf(super::msf::Consumer<E>),
	#[doc(hidden)]
	Closed(CatalogFormat),
}

impl<E: CatalogExt> Consumer<E> {
	pub(crate) fn format(&self) -> CatalogFormat {
		match self {
			Self::Hang(consumer) => consumer.format(),
			Self::Msf(_) => CatalogFormat::Msf,
			Self::Closed(format) => *format,
		}
	}

	/// Subscribe to the catalog track advertised by `format`.
	pub async fn new(broadcast: &moq_net::broadcast::Consumer, format: CatalogFormat) -> Result<Self, crate::Error> {
		Ok(match format {
			CatalogFormat::Hang => {
				let track = broadcast
					.track(hang::Catalog::DEFAULT_NAME)?
					.subscribe(hang::Catalog::default_subscription())
					.await?;
				Self::Hang(super::hang::Consumer::new(track))
			}
			CatalogFormat::HangZ => {
				let track = broadcast
					.track(hang::Catalog::COMPRESSED_NAME)?
					.subscribe(hang::Catalog::default_subscription())
					.await?;
				Self::Hang(super::hang::Consumer::compressed(track))
			}
			CatalogFormat::Msf => {
				let track = broadcast.track(moq_msf::DEFAULT_NAME)?.subscribe(None).await?;
				Self::Msf(super::msf::Consumer::new(track))
			}
		})
	}
}

impl<E: CatalogExt> Stream for Consumer<E> {
	type Ext = E;

	fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Catalog<E>>>> {
		let next = match self {
			Self::Hang(consumer) => consumer.poll_next(waiter),
			Self::Msf(consumer) => consumer.poll_next(waiter),
			Self::Closed(_) => return Poll::Ready(Ok(None)),
		};
		if matches!(&next, Poll::Ready(Ok(None))) {
			// A follower still needs the format, but no ended subscription or decoder.
			*self = Self::Closed(self.format());
		}
		next
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test(start_paused = true)]
	async fn eof_releases_each_catalog_subscription() {
		for format in [CatalogFormat::Hang, CatalogFormat::HangZ, CatalogFormat::Msf] {
			let name = match format {
				CatalogFormat::Hang => hang::Catalog::DEFAULT_NAME,
				CatalogFormat::HangZ => hang::Catalog::COMPRESSED_NAME,
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
