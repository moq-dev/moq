//! Unified catalog consumer.
//!
//! Subscribes to whichever catalog track ([`hang`] or [`msf`]) the broadcast
//! advertises and yields [`hang::Catalog`] snapshots so callers and exporters
//! only deal with one shape.

use std::task::Poll;

use hang::Catalog;

use super::{CatalogFormat, Stream};

/// A catalog stream sourced from a [`moq_net::BroadcastConsumer`].
///
/// Both variants emit [`hang::Catalog`]; the MSF variant converts each snapshot
/// on the fly. Wrap with [`Filter`](super::Filter) / [`Target`](super::Target)
/// to narrow the rendition set before handing the stream to an exporter.
pub enum Consumer {
	Hang(super::hang::Consumer),
	Msf(super::msf::Consumer),
}

impl Consumer {
	/// Subscribe to the catalog track advertised by `format`.
	pub fn new(broadcast: &moq_net::BroadcastConsumer, format: CatalogFormat) -> Result<Self, crate::Error> {
		Ok(match format {
			CatalogFormat::Hang => {
				let track = broadcast.subscribe_track(&hang::Catalog::default_track())?;
				Self::Hang(super::hang::Consumer::new(track))
			}
			CatalogFormat::Msf => {
				let track = broadcast.subscribe_track(&moq_net::Track::new(moq_msf::DEFAULT_NAME))?;
				Self::Msf(super::msf::Consumer::new(track))
			}
		})
	}
}

impl Stream for Consumer {
	fn poll_next(&mut self, waiter: &conducer::Waiter) -> Poll<crate::Result<Option<Catalog>>> {
		match self {
			Self::Hang(c) => c.poll_next(waiter),
			Self::Msf(c) => c.poll_next(waiter).map_err(Into::into),
		}
	}
}
