//! Subscribe to a moq broadcast and decode media frames.
//!
//! - [`Fmp4`] subscribes to a broadcast, decodes every track via
//!   [`Consumer<Hang>`](crate::container::Consumer), and yields a single fMP4 / CMAF byte
//!   stream, the merged init segment followed by moof+mdat fragments in
//!   timestamp order across tracks.
//! - [`Mkv`] does the same but yields a Matroska / WebM byte stream: EBML
//!   header + unknown-size Segment + Cluster fragments.
//!
//! Codec-shape conversion for Annex-B sources is handled by
//! [`crate::transform`], which both exporters compose internally.

use std::task::Poll;

mod fmp4;
mod mkv;

pub use fmp4::Fmp4;
pub use mkv::Mkv;

#[cfg(test)]
mod test;

use crate::catalog::CatalogFormat;

/// Source for the catalog stream backing an exporter.
///
/// Both variants expose the same [`hang::Catalog`] shape; the MSF variant converts on
/// the fly so the rest of the pipeline only deals with hang types.
pub(super) enum CatalogSource {
	/// The hang catalog track (track name `catalog.json`, JSON payload).
	Hang(crate::catalog::Consumer),
	/// The MSF catalog track (track name `catalog`, MSF JSON payload converted to hang).
	Msf(crate::catalog::MsfConsumer),
}

impl CatalogSource {
	pub(super) fn new(broadcast: &moq_net::BroadcastConsumer, format: CatalogFormat) -> Result<Self, crate::Error> {
		Ok(match format {
			CatalogFormat::Hang => {
				let track = broadcast.subscribe_track(&hang::Catalog::default_track())?;
				CatalogSource::Hang(crate::catalog::Consumer::new(track))
			}
			CatalogFormat::Msf => {
				let track = broadcast.subscribe_track(&moq_net::Track::new(moq_msf::DEFAULT_NAME))?;
				CatalogSource::Msf(crate::catalog::MsfConsumer::new(track))
			}
		})
	}

	pub(super) fn poll_next(&mut self, waiter: &conducer::Waiter) -> Poll<anyhow::Result<Option<hang::Catalog>>> {
		match self {
			Self::Hang(c) => c.poll_next(waiter).map_err(Into::into),
			Self::Msf(c) => c.poll_next(waiter),
		}
	}
}
