//! Per-rendition export source that normalizes frame shape and exposes the
//! resolved codec configuration record.
//!
//! Exporters declare what wire shape they want their frames in (currently:
//! avc1/hvc1 length-prefixed for H.264/H.265) and call [`ExportSource::poll_read`]
//! to pull normalized frames. For Annex-B sources (catalog codec marked
//! `inline: true` / `in_band: true`, empty `description`) the source attaches
//! an [`Avc1`] / [`Hvc1`] transform that caches parameter sets, synthesizes
//! the codec config record, and length-prefixes slice NALs. Frame emission
//! is deferred until the transform has produced its config record.
//!
//! `description()` returns the resolved codec config: either the catalog's
//! existing `description` (for already-out-of-band sources) or the synthesized
//! avcC/hvcC (for Annex-B sources).

use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;
use hang::catalog::{AudioConfig, VideoCodec, VideoConfig};

use crate::catalog::CatalogFormat;
use crate::catalog::hang::Container as HangContainer;
use crate::codec::h264::Avc1;
use crate::codec::h265::Hvc1;
use crate::container::{Consumer, Frame};

/// Source for the catalog stream backing an exporter.
///
/// Both variants expose the same [`hang::Catalog`] shape; the MSF variant
/// converts on the fly so the rest of the pipeline only deals with hang types.
pub(crate) enum CatalogSource {
	/// The hang catalog track (track name `catalog.json`, JSON payload).
	Hang(crate::catalog::hang::Consumer),
	/// The MSF catalog track (track name `catalog`, MSF JSON payload converted to hang).
	Msf(crate::catalog::msf::Consumer),
}

impl CatalogSource {
	pub(crate) fn new(broadcast: &moq_net::BroadcastConsumer, format: CatalogFormat) -> Result<Self, crate::Error> {
		Ok(match format {
			CatalogFormat::Hang => {
				let track = broadcast.subscribe_track(&hang::Catalog::default_track())?;
				CatalogSource::Hang(crate::catalog::hang::Consumer::new(track))
			}
			CatalogFormat::Msf => {
				let track = broadcast.subscribe_track(&moq_net::Track::new(moq_msf::DEFAULT_NAME))?;
				CatalogSource::Msf(crate::catalog::msf::Consumer::new(track))
			}
		})
	}

	pub(crate) fn poll_next(&mut self, waiter: &conducer::Waiter) -> Poll<crate::Result<Option<hang::Catalog>>> {
		match self {
			Self::Hang(c) => c.poll_next(waiter).map_err(Into::into),
			Self::Msf(c) => c.poll_next(waiter),
		}
	}
}

/// Per-track video transform that bridges between codec shapes.
pub(crate) enum VideoTransform {
	Avc1(Avc1),
	Hvc1(Hvc1),
}

impl VideoTransform {
	fn codec_private(&self) -> Option<&Bytes> {
		match self {
			VideoTransform::Avc1(t) => t.avcc(),
			VideoTransform::Hvc1(t) => t.hvcc(),
		}
	}

	fn transform(&mut self, payload: Bytes) -> crate::Result<Option<Bytes>> {
		match self {
			VideoTransform::Avc1(t) => Ok(t.transform(payload)?),
			VideoTransform::Hvc1(t) => Ok(t.transform(payload)?),
		}
	}
}

/// A per-rendition source that normalizes frame shape (Annex-B →
/// length-prefixed for H.264/H.265) and exposes the resolved codec config
/// record alongside the frame stream.
pub(crate) struct ExportSource {
	consumer: Consumer<HangContainer>,
	transform: Option<VideoTransform>,
	/// Resolved codec configuration record (avcC / hvcC / AudioSpecificConfig /
	/// OpusHead). Some once the codec config is available — from the catalog
	/// `description`, or synthesized by the transform.
	description: Option<Bytes>,
}

impl ExportSource {
	/// Subscribe to a video rendition and build an `ExportSource`.
	pub fn for_video(
		broadcast: &moq_net::BroadcastConsumer,
		name: &str,
		config: &VideoConfig,
		latency: Duration,
	) -> Result<Self, crate::Error> {
		let media: HangContainer = (&config.container).try_into()?;
		let track = broadcast.subscribe_track(&moq_net::Track::new(name.to_string()))?;
		let consumer = Consumer::new(track, media).with_latency(latency);

		let transform = build_video_transform(config);
		let description = config.description.as_ref().filter(|b| !b.is_empty()).cloned();

		Ok(Self {
			consumer,
			transform,
			description,
		})
	}

	/// Subscribe to an audio rendition. Audio has no codec-shape transform;
	/// `description` is taken straight from the catalog.
	pub fn for_audio(
		broadcast: &moq_net::BroadcastConsumer,
		name: &str,
		config: &AudioConfig,
		latency: Duration,
	) -> Result<Self, crate::Error> {
		let media: HangContainer = (&config.container).try_into()?;
		let track = broadcast.subscribe_track(&moq_net::Track::new(name.to_string()))?;
		let consumer = Consumer::new(track, media).with_latency(latency);
		let description = config.description.as_ref().filter(|b| !b.is_empty()).cloned();

		Ok(Self {
			consumer,
			transform: None,
			description,
		})
	}

	/// The resolved codec-config record, if available.
	pub fn description(&self) -> Option<&Bytes> {
		self.description.as_ref()
	}

	/// True if the codec config is resolved (either present in the catalog,
	/// no transform attached, or the transform has built its record).
	pub fn header_ready(&self) -> bool {
		self.transform.is_none() || self.description.is_some()
	}

	/// Pull the next normalized frame.
	///
	/// Parameter-only frames (SPS/PPS-only inputs to the Avc3 transform) are
	/// absorbed and the next frame is polled. Returns `Ready(None)` at
	/// end-of-track.
	pub fn poll_read(&mut self, waiter: &conducer::Waiter) -> Poll<crate::Result<Option<Frame>>> {
		loop {
			let frame = match self.consumer.poll_read(waiter) {
				Poll::Ready(Ok(Some(f))) => f,
				Poll::Ready(Ok(None)) => return Poll::Ready(Ok(None)),
				Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
				Poll::Pending => return Poll::Pending,
			};

			let Some(transform) = self.transform.as_mut() else {
				return Poll::Ready(Ok(Some(frame)));
			};

			match transform.transform(frame.payload.clone())? {
				None => {
					// Parameter set absorbed by the transform. Refresh the
					// resolved description (it may have just become available)
					// and pull the next frame.
					self.refresh_description();
					continue;
				}
				Some(payload) => {
					self.refresh_description();
					return Poll::Ready(Ok(Some(Frame { payload, ..frame })));
				}
			}
		}
	}

	fn refresh_description(&mut self) {
		if self.description.is_some() {
			return;
		}
		if let Some(transform) = self.transform.as_ref()
			&& let Some(d) = transform.codec_private()
		{
			self.description = Some(d.clone());
		}
	}
}

/// Build a video transform for an Annex-B source, or `None` if the catalog
/// already provides an out-of-band description.
fn build_video_transform(config: &VideoConfig) -> Option<VideoTransform> {
	let needs_transform = config.description.as_ref().map(|d| d.is_empty()).unwrap_or(true);
	if !needs_transform {
		return None;
	}
	match &config.codec {
		VideoCodec::H264(_) => Some(VideoTransform::Avc1(Avc1::new())),
		VideoCodec::H265(_) => Some(VideoTransform::Hvc1(Hvc1::new())),
		_ => None,
	}
}
