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
	Requesting(kio::Pending<moq_net::origin::Requested>, String),
	/// Waiting for the subscription to resolve (blocks on the publisher's SUBSCRIBE_OK).
	Subscribing(kio::Pending<moq_net::track::Subscribe>),
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
}

impl ExportSource {
	/// Subscribe to a video rendition and build an `ExportSource`.
	pub fn for_video(
		source: &crate::Source,
		name: &str,
		config: &VideoConfig,
		latency: Duration,
	) -> Result<Self, crate::Error> {
		let media: HangContainer = (&config.container).try_into()?;
		let transform = build_video_transform(config);
		let description = config.description.as_ref().filter(|b| !b.is_empty()).cloned();

		Ok(Self {
			state: SourceState::Requesting(source.request(config.broadcast.as_ref()), name.to_string()),
			media: Some(media),
			latency,
			transform,
			description,
		})
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
	) -> Result<Self, crate::Error> {
		let media: HangContainer = (&config.container).try_into()?;
		let description = config.description.as_ref().filter(|b| !b.is_empty()).cloned();

		Ok(Self {
			state: SourceState::Requesting(source.request(config.broadcast.as_ref()), name.to_string()),
			media: Some(media),
			latency,
			transform: None,
			description,
		})
	}

	/// Subscribe to an audio rendition. Audio has no codec-shape transform;
	/// `description` is taken straight from the catalog.
	pub fn for_audio(
		source: &crate::Source,
		name: &str,
		config: &AudioConfig,
		latency: Duration,
	) -> Result<Self, crate::Error> {
		let media: HangContainer = (&config.container).try_into()?;
		let description = config.description.as_ref().filter(|b| !b.is_empty()).cloned();

		Ok(Self {
			state: SourceState::Requesting(source.request(config.broadcast.as_ref()), name.to_string()),
			media: Some(media),
			latency,
			transform: None,
			description,
		})
	}

	/// Subscribe to a verbatim `mpegts` stream rendition (SCTE-35, private PES, ...).
	/// No codec-shape transform and no description: the frames are Legacy-framed
	/// verbatim bytes the muxer writes back out as PES or private sections.
	pub fn for_stream(source: &crate::Source, name: &str, latency: Duration) -> Result<Self, crate::Error> {
		Ok(Self {
			state: SourceState::Requesting(source.request(None), name.to_string()),
			media: Some(HangContainer::Legacy),
			latency,
			transform: None,
			description: None,
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
