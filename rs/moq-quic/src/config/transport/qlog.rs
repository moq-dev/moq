//! qlog trace capture

use std::{
	io,
	sync::{Arc, Mutex},
};

use qlog::streamer::QlogStreamer;

use crate::Instant;
pub use crate::connection::qlog::QlogStream as Stream;

/// Configuration for qlog trace logging
#[non_exhaustive]
pub struct Config {
	/// Where to write a qlog `TraceSeq`, or `None` to capture nothing
	pub writer: Option<Box<dyn io::Write + Send + Sync>>,
	/// Title to record in the qlog capture
	pub title: Option<String>,
	/// Description to record in the qlog capture
	pub description: Option<String>,
	/// Epoch qlog event times are recorded relative to, now by default
	pub start_time: Instant,
}

impl Config {
	/// Construct the [`Stream`] described by this configuration
	pub fn into_stream(self) -> Option<Stream> {
		use tracing::warn;

		let writer = self.writer?;
		let trace = qlog::TraceSeq::new(
			self.title.clone(),
			self.description.clone(),
			None,
			Some(qlog::VantagePoint {
				name: None,
				ty: qlog::VantagePointType::Unknown,
				flow: None,
			}),
			vec![],
		);

		// The streamer's own epoch only times the events it stamps itself, which this
		// crate never asks it to; `Stream` stamps them on MoQ's clock instead.
		let mut streamer = QlogStreamer::new(
			self.title,
			self.description,
			moq_clock::now().into_std(),
			trace,
			qlog::events::EventImportance::Core,
			// `Instant` should have sub-microsecond precision on most platforms
			qlog::streamer::EventTimePrecision::NanoSeconds,
			writer,
		);

		match streamer.start_log() {
			Ok(()) => Some(Stream {
				streamer: Arc::new(Mutex::new(streamer)),
				start: self.start_time,
			}),
			Err(e) => {
				warn!("could not initialize endpoint qlog streamer: {e}");
				None
			}
		}
	}
}

impl Default for Config {
	fn default() -> Self {
		Self {
			writer: None,
			title: None,
			description: None,
			start_time: Instant::now(),
		}
	}
}

impl std::fmt::Debug for Config {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Config")
			.field("writer", &self.writer.is_some())
			.field("title", &self.title)
			.field("description", &self.description)
			.field("start_time", &self.start_time)
			.finish()
	}
}
