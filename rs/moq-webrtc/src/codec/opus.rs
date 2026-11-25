//! Opus bridge.
//!
//! str0m hands us one Opus packet per frame, which is exactly the
//! raw shape that [`moq_mux::codec::opus::Import`] consumes.

use crate::{Result, codec};

pub struct Bridge {
	import: moq_mux::codec::opus::Import,
	join: moq_mux::container::Join,
}

impl Bridge {
	pub fn new(
		broadcast: moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::Producer,
		sample_rate: u32,
		channel_count: u32,
	) -> Result<Self> {
		let config = moq_mux::codec::opus::Config::new(sample_rate, channel_count);
		let track = broadcast.unique_track(".opus", catalog.track_info(hang::catalog::PRIORITY.audio));
		let import = moq_mux::codec::opus::Import::new(track, catalog.reserve(), config.into())?;
		Ok(Self {
			import,
			join: moq_mux::container::Join::default(),
		})
	}
}

impl codec::Bridge for Bridge {
	fn push(&mut self, frame: codec::Frame) -> Result<()> {
		let pts = moq_net::Timestamp::from_micros(frame.timestamp_us).map_err(moq_mux::Error::from)?;
		// The packet states its own length; one that does not counts as none.
		let end = moq_mux::codec::opus::packet_samples(&frame.payload)
			.map(|samples| {
				frame
					.timestamp_us
					.saturating_add(u64::from(samples) * 1_000_000 / 48_000)
			})
			.map(moq_net::Timestamp::from_micros)
			.transpose()
			.map_err(moq_mux::Error::from)?;
		// The importer writes each packet at once and only accumulates the group, so the packet that
		// fills a join closes it, and the relay sees the group finish without waiting for the next.
		self.import.decode(&frame.payload, Some(pts))?;
		if self.join.fill(pts, end.unwrap_or(pts)) {
			self.import.cut(end)?;
		}
		Ok(())
	}

	fn discontinuity(&mut self) -> Result<()> {
		self.import.discontinuity()?;
		self.join.reset();
		Ok(())
	}

	fn finish(self: Box<Self>) -> Result<codec::Finished> {
		Ok(Box::new(self.import.finish()?))
	}

	fn abort(self: Box<Self>, err: moq_net::Error) {
		self.import.abort(err);
	}
}
