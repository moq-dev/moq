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
	max_age: std::time::Duration,
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
	///
	/// Fails with [`Error::EscapingBroadcast`](crate::Error::EscapingBroadcast) if the catalog
	/// `broadcast` reference escapes above the root, naming no broadcast to subscribe on. The
	/// catalog stream rejects such a reference first, so this is a backstop for a caller that
	/// built the config itself.
	pub fn for_video(
		source: &crate::Source,
		name: &str,
		config: &VideoConfig,
		max_age: std::time::Duration,
	) -> Result<Option<Self>, crate::Error> {
		Self::video(source, name, config, max_age, build_video_transform(config))
	}

	/// Subscribe to a video rendition without attaching any codec-shape
	/// transform. Payloads pass through untouched (Annex-B stays Annex-B,
	/// avc1 length-prefixed stays length-prefixed). The Annex-B exporter
	/// uses this to keep parameter sets in-band.
	///
	/// Rejects an escaping `broadcast` reference like [`Self::for_video`].
	pub fn for_video_raw(
		source: &crate::Source,
		name: &str,
		config: &VideoConfig,
		max_age: std::time::Duration,
	) -> Result<Option<Self>, crate::Error> {
		Self::video(source, name, config, max_age, None)
	}

	fn video(
		source: &crate::Source,
		name: &str,
		config: &VideoConfig,
		max_age: std::time::Duration,
		transform: Option<VideoTransform>,
	) -> Result<Option<Self>, crate::Error> {
		let media: HangContainer = (&config.container).try_into()?;
		let description = config.description.as_ref().filter(|b| !b.is_empty()).cloned();
		let Some(request) = source.try_request(config.broadcast.as_ref()) else {
			return Ok(None);
		};

		let mut source = Self {
			state: SourceState::Requesting(request, name.to_string()),
			media: Some(media),
			max_age,
			transform,
			description,
			video_codec: Some(config.codec.clone()),
			video_dimensions: catalog_dimensions(config),
		};
		source.resolve_video_dimensions(&[])?;
		Ok(Some(source))
	}

	/// Subscribe to an audio rendition. Audio has no codec-shape transform;
	/// `description` is taken straight from the catalog.
	///
	/// Rejects an escaping `broadcast` reference like [`Self::for_video`].
	pub fn for_audio(
		source: &crate::Source,
		name: &str,
		config: &AudioConfig,
		max_age: std::time::Duration,
	) -> Result<Option<Self>, crate::Error> {
		let media: HangContainer = (&config.container).try_into()?;
		let description = config.description.as_ref().filter(|b| !b.is_empty()).cloned();
		let Some(request) = source.try_request(config.broadcast.as_ref()) else {
			return Ok(None);
		};

		Ok(Some(Self {
			state: SourceState::Requesting(request, name.to_string()),
			media: Some(media),
			max_age,
			transform: None,
			description,
			video_codec: None,
			video_dimensions: None,
		}))
	}

	/// Subscribe to a verbatim `mpegts` stream rendition (SCTE-35, private PES, ...).
	/// No codec-shape transform and no description: the frames are Legacy-framed
	/// verbatim bytes the muxer writes back out as PES or private sections.
	///
	/// Such a rendition has no catalog `broadcast` field, so it always lives on the
	/// catalog broadcast and can never carry an escaping reference.
	pub fn for_stream(source: &crate::Source, name: &str, max_age: std::time::Duration) -> Result<Self, crate::Error> {
		Ok(Self {
			state: SourceState::Requesting(source.request_catalog(), name.to_string()),
			media: Some(HangContainer::Legacy),
			max_age,
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
			let subscription = moq_net::track::Subscription::default().with_max_age(self.max_age);
			self.state = SourceState::Subscribing(broadcast.track(&name)?.subscribe(subscription));
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
			self.state = SourceState::Active(Box::new(Consumer::new(track, media)));
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
				self.resolve_video_dimensions(&frame.payload)?;
				return Poll::Ready(Ok(Some(frame)));
			};

			match transform.transform(frame.payload.clone())? {
				None => {
					// Parameter set absorbed by the transform. Refresh the
					// resolved description (it may have just become available)
					// and pull the next frame.
					self.refresh_description();
					self.resolve_video_dimensions(&frame.payload)?;
					continue;
				}
				Some(payload) => {
					self.refresh_description();
					self.resolve_video_dimensions(&payload)?;
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

	fn resolve_video_dimensions(&mut self, payload: &[u8]) -> crate::Result<()> {
		if self.video_dimensions.is_some() {
			return Ok(());
		}
		let Some(codec) = self.video_codec.as_ref() else {
			return Ok(());
		};
		self.video_dimensions = codec_dimensions(codec, self.description.as_deref(), payload)?;
		Ok(())
	}
}

pub(crate) fn catalog_dimensions(config: &VideoConfig) -> Option<(u32, u32)> {
	let dimensions = (config.coded_width?, config.coded_height?);
	(dimensions.0 > 0 && dimensions.1 > 0).then_some(dimensions)
}

/// Resolve encoded dimensions from codec configuration or an in-band keyframe.
pub(crate) fn codec_dimensions(
	codec: &VideoCodec,
	description: Option<&[u8]>,
	payload: &[u8],
) -> crate::Result<Option<(u32, u32)>> {
	let dimensions = match codec {
		VideoCodec::H264(_) => match description {
			Some(description) => catalog_dimensions(&crate::codec::h264::config(description)?),
			None => None,
		},
		VideoCodec::H265(_) => match description {
			Some(description) => catalog_dimensions(&crate::codec::h265::config(description)?),
			None => None,
		},
		VideoCodec::VP8 if !payload.is_empty() => crate::codec::vp8::FrameHeader::parse(payload)?
			.dimensions
			.map(|(width, height)| (u32::from(width), u32::from(height))),
		VideoCodec::VP9(_) if !payload.is_empty() => crate::codec::vp9::config_from_keyframe(payload)?
			.as_ref()
			.and_then(catalog_dimensions),
		VideoCodec::AV1(_) if !payload.is_empty() => crate::codec::av1::dimensions(payload)?,
		_ => None,
	};

	Ok(dimensions.filter(|(width, height)| *width > 0 && *height > 0))
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

#[cfg(test)]
mod tests {
	use hang::catalog::{AudioCodec, Container, H264};
	use moq_net::PathRelative;

	use super::*;
	use crate::container::test_util::Live;

	fn video(broadcast: Option<&str>) -> VideoConfig {
		let mut config = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0xc0,
			level: 0x1f,
			inline: true,
		});
		config.container = Container::Legacy;
		config.broadcast = broadcast.map(|b| PathRelative::new(b).into_owned());
		config
	}

	fn audio(broadcast: Option<&str>) -> AudioConfig {
		let mut config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		config.container = Container::Legacy;
		config.broadcast = broadcast.map(|b| PathRelative::new(b).into_owned());
		config
	}

	/// A rendition whose `broadcast` escapes the root is skipped, rather than resolving
	/// against whatever broadcast the clamped path lands on. The test origin serves every
	/// path it is asked for, so a clamped request would happily resolve.
	#[tokio::test]
	async fn escaping_reference_skips_the_rendition() {
		let live = Live::avc3();
		let source = live.source();
		let max_age = std::time::Duration::ZERO;

		let escaping = |result: Result<Option<ExportSource>, crate::Error>, what: &str| match result {
			Ok(None) => {}
			Err(err) => panic!("{what} failed instead of being skipped: {err:?}"),
			Ok(Some(_)) => panic!("{what} should not resolve"),
		};

		// A reference resolves against the catalog's parent, and the source is rooted at a
		// single-segment path, so its parent is the root and any `..` walks above it.
		for reference in ["..", "../source", "../../elsewhere"] {
			let config = video(Some(reference));
			escaping(ExportSource::for_video(&source, "video", &config, max_age), reference);
			escaping(
				ExportSource::for_video_raw(&source, "video", &config, max_age),
				reference,
			);
			escaping(
				ExportSource::for_audio(&source, "audio", &audio(Some(reference)), max_age),
				reference,
			);
		}
	}

	/// A legal reference still resolves, as does an absent or empty one (the catalog's own
	/// broadcast). A lone `.` names the parent, which here is the root.
	#[tokio::test]
	async fn legal_reference_keeps_the_rendition() {
		let live = Live::avc3();
		let source = live.source();
		let max_age = std::time::Duration::ZERO;

		for reference in [None, Some(""), Some("./source"), Some("sub"), Some(".")] {
			ExportSource::for_video(&source, "video", &video(reference), max_age)
				.unwrap_or_else(|err| panic!("{reference:?} should keep the rendition: {err:?}"))
				.unwrap_or_else(|| panic!("{reference:?} should keep the rendition"));
			ExportSource::for_audio(&source, "audio", &audio(reference), max_age)
				.unwrap_or_else(|err| panic!("{reference:?} should keep the rendition: {err:?}"))
				.unwrap_or_else(|| panic!("{reference:?} should keep the rendition"));
		}
	}

	/// The requested budget must be present on the first SUBSCRIBE. Updating it
	/// after SUBSCRIBE_OK cannot recover backlog the publisher already skipped.
	#[tokio::test]
	async fn latency_is_sent_with_the_initial_subscription() {
		let live = Live::avc3();
		let max_age = std::time::Duration::from_secs(10);
		let mut export = ExportSource::for_video(&live.source(), live.track.name(), &video(None), max_age)
			.unwrap()
			.expect("fixture should produce a video rendition");

		let observed = kio::wait(|waiter| {
			let _ = export.poll_read(waiter);
			match live.track.subscription() {
				Some(subscription) => Poll::Ready(subscription.max_age),
				None => Poll::Pending,
			}
		})
		.await;

		assert_eq!(observed, max_age);
	}
}
