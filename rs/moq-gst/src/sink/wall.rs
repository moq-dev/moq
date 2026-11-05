//! The wall-clock time of running-time zero, shared by every pad in a publication.

use std::sync::{
	Arc, Mutex,
	atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use anyhow::{Context, Result};

/// The pipeline running time and local wall time, read together.
#[derive(Clone, Copy)]
pub(super) struct Sample {
	pub running: Option<gst::ClockTime>,
	pub wall: kio::clock::Wall,
}

/// Places running-time zero on the wall clock once per run, holding the first catalog until then.
pub(super) struct Wall {
	timebase: moq_mux::catalog::Timebase,
	// CAPS can resolve every rendition before the first buffer supplies its reference timestamp.
	pending: Mutex<Option<moq_mux::catalog::Reserved>>,
	placed: AtomicBool,
}

impl Wall {
	pub(super) fn new(catalog: &moq_mux::catalog::Producer) -> Arc<Self> {
		let timebase = catalog.timebase();
		Arc::new(Self {
			pending: Mutex::new(Some(timebase.reserve())),
			timebase,
			placed: AtomicBool::new(false),
		})
	}

	pub(super) fn place(&self, buffer: &gst::BufferRef, pts: Duration, sample: Sample) -> Result<()> {
		if self.placed.load(Ordering::Acquire) {
			return Ok(());
		}
		let mut pending = self.pending.lock().unwrap();
		if pending.is_none() {
			return Ok(());
		}
		let wall = wall_zero(buffer, pts, sample)?;
		self.timebase.place(hang::container::Timestamp::from_micros(0)?, wall)?;
		self.placed.store(true, Ordering::Release);
		pending.take();
		Ok(())
	}

	/// A run without buffers still closes its catalog, without leaving a reservation outstanding.
	pub(super) fn finish(&self) {
		self.pending.lock().unwrap().take();
	}
}

fn wall_zero(buffer: &gst::BufferRef, pts: Duration, sample: Sample) -> Result<kio::clock::Wall> {
	for meta in buffer.iter_meta::<gst::ReferenceTimestampMeta>() {
		let Some(reference) = meta.reference().structure(0) else {
			continue;
		};
		let timestamp = Duration::from_nanos(meta.timestamp().nseconds());
		let unix = match reference.name().as_str() {
			"timestamp/x-unix" => timestamp,
			"timestamp/x-ntp" => timestamp
				.checked_sub(Duration::from_secs(2_208_988_800))
				.context("NTP reference timestamp is before the UNIX epoch")?,
			// PTP and device clocks need an explicit UTC relation; their names do not provide one.
			_ => continue,
		};
		return kio::clock::Wall::UNIX_EPOCH
			.checked_add(unix)
			.and_then(|wall| wall.checked_sub(pts))
			.context("reference timestamp cannot place running-time zero");
	}
	// Use the pipeline's current running time, not this buffer's PTS: delayed first buffers
	// must retain their capture time instead of becoming live on arrival.
	sample
		.wall
		.checked_sub(Duration::from_nanos(
			sample
				.running
				.context("pipeline has no running-time reference")?
				.nseconds(),
		))
		.context("pipeline running time cannot place wall-clock zero")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn delayed_buffers_and_unknown_domains_use_the_pipeline_clock() {
		gst::init().unwrap();
		let now = kio::clock::Wall::UNIX_EPOCH + Duration::from_secs(1_735_689_620);
		let mut buffer = gst::Buffer::new();
		gst::ReferenceTimestampMeta::add(
			buffer.get_mut().unwrap(),
			&gst::Caps::builder("timestamp/x-ptp").build(),
			gst::ClockTime::from_seconds(17),
			None,
		);
		for pts in [0, 3_000_000, 19_000_000] {
			assert_eq!(
				wall_zero(
					&buffer,
					Duration::from_micros(pts),
					Sample {
						running: Some(gst::ClockTime::from_seconds(20)),
						wall: now
					}
				)
				.unwrap(),
				now - Duration::from_secs(20)
			);
		}
		assert!(
			wall_zero(
				&buffer,
				Duration::ZERO,
				Sample {
					running: None,
					wall: now
				}
			)
			.is_err()
		);
	}

	#[test]
	fn unix_and_ntp_references_place_the_same_wall_time() {
		gst::init().unwrap();
		for (domain, seconds) in [("timestamp/x-unix", 1_735_689_603), ("timestamp/x-ntp", 3_944_678_403)] {
			let mut buffer = gst::Buffer::new();
			gst::ReferenceTimestampMeta::add(
				buffer.get_mut().unwrap(),
				&gst::Caps::builder(domain).build(),
				gst::ClockTime::from_seconds(seconds),
				None,
			);
			assert_eq!(
				wall_zero(
					&buffer,
					Duration::from_secs(3),
					Sample {
						running: None,
						wall: kio::clock::Wall::UNIX_EPOCH
					}
				)
				.unwrap(),
				kio::clock::Wall::UNIX_EPOCH + Duration::from_secs(1_735_689_600)
			);
		}
	}
}
