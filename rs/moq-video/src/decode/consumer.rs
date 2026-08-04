//! Subscribe to an encoded H.264, H.265, or AV1 track and emit raw I420 frames.

use std::collections::VecDeque;

use hang::catalog::VideoConfig;

use super::decoder::Config;
use super::sink::Sink;
use crate::Error;
use crate::Frame;

/// Subscribe to a moq-mux video track and emit decoded I420.
///
/// The codec/backend are fixed at construction; [`read`](Self::read) returns
/// plain [`Frame`]s. The direct mirror of `moq_audio::decode::Consumer`.
pub struct Consumer {
	/// A [`Sink`] rather than a bare `Decoder`: the read loop below is held
	/// across `.await` by every caller (libmoq's spawned task, moq-transcode),
	/// so the codec would otherwise migrate between executor workers and
	/// unbalance the per-thread COM apartment the Windows backend opens.
	decoder: Sink,
	track: moq_mux::container::Consumer<moq_mux::container::legacy::Wire>,
	/// Frames a single access unit decoded to but `read` hasn't returned yet.
	/// One AU yields one frame in the low-delay path, but a backend may hand back
	/// more, so we buffer to keep `read` one-frame-per-call.
	pending: VecDeque<Frame>,
}

impl Consumer {
	/// Subscribe to `name` in `broadcast`, decoding it per the catalog entry.
	/// Errors if the rendition's codec is not supported by a native backend.
	pub async fn new(
		broadcast: &moq_net::broadcast::Consumer,
		catalog: &VideoConfig,
		name: impl Into<String>,
		config: Config,
	) -> Result<Self, Error> {
		let decoder = Sink::open(catalog, &config).await?;

		let name = name.into();
		let track = broadcast
			.track(&name)?
			.subscribe(moq_net::track::Subscription::default().with_priority(hang::catalog::PRIORITY.video))
			.await?;
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

			self.pending.extend(
				self.decoder
					.decode(mux_frame.payload, mux_frame.timestamp, mux_frame.keyframe)
					.await?,
			);
		}
	}
}
