//! Unified catalog consumer.
//!
//! Subscribes to whichever catalog track ([`hang`] or [`msf`]) the broadcast
//! advertises and yields [`Catalog<E>`](super::hang::Catalog) snapshots so callers
//! and exporters only deal with one shape.

use std::task::{Poll, ready};

use super::hang::{Catalog, CatalogExt};
use super::{CatalogFormat, Stream};

/// A catalog stream sourced from a [`moq_net::BroadcastConsumer`].
///
/// Both variants emit [`Catalog<E>`](super::hang::Catalog); the MSF variant is
/// media-only, so its extension is always the default. Wrap with
/// [`Select`](super::Select) to narrow the rendition set before handing the
/// stream to an exporter.
///
/// The variants are an implementation detail: drive it through the [`Stream`]
/// trait rather than matching on them. New catalog encodings may be added.
#[non_exhaustive]
pub enum Consumer<E: CatalogExt = ()> {
	#[doc(hidden)]
	Hang(super::hang::Consumer<E>),
	#[doc(hidden)]
	Msf(super::msf::Consumer),
}

impl<E: CatalogExt> Consumer<E> {
	/// Subscribe to the catalog track advertised by `format`.
	pub async fn new(broadcast: &moq_net::BroadcastConsumer, format: CatalogFormat) -> Result<Self, crate::Error> {
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
		match self {
			Self::Hang(c) => c.poll_next(waiter),
			Self::Msf(c) => {
				// MSF carries only the media sections, so the extension defaults.
				let media = match ready!(c.poll_next(waiter)) {
					Ok(media) => media,
					Err(err) => return Poll::Ready(Err(err)),
				};
				Poll::Ready(Ok(media.map(|m| Catalog::<E> {
					video: m.video,
					audio: m.audio,
					ext: E::default(),
				})))
			}
		}
	}
}
