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

use crate::catalog::hang::Container as HangContainer;
use crate::codec::h264::Avc1;
use crate::codec::h265::Hvc1;
use crate::container::{Consumer, Frame};

/// Per-track video transform that bridges between codec shapes.
pub(crate) enum VideoTransform {
	Avc1(Avc1),
	Hvc1(Hvc1),
}

impl VideoTransform {
	pub(crate) fn codec_private(&self) -> Option<&Bytes> {
		match self {
			VideoTransform::Avc1(t) => t.avcc(),
			VideoTransform::Hvc1(t) => t.hvcc(),
		}
	}

	pub(crate) fn transform(&mut self, payload: Bytes) -> crate::Result<Option<Bytes>> {
		match self {
			VideoTransform::Avc1(t) => Ok(t.transform(payload)?),
			VideoTransform::Hvc1(t) => Ok(t.transform(payload)?),
		}
	}
}

/// A subscription that resolves on first poll, then the live consumer.
enum SourceState {
	/// Waiting for the target broadcast (the catalog broadcast, or a cross-broadcast
	/// reference) to resolve; the track (by name) is subscribed once it does.
	Requesting(kio::Pending<moq_net::origin::Requesting>, String),
	/// Waiting for the subscription to resolve (blocks on the publisher's SUBSCRIBE_OK).
	Subscribing(kio::Pending<moq_net::track::Subscribing>),
	/// The resolved consumer, reading frames. Boxed because it's much larger than
	/// the `Subscribing` variant (clippy `large_enum_variant`).
	Active(Box<Consumer<HangContainer>>),
}

/// A per-rendition source that normalizes frame shape (Annex-B →
/// length-prefixed for H.264/H.265) and exposes the resolved codec config
/// record alongside the frame stream.
pub(crate) struct ExportSource {
	state: SourceState,
	/// Wire format, consumed when the subscription resolves into a consumer.
	media: Option<HangContainer>,
	latency: Duration,
	transform: Option<VideoTransform>,
	/// Resolved codec configuration record (avcC / hvcC / AudioSpecificConfig /
	/// OpusHead). Some once the codec config is available — from the catalog
	/// `description`, or synthesized by the transform.
	description: Option<Bytes>,
	/// Video codec used to derive geometry from its configuration or keyframes.
	video_codec: Option<VideoCodec>,
	/// Geometry resolved from the initial catalog or codec data received afterward.
	video_dimensions: Option<(u32, u32)>,
}

impl ExportSource {
	/// Subscribe to a video rendition and build an `ExportSource`.
	pub fn for_video(
		source: &crate::Source,
		name: &str,
		config: &VideoConfig,
		latency: Duration,
	) -> Result<Option<Self>, crate::Error> {
		Self::video(source, name, config, latency, build_video_transform(config))
	}

	/// Subscribe to a video rendition without attaching any codec-shape
	/// transform. Payloads pass through untouched (Annex-B stays Annex-B,
	/// avc1 length-prefixed stays length-prefixed). The Annex-B exporter
	/// uses this to keep parameter sets in-band.
	pub fn for_video_raw(
		source: &crate::Source,
		name: &str,
		config: &VideoConfig,
		latency: Duration,
	) -> Result<Option<Self>, crate::Error> {
		Self::video(source, name, config, latency, None)
	}

	fn video(
		source: &crate::Source,
		name: &str,
		config: &VideoConfig,
		latency: Duration,
		transform: Option<VideoTransform>,
	) -> Result<Option<Self>, crate::Error> {
		let media: HangContainer = (&config.container).try_into()?;
		let description = config.description.as_ref().filter(|b| !b.is_empty()).cloned();
		let Some(request) = source.request(config.broadcast.as_ref()) else {
			return Ok(None);
		};

		let mut source = Self {
			state: SourceState::Requesting(request, name.to_string()),
			media: Some(media),
			latency,
			transform,
			description,
			video_codec: Some(config.codec.clone()),
			video_dimensions: catalog_dimensions(config),
		};
		source.resolve_video_dimensions(&[]);
		Ok(Some(source))
	}

	/// Subscribe to an audio rendition. Audio has no codec-shape transform;
	/// `description` is taken straight from the catalog.
	pub fn for_audio(
		source: &crate::Source,
		name: &str,
		config: &AudioConfig,
		latency: Duration,
	) -> Result<Option<Self>, crate::Error> {
		let media: HangContainer = (&config.container).try_into()?;
		let description = config.description.as_ref().filter(|b| !b.is_empty()).cloned();
		let Some(request) = source.request(config.broadcast.as_ref()) else {
			return Ok(None);
		};

		Ok(Some(Self {
			state: SourceState::Requesting(request, name.to_string()),
			media: Some(media),
			latency,
			transform: None,
			description,
			video_codec: None,
			video_dimensions: None,
		}))
	}

	/// Subscribe to a verbatim `mpegts` stream rendition (SCTE-35, private PES, ...).
	/// No codec-shape transform and no description: the frames are Legacy-framed
	/// verbatim bytes the muxer writes back out as PES or private sections.
	pub fn for_stream(source: &crate::Source, name: &str, latency: Duration) -> Result<Self, crate::Error> {
		let request = source.request(None).expect("the catalog broadcast is always valid");
		Ok(Self {
			state: SourceState::Requesting(request, name.to_string()),
			media: Some(HangContainer::Legacy),
			latency,
			transform: None,
			description: None,
			video_codec: None,
			video_dimensions: None,
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

	/// Combine the latest catalog config with geometry resolved from codec data.
	pub fn video_config(&self, config: &VideoConfig) -> Option<VideoConfig> {
		if catalog_dimensions(config).is_some() {
			return Some(config.clone());
		}

		let (width, height) = self.video_dimensions?;
		let mut config = config.clone();
		config.coded_width = Some(width);
		config.coded_height = Some(height);
		Some(config)
	}

	/// True when this codec is unsupported or has enough geometry to build a video header.
	pub fn video_geometry_ready(&self, config: &VideoConfig) -> bool {
		!matches!(
			config.codec,
			VideoCodec::H264(_) | VideoCodec::H265(_) | VideoCodec::VP8 | VideoCodec::VP9(_) | VideoCodec::AV1(_)
		) || self.video_config(config).is_some()
	}

	/// Pull the next normalized frame.
	///
	/// Parameter-only frames (SPS/PPS-only inputs to the Avc3 transform) are
	/// absorbed and the next frame is polled. Returns `Ready(None)` at
	/// end-of-track.
	pub fn poll_read(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Frame>>> {
		// Resolve a cross-broadcast reference into a broadcast before subscribing.
		if matches!(self.state, SourceState::Requesting(..)) {
			let (broadcast, name) = {
				let SourceState::Requesting(pending, name) = &self.state else {
					unreachable!("just matched Requesting");
				};
				match pending.poll_ok(waiter) {
					Poll::Ready(Ok(broadcast)) => (broadcast, name.clone()),
					Poll::Ready(Err(e)) => return Poll::Ready(Err(e.into())),
					Poll::Pending => return Poll::Pending,
				}
			};
			self.state = SourceState::Subscribing(broadcast.track(&name)?.subscribe(None));
		}

		// Resolve the subscription before reading any frames.
		if matches!(self.state, SourceState::Subscribing(_)) {
			// Scope the `pending` borrow so it ends before we touch `self.media`/`self.state`.
			let track = {
				let SourceState::Subscribing(pending) = &self.state else {
					unreachable!("just matched Subscribing");
				};
				match pending.poll_ok(waiter) {
					Poll::Ready(Ok(track)) => track,
					Poll::Ready(Err(e)) => return Poll::Ready(Err(e.into())),
					Poll::Pending => return Poll::Pending,
				}
			};
			let media = self
				.media
				.take()
				.expect("media present until the subscription resolves");
			self.state = SourceState::Active(Box::new(Consumer::new(track, media).with_latency(self.latency)));
		}

		loop {
			// Scope the consumer borrow to the poll so `self.transform` /
			// `self.refresh_description` can borrow `self` afterwards.
			let frame = {
				let SourceState::Active(consumer) = &mut self.state else {
					unreachable!("subscription resolved into an Active consumer");
				};
				match consumer.poll_read(waiter) {
					Poll::Ready(Ok(Some(f))) => f,
					Poll::Ready(Ok(None)) => return Poll::Ready(Ok(None)),
					Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
					Poll::Pending => return Poll::Pending,
				}
			};

			let Some(transform) = self.transform.as_mut() else {
				self.resolve_video_dimensions(&frame.payload);
				return Poll::Ready(Ok(Some(frame)));
			};

			match transform.transform(frame.payload.clone())? {
				None => {
					// Parameter set absorbed by the transform. Refresh the
					// resolved description (it may have just become available)
					// and pull the next frame.
					self.refresh_description();
					self.resolve_video_dimensions(&frame.payload);
					continue;
				}
				Some(payload) => {
					self.refresh_description();
					self.resolve_video_dimensions(&payload);
					return Poll::Ready(Ok(Some(Frame { payload, ..frame })));
				}
			}
		}
	}

	fn refresh_description(&mut self) {
		// Track the transform's record even after it is first set: a mid-stream
		// reconfiguration rebuilds the avcC/hvcC with a new parameter set, and the
		// muxer re-injects from this on every keyframe, so a stale record would
		// carry superseded SPS/PPS.
		if let Some(transform) = self.transform.as_ref()
			&& let Some(d) = transform.codec_private()
			&& self.description.as_ref() != Some(d)
		{
			self.description = Some(d.clone());
		}
	}

	fn resolve_video_dimensions(&mut self, payload: &[u8]) {
		if self.video_dimensions.is_some() {
			return;
		}
		let Some(codec) = self.video_codec.as_ref() else {
			return;
		};

		let dimensions = match codec {
			VideoCodec::H264(_) => self
				.description
				.as_deref()
				.and_then(|description| crate::codec::h264::config(description).ok())
				.as_ref()
				.and_then(catalog_dimensions),
			VideoCodec::H265(_) => self
				.description
				.as_deref()
				.and_then(|description| crate::codec::h265::config(description).ok())
				.as_ref()
				.and_then(catalog_dimensions),
			VideoCodec::VP8 if !payload.is_empty() => crate::codec::vp8::FrameHeader::parse(payload)
				.ok()
				.and_then(|header| header.dimensions)
				.map(|(width, height)| (u32::from(width), u32::from(height))),
			VideoCodec::VP9(_) if !payload.is_empty() => crate::codec::vp9::config_from_keyframe(payload)
				.ok()
				.flatten()
				.as_ref()
				.and_then(catalog_dimensions),
			VideoCodec::AV1(_) if !payload.is_empty() => crate::codec::av1::dimensions(payload).ok().flatten(),
			_ => None,
		};

		if dimensions.is_some_and(|(width, height)| width > 0 && height > 0) {
			self.video_dimensions = dimensions;
		}
	}
}

fn catalog_dimensions(config: &VideoConfig) -> Option<(u32, u32)> {
	let dimensions = (config.coded_width?, config.coded_height?);
	(dimensions.0 > 0 && dimensions.1 > 0).then_some(dimensions)
}

/// Build a video transform for an Annex-B source, or `None` if the catalog
/// already provides an out-of-band description.
pub(crate) fn build_video_transform(config: &VideoConfig) -> Option<VideoTransform> {
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
