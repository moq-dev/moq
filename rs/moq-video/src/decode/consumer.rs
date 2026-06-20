//! Subscribe to an encoded H.264 track and emit raw I420 frames.

use std::collections::VecDeque;

use bytes::Bytes;
use hang::catalog::VideoConfig;

use super::Frame;
use super::decoder::{Config, Decoder};
use crate::Error;

/// Subscribe to a moq-mux H.264 track and emit decoded I420.
///
/// The codec/backend are fixed at construction; [`read`](Self::read) returns
/// plain [`Frame`]s. The direct mirror of `moq_audio::AudioConsumer`.
pub struct Consumer {
	decoder: Decoder,
	track: moq_mux::container::Consumer<moq_mux::container::legacy::Wire>,
	/// Frames a single access unit decoded to but `read` hasn't returned yet.
	/// One AU yields one frame in the low-delay path, but a backend may hand back
	/// more, so we buffer to keep `read` one-frame-per-call.
	pending: VecDeque<Frame>,
}

impl Consumer {
	/// Subscribe to `name` in `broadcast`, decoding it per the catalog entry.
	/// Errors if the rendition isn't H.264.
	pub async fn new(
		broadcast: &moq_net::BroadcastConsumer,
		catalog: &VideoConfig,
		name: impl Into<String>,
		config: Config,
	) -> Result<Self, Error> {
		let decoder = Decoder::new(catalog, &config.kind)?;

		let name = name.into();
		let track = broadcast.track(&name)?.subscribe(None)?.await?;
		let mut track = moq_mux::container::Consumer::new(track, moq_mux::container::legacy::Wire);
		if let Some(latency) = config.latency_max {
			track = track.with_latency(latency);
		}

		Ok(Self {
			decoder,
			track,
			pending: VecDeque::new(),
		})
	}

	/// The decoder backend name in use, e.g. `"videotoolbox"` or `"openh264"`.
	pub fn name(&self) -> &str {
		self.decoder.name()
	}

	/// Read the next decoded I420 frame, or `None` when the track ends.
	pub async fn read(&mut self) -> Result<Option<Frame>, Error> {
		loop {
			if let Some(frame) = self.pending.pop_front() {
				return Ok(Some(frame));
			}

			let Some(mux_frame) = self.track.read().await? else {
				return Ok(None);
			};
			let timestamp_us: u64 = mux_frame
				.timestamp
				.as_micros()
				.try_into()
				.map_err(|_| moq_net::TimeOverflow)?;

			for i420 in self.decoder.decode(&mux_frame.payload, mux_frame.keyframe)? {
				self.pending.push_back(Frame {
					timestamp_us,
					width: i420.width,
					height: i420.height,
					data: Bytes::from(i420.data),
				});
			}
		}
	}
}
