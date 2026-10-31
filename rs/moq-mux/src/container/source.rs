//! Per-rendition export source that normalizes frame shape and exposes the
//! resolved codec configuration record.
//!
//! Exporters declare what wire shape they want their frames in (currently:
//! length-prefixed NAL units for H.264/H.265) and call [`ExportSource::poll_read`]
//! to pull normalized frames. For Annex-B sources (catalog codec marked
//! `inline: true` / `in_band: true`, empty `description`) the source attaches
//! an [`Avc1`] / [`Hvc1`] transform that strips parameter sets into an avcC or
//! hvcC and length-prefixes the rest; frames wait until that record exists. fMP4
//! instead attaches an [`InBand`] transform, keeping the sets in the samples, when
//! the catalog codec string and dimensions already determine the record.
//!
//! `description()` returns the resolved codec config: the catalog description,
//! a record derived from the codec string, or an avcC/hvcC synthesized from
//! in-band parameter sets.

use std::ops::Bound;
use std::task::{Poll, ready};

use std::time::Duration;

use bytes::Bytes;
use hang::catalog::{AudioConfig, Container, Reference, VideoCodec, VideoConfig};

use super::consumer::{Event, Item};
use crate::catalog::hang::Container as HangContainer;
use crate::codec::annexb::{InBand, InBandCodec};
use crate::codec::h264::Avc1;
use crate::codec::h265::Hvc1;
use crate::container::{Consumer, Frame};

/// Per-track video transform that bridges between codec shapes.
pub(crate) enum VideoTransform {
	Avc1(Avc1),
	Hvc1(Hvc1),
	/// Parameter sets stay in the samples, under a record derived from the catalog.
	InBand(InBand),
}

impl VideoTransform {
	pub(crate) fn codec_private(&self) -> Option<&Bytes> {
		match self {
			VideoTransform::Avc1(t) => t.avcc(),
			VideoTransform::Hvc1(t) => t.hvcc(),
			VideoTransform::InBand(_) => None,
		}
	}

	pub(crate) fn transform(&mut self, payload: Bytes, keyframe: bool) -> crate::Result<Option<Bytes>> {
		match self {
			VideoTransform::Avc1(t) => Ok(t.transform(payload)?),
			VideoTransform::Hvc1(t) => Ok(t.transform(payload)?),
			VideoTransform::InBand(t) => Ok(t.transform(payload, keyframe)?),
		}
	}
}

/// A subscription that resolves on first poll, then the live consumer.
enum SourceState {
	/// Waiting for the target broadcast (the catalog broadcast, or a cross-broadcast
	/// reference) to resolve; the track (by name) is subscribed once it does.
	Requesting(crate::Binding, String),
	/// Waiting for the subscription to resolve (blocks on the publisher's SUBSCRIBE_OK).
	Subscribing(kio::Pending<moq_net::track::Subscribing>),
	/// The resolved consumer, reading frames. Boxed because it's much larger than
	/// the `Subscribing` variant (clippy `large_enum_variant`).
	Active(Box<Consumer<HangContainer>>),
	/// The sibling instance it read went, and its rendition with it.
	Ended,
}

/// The track a source reads: its broadcast's path, the publisher instance that broadcast must
/// be, and the track's name there.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
	path: moq_net::Path,
	epoch: Option<moq_net::Epoch>,
	track: String,
}

impl Target {
	/// Where rendition `name` reads, or `None` when its reference escapes the root.
	fn of(source: &crate::Source, name: &str, config: &impl Reference) -> Option<Self> {
		Some(Self {
			path: source.resolve_reference(config.broadcast())?,
			epoch: config.epoch().cloned(),
			track: config.track(name).to_string(),
		})
	}

	fn request(&self, source: &crate::Source) -> SourceState {
		let binding = source.request_path(&self.path, self.epoch.as_ref());
		SourceState::Requesting(binding, self.track.clone())
	}
}

/// Where a rendition's source stopped reading ([`ExportSource::resume`]).
pub(crate) struct Resume {
	target: Target,
	/// The group of the last frame delivered.
	group: Option<u64>,
	/// The latest presentation time delivered.
	latest: Option<Duration>,
}

/// What a source continuing another rendition's track skips: media up to what that track delivered.
struct Splice {
	/// The latest presentation time the previous track delivered.
	read: Duration,
	/// The group of the first frame read, the only one that may overlap `read`.
	group: Option<u64>,
}

/// A per-rendition source that normalizes frame shape (Annex-B →
/// length-prefixed for H.264/H.265) and exposes the resolved codec config
/// record alongside the frame stream.
pub(crate) struct ExportSource {
	/// The rendition's ID in the catalog.
	name: String,
	source: crate::Source,
	priority_control: Option<crate::source::Priority>,
	target: Target,
	state: SourceState,
	/// Wire format, consumed when the subscription resolves into a consumer.
	media: Option<HangContainer>,
	max_delay: std::time::Duration,
	/// Start at the newest group, rather than the oldest the max delay still reaches.
	live: bool,
	/// Read only groups after this one, so a resubscription skips groups already read.
	after: Option<u64>,
	transform: Option<VideoTransform>,
	/// Resolved codec configuration record (avcC / hvcC / AudioSpecificConfig /
	/// OpusHead). Some once the codec config is available — from the catalog
	/// `description`, or synthesized by the transform.
	description: Option<Bytes>,
	/// Video codec used to derive geometry from its configuration or keyframes.
	video_codec: Option<VideoCodec>,
	/// Geometry resolved from the initial catalog or codec data received afterward.
	video_dimensions: Option<(u32, u32)>,
	/// The transform absorbed a keyframe that carried no picture (parameter sets sent
	/// as their own frame), so the next frame it emits is the sync point.
	absorbed_keyframe: bool,
	skips: u64,
	markers: u64,
	/// Whether a frame of the group being read went out and the group has not ended since.
	open: bool,
	cuts: u64,
	// A stopped encoder may still emit final packets in this group on the old timeline.
	ends: Option<(u64, u64)>,
	/// The counters the export source reports for this rendition, as received, before any transform.
	received: std::sync::Arc<crate::stats::Received>,
	/// The latest presentation time delivered, here or by the source this one resumes.
	latest: Option<Duration>,
	/// The group of the last frame delivered.
	delivered: Option<u64>,
	/// Set while continuing another source, until the first sync frame past what it delivered.
	splice: Option<Splice>,
	/// The sibling broadcast the rendition reads, once resolved: its end ends the rendition.
	instance: Option<moq_net::broadcast::Consumer>,
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
		max_delay: std::time::Duration,
	) -> Result<Option<Self>, crate::Error> {
		Self::video(source, name, config, max_delay, build_video_transform(config))
	}

	/// Subscribe to a video rendition for fMP4 export.
	///
	/// Annex-B H.264 and H.265 whose catalog codec string and dimensions determine
	/// the sample entry keep their parameter sets in the samples and expose that
	/// record immediately. Every other source matches [`Self::for_video`].
	pub(crate) fn for_fmp4_video(
		source: &crate::Source,
		name: &str,
		config: &VideoConfig,
		max_delay: std::time::Duration,
	) -> Result<Option<Self>, crate::Error> {
		let record = annexb_catalog_record(config);
		let transform = match (&config.codec, record.is_some()) {
			(VideoCodec::H264(_), true) => Some(VideoTransform::InBand(InBand::new(InBandCodec::H264))),
			(VideoCodec::H265(_), true) => Some(VideoTransform::InBand(InBand::new(InBandCodec::H265))),
			_ => build_video_transform(config),
		};
		let mut source = Self::video(source, name, config, max_delay, transform)?;
		if let (Some(source), Some(record)) = (source.as_mut(), record) {
			source.description = Some(record);
		}
		Ok(source)
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
		max_delay: std::time::Duration,
	) -> Result<Option<Self>, crate::Error> {
		Self::video(source, name, config, max_delay, None)
	}

	fn video(
		source: &crate::Source,
		name: &str,
		config: &VideoConfig,
		max_delay: std::time::Duration,
		transform: Option<VideoTransform>,
	) -> Result<Option<Self>, crate::Error> {
		let media: HangContainer = config.try_into()?;
		let Some(target) = Target::of(source, name, config) else {
			return Ok(None);
		};

		let mut export = Self::new(source, name, target, media, max_delay);
		export.transform = transform;
		export.description = config.description.as_ref().filter(|b| !b.is_empty()).cloned();
		export.video_codec = Some(config.codec.clone());
		export.video_dimensions = catalog_dimensions(config);
		export.resolve_video_dimensions(&[])?;
		Ok(Some(export))
	}

	/// Subscribe to an audio rendition. Audio has no codec-shape transform;
	/// `description` is taken straight from the catalog.
	///
	/// Rejects an escaping `broadcast` reference like [`Self::for_video`].
	pub fn for_audio(
		source: &crate::Source,
		name: &str,
		config: &AudioConfig,
		max_delay: std::time::Duration,
	) -> Result<Option<Self>, crate::Error> {
		let media: HangContainer = config.try_into()?;
		let Some(target) = Target::of(source, name, config) else {
			return Ok(None);
		};

		let mut export = Self::new(source, name, target, media, max_delay);
		export.description = config.description.as_ref().filter(|b| !b.is_empty()).cloned();
		Ok(Some(export))
	}

	/// Subscribe to a verbatim `mpegts` stream rendition (SCTE-35, private PES, ...).
	/// No codec-shape transform and no description: the frames are Legacy-framed
	/// verbatim bytes the muxer writes back out as PES or private sections.
	///
	/// Such a rendition has no catalog `broadcast` field, so it always lives on the
	/// catalog broadcast and can never carry an escaping reference.
	pub fn for_stream(
		source: &crate::Source,
		name: &str,
		max_delay: std::time::Duration,
	) -> Result<Self, crate::Error> {
		let target = Target {
			path: source.path().clone(),
			epoch: None,
			track: name.to_string(),
		};
		let media = HangContainer::Legacy(crate::container::Kind::Data);
		Ok(Self::new(source, name, target, media, max_delay))
	}

	/// A source of rendition `name` reading `target`, with no codec configuration yet.
	fn new(source: &crate::Source, name: &str, target: Target, media: HangContainer, max_delay: Duration) -> Self {
		Self {
			name: name.to_string(),
			source: source.clone(),
			priority_control: None,
			state: target.request(source),
			target,
			media: Some(media),
			max_delay,
			live: false,
			after: None,
			transform: None,
			description: None,
			video_codec: None,
			video_dimensions: None,
			absorbed_keyframe: false,
			skips: 0,
			markers: 0,
			open: false,
			cuts: 0,
			ends: None,
			received: source.received(name),
			latest: None,
			delivered: None,
			splice: None,
			instance: None,
		}
	}

	/// Whether this source reads the track `config` names for rendition `name`. A catalog update
	/// that moves the rendition to another track or broadcast needs a new source.
	pub(crate) fn reads(&self, source: &crate::Source, name: &str, config: &impl Reference) -> bool {
		Target::of(source, name, config).is_some_and(|target| target == self.target)
	}

	/// Carry on from `previous`, the source this rendition read before the catalog moved it to
	/// this source's track, as [`Self::resuming`] does. Its counters run on, the move counting as
	/// a skip that cuts the group `previous` was reading, if any.
	pub(crate) fn continuing(mut self, previous: &ExportSource) -> Self {
		let ends = previous.ends.map_or(0, |(_, count)| count);
		self.skips = previous.skips + ends + 1;
		self.markers = previous.markers + ends;
		self.cuts = previous.cuts + u64::from(previous.open);
		self.resuming(&previous.resume())
	}

	/// Where this source stopped reading, for a later source of the same rendition.
	pub(crate) fn resume(&self) -> Resume {
		Resume {
			target: self.target.clone(),
			group: self.delivered,
			latest: self.latest,
		}
	}

	/// Pick up where an earlier source of this rendition stopped.
	///
	/// On the same track, start after the last group it delivered, so the publisher's cached
	/// group is not replayed. Another track is new content, read from its live edge, and the
	/// rendition never goes back in time: frames are dropped until a sync frame past the latest
	/// one delivered. On a shared clock the first group read may overlap that, but a later group
	/// that still does fails with [`Error::RenditionRewound`](crate::Error::RenditionRewound).
	pub(crate) fn resuming(mut self, resume: &Resume) -> Self {
		if resume.target == self.target {
			self.after = resume.group;
		} else {
			self.live = true;
			self.splice = resume.latest.map(|read| Splice { read, group: None });
		}
		self.latest = resume.latest;
		self
	}

	/// Start at the newest group, the live edge, rather than the oldest one the max delay still
	/// reaches. The subscription carries the max delay from the start: under a zero budget every
	/// hop drops each group a newer one supersedes, and raising it later cannot recover the
	/// groups that follow the edge.
	pub fn live(mut self) -> Self {
		self.live = true;
		self
	}

	/// The group sequence of the last frame read, or None until the subscription resolves.
	/// Meaningful only once a frame was read.
	pub(crate) fn group(&self) -> Option<u64> {
		match &self.state {
			SourceState::Active(consumer) => Some(consumer.group()),
			_ => None,
		}
	}

	/// Gaps and declared ends this source has advanced past.
	pub fn skips(&self) -> u64 {
		self.skips
	}

	/// The resolved codec-config record, if available.
	pub fn description(&self) -> Option<&Bytes> {
		self.description.as_ref()
	}

	/// Refresh a catalog-derived record after the catalog entry changes.
	///
	/// Returns false when this source was described from the catalog and the new
	/// entry no longer carries everything that record needs. The previous record
	/// is left in place so the caller can fail the export against it.
	pub(crate) fn note_catalog(&mut self, config: &VideoConfig) -> bool {
		if !matches!(self.transform, Some(VideoTransform::InBand(_))) {
			return true;
		}
		match annexb_catalog_record(config) {
			Some(record) => {
				self.description = Some(record);
				true
			}
			None => false,
		}
	}

	/// Declared ends this source has advanced past.
	pub fn markers(&self) -> u64 {
		self.markers
	}

	/// Gaps that lost the rest of a group this source had begun delivering, rather than whole
	/// groups after one that ended.
	pub fn cuts(&self) -> u64 {
		self.cuts
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
		loop {
			match ready!(self.poll_event(waiter))? {
				Some(Event::Frame { frame, .. }) => return Poll::Ready(Ok(Some(frame))),
				Some(Event::GroupEnd | Event::FrameEnd { .. } | Event::Cut { .. }) => continue,
				None => return Poll::Ready(Ok(None)),
			}
		}
	}

	/// Receive group handles and wait for all input bytes while frame delivery is backpressured.
	pub fn poll_received(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		match &mut self.state {
			SourceState::Active(consumer) => consumer.poll_received(waiter),
			SourceState::Ended => Poll::Ready(()),
			_ => Poll::Pending,
		}
	}

	/// Read normalized media or a clean group boundary.
	pub fn poll_event(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Event>>> {
		loop {
			let event = ready!(self.poll_track(waiter))?;
			match &event {
				Some(Event::Frame { frame, .. }) => {
					if !self.spliced(frame)? {
						continue;
					}
					let at = Duration::from(frame.timestamp);
					self.latest = Some(self.latest.map_or(at, |latest| latest.max(at)));
					self.delivered = self.group();
				}
				// The boundaries of frames the splice dropped.
				Some(_) if self.splice.is_some() => continue,
				_ => {}
			}
			return Poll::Ready(Ok(event));
		}
	}

	/// Whether `frame` goes out, rather than being dropped as media the previous track of a
	/// [continued](Self::continuing) rendition already covered.
	fn spliced(&mut self, frame: &Frame) -> crate::Result<bool> {
		let group = self.group();
		let sync = frame.keyframe || self.video_codec.is_none();
		let Some(splice) = self.splice.as_mut() else {
			return Ok(true);
		};
		let at = Duration::from(frame.timestamp);
		if sync && at > splice.read {
			self.splice = None;
			return Ok(true);
		}
		let first = *splice.group.get_or_insert_with(|| group.unwrap_or_default());
		if frame.keyframe && group != Some(first) {
			return Err(crate::Error::RenditionRewound {
				rendition: self.name.clone(),
				start: at,
				read: splice.read,
			});
		}
		Ok(false)
	}

	/// Read the track's normalized media or a clean group boundary.
	fn poll_track(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Event>>> {
		if matches!(self.state, SourceState::Ended) {
			return Poll::Ready(Ok(None));
		}

		// Resolve a cross-broadcast reference into a broadcast before subscribing.
		if matches!(self.state, SourceState::Requesting(..)) {
			let (broadcast, name, sibling) = {
				let SourceState::Requesting(pending, name) = &self.state else {
					unreachable!("just matched Requesting");
				};
				(ready!(pending.poll_broadcast(waiter))?, name.clone(), pending.sibling())
			};
			if sibling {
				self.instance = Some(broadcast.clone());
			}
			let mut subscription = moq_net::track::Subscription::default().with_max_delay(self.max_delay);
			if let Some(after) = self.after {
				subscription = subscription.with_range((Bound::Excluded(after), Bound::Unbounded));
			}
			let track = match broadcast.track(&name) {
				Ok(track) => track,
				Err(_) if self.instance_ended(waiter) => return self.end(),
				Err(err) => return Poll::Ready(Err(err.into())),
			};
			self.state = SourceState::Subscribing(track.subscribe(self.source.subscription(subscription)));
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
					Poll::Ready(Err(_)) | Poll::Pending if self.instance_ended(waiter) => return self.end(),
					Poll::Ready(Err(err)) => return Poll::Ready(Err(err.into())),
					Poll::Pending => return Poll::Pending,
				}
			};
			let media = self
				.media
				.take()
				.expect("media present until the subscription resolves");
			self.priority_control = Some(self.source.control(track.control())?);
			let consumer = Consumer::new(track, media);
			let consumer = match self.live {
				true => consumer.live(),
				false => consumer,
			};
			self.state = SourceState::Active(Box::new(consumer));
		}

		loop {
			// Scope the consumer borrow to the poll so `self.transform` /
			// `self.refresh_description` can borrow `self` afterwards.
			let (frame, arrived) = {
				let SourceState::Active(consumer) = &mut self.state else {
					unreachable!("subscription resolved into an Active consumer");
				};
				let item = match consumer.poll_item(waiter) {
					Poll::Ready(Ok(item)) => item,
					// Only an orderly shutdown of the instance is its end: malformed media or a
					// failed transport still fails the export. The field, not `instance_ended`, so
					// the borrow stays off the consumer.
					Poll::Ready(Err(crate::Error::Moq(
						moq_net::Error::Cancel
						| moq_net::Error::Closed
						| moq_net::Error::GoingAway
						| moq_net::Error::Stream(moq_net::StreamError::Session(moq_net::SessionError::Cancel))
						| moq_net::Error::Session(moq_net::SessionError::Cancel),
					)))
					| Poll::Pending
						if self
							.instance
							.as_ref()
							.is_some_and(|instance| instance.poll_closed(waiter).is_ready()) =>
					{
						return self.end();
					}
					Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
					Poll::Pending => return Poll::Pending,
				};
				match item {
					Some(Item::Frame { group, frame, arrived }) => {
						if self.ends.is_some_and(|(end_group, _)| end_group != group) {
							self.finish_ends();
						}
						self.open = true;
						self.received.frame(frame.timestamp, frame.payload.len());
						(frame, arrived)
					}
					Some(Item::Gap { timestamp, duration }) => {
						// Say what the export never receives and where.
						tracing::warn!(
							track = consumer.name(),
							at = ?std::time::Duration::from(timestamp),
							skipped = ?std::time::Duration::from(duration),
							"media skipped upstream; resuming at the next group"
						);
						self.finish_ends();
						self.skips += 1;
						self.cuts += u64::from(std::mem::take(&mut self.open));
						return Poll::Ready(Ok(Some(Event::GroupEnd)));
					}
					Some(Item::End { group, end }) => {
						if self.ends.is_some_and(|(end_group, _)| end_group != group) {
							self.finish_ends();
						}
						self.ends.get_or_insert((group, 0)).1 += 1;
						return Poll::Ready(Ok(Some(Event::FrameEnd { end, opens: true })));
					}
					Some(Item::FrameEnd(end)) => return Poll::Ready(Ok(Some(Event::FrameEnd { end, opens: false }))),
					Some(Item::GroupEnd) => {
						self.finish_ends();
						self.open = false;
						return Poll::Ready(Ok(Some(Event::GroupEnd)));
					}
					// The skip is counted with the gap the next group reads.
					Some(Item::Cut { observed }) => {
						self.finish_ends();
						self.cuts += u64::from(std::mem::take(&mut self.open));
						return Poll::Ready(Ok(Some(Event::Cut { observed })));
					}
					None => {
						self.priority_control = None;
						self.finish_ends();
						return Poll::Ready(Ok(None));
					}
				}
			};

			let Some(transform) = self.transform.as_mut() else {
				self.resolve_video_dimensions(&frame.payload)?;
				return Poll::Ready(Ok(Some(Event::Frame { frame, arrived })));
			};

			// Spent on the next emitted frame even when that frame is a keyframe itself.
			let keyframe = std::mem::take(&mut self.absorbed_keyframe) | frame.keyframe;
			match transform.transform(frame.payload.clone(), keyframe)? {
				None => {
					// Parameter set absorbed by the transform. Refresh the
					// resolved description (it may have just become available)
					// and pull the next frame.
					self.absorbed_keyframe = keyframe;
					self.refresh_description();
					self.resolve_video_dimensions(&frame.payload)?;
					continue;
				}
				Some(payload) => {
					self.refresh_description();
					self.resolve_video_dimensions(&payload)?;
					let frame = Frame {
						payload,
						keyframe,
						..frame
					};
					return Poll::Ready(Ok(Some(Event::Frame { frame, arrived })));
				}
			}
		}
	}

	/// Whether the sibling broadcast this rendition reads has stopped serving, registering
	/// `waiter` for when it does. Its track stalling or failing then is that instance's end,
	/// which ends the rendition rather than the export once what it delivered is read; following
	/// the instance that replaces it is the export's own call.
	fn instance_ended(&self, waiter: &kio::Waiter) -> bool {
		self.instance
			.as_ref()
			.is_some_and(|instance| instance.poll_closed(waiter).is_ready())
	}

	/// Release a replaced instance's subscription while keeping its resolved metadata.
	pub(crate) fn stop(&mut self) {
		self.finish_ends();
		self.state = SourceState::Ended;
		self.priority_control = None;
		self.instance = None;
	}

	/// End the rendition with its instance.
	fn end(&mut self) -> Poll<crate::Result<Option<Event>>> {
		self.finish_ends();
		self.state = SourceState::Ended;
		self.priority_control = None;
		Poll::Ready(Ok(None))
	}

	fn finish_ends(&mut self) {
		if let Some((_, count)) = self.ends.take() {
			self.skips += count;
			self.markers += count;
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

/// An avcC or hvcC derived from the catalog codec string, when that string and
/// the catalog dimensions are everything the fMP4 sample entry needs.
///
/// CMAF keeps the init segment it was declared with. A catalog that already
/// carries a description, or that omits dimensions, stays on the path that
/// waits for parameter sets in the bitstream.
fn annexb_catalog_record(config: &VideoConfig) -> Option<Bytes> {
	if !matches!(config.container, Container::Legacy | Container::Loc) {
		return None;
	}
	if config
		.description
		.as_ref()
		.is_some_and(|description| !description.is_empty())
	{
		return None;
	}
	catalog_dimensions(config)?;
	match &config.codec {
		VideoCodec::H264(h264) => crate::codec::h264::catalog_avcc(h264),
		VideoCodec::H265(h265) => crate::codec::h265::catalog_hvcc(h265),
		_ => None,
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

#[cfg(test)]
mod tests {
	use hang::catalog::{AudioCodec, Container, H264};
	use moq_net::path::Relative;

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
		config.broadcast = broadcast.map(|rel| Relative::new(rel).unwrap());
		config
	}

	fn audio(broadcast: Option<&str>) -> AudioConfig {
		let mut config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		config.container = Container::Legacy;
		config.broadcast = broadcast.map(|rel| Relative::new(rel).unwrap());
		config
	}

	/// A rendition whose `broadcast` escapes the root is skipped, rather than resolving
	/// against whatever broadcast the clamped path lands on. The test origin serves every
	/// path it is asked for, so a clamped request would happily resolve.
	#[tokio::test]
	async fn escaping_reference_skips_the_rendition() {
		let live = Live::avc3();
		let source = live.source().await;
		let max_delay = std::time::Duration::ZERO;

		let escaping = |result: Result<Option<ExportSource>, crate::Error>, what: &str| match result {
			Ok(None) => {}
			Err(err) => panic!("{what} failed instead of being skipped: {err:?}"),
			Ok(Some(_)) => panic!("{what} should not resolve"),
		};

		// A reference resolves against the catalog's parent, and the source is rooted at a
		// single-segment path, so its parent is the root and any `..` walks above it.
		for reference in ["..", "../source", "../../elsewhere"] {
			let config = video(Some(reference));
			escaping(ExportSource::for_video(&source, "video", &config, max_delay), reference);
			escaping(
				ExportSource::for_video_raw(&source, "video", &config, max_delay),
				reference,
			);
			escaping(
				ExportSource::for_audio(&source, "audio", &audio(Some(reference)), max_delay),
				reference,
			);
		}
	}

	/// A legal reference still resolves, as does an absent or empty one (the catalog's own
	/// broadcast). A lone `.` names the parent, which here is the root.
	#[tokio::test]
	async fn legal_reference_keeps_the_rendition() {
		let live = Live::avc3();
		let source = live.source().await;
		let max_delay = std::time::Duration::ZERO;

		for reference in [None, Some(""), Some("./source"), Some("sub"), Some(".")] {
			ExportSource::for_video(&source, "video", &video(reference), max_delay)
				.unwrap_or_else(|err| panic!("{reference:?} should keep the rendition: {err:?}"))
				.unwrap_or_else(|| panic!("{reference:?} should keep the rendition"));
			ExportSource::for_audio(&source, "audio", &audio(reference), max_delay)
				.unwrap_or_else(|err| panic!("{reference:?} should keep the rendition: {err:?}"))
				.unwrap_or_else(|| panic!("{reference:?} should keep the rendition"));
		}
	}

	/// A sibling rendition's first announcement starts it, a replacement is never read, and its
	/// instance going ends the rendition instead of failing the export, both mid-stream and
	/// before the subscription resolves.
	#[tokio::test(start_paused = true)]
	async fn a_sibling_rendition_starts_late_and_ends_with_its_instance() {
		use std::time::Duration;

		use crate::container::test_util::raw_frame;

		let origin = crate::source::produce_origin();
		let _catalog = origin.publish("a/pub", Default::default()).unwrap();
		tokio::task::yield_now().await;
		let source = crate::source::resolved(&origin, "a/pub").await;
		let config = audio(Some("./source"));
		let mut export = ExportSource::for_audio(&source, "audio", &config, Duration::from_secs(10))
			.unwrap()
			.unwrap();
		let mut read = async || {
			tokio::time::timeout(Duration::from_millis(100), kio::wait(|waiter| export.poll_read(waiter)))
				.await
				.ok()
				.map(|frame| frame.unwrap().map(|frame| frame.timestamp.as_micros()))
		};
		let wire = || HangContainer::Legacy(crate::container::Kind::Audio);
		let info = || hang::container::track_info(hang::catalog::PRIORITY.audio);

		assert_eq!(read().await, None, "nothing announces the sibling yet");
		let old = origin.publish("a/source", Default::default()).unwrap();
		let mut track = crate::container::Producer::new(old.create_track("audio", info()).unwrap(), wire());
		track.write(raw_frame(0, b"old", true)).unwrap();
		assert_eq!(read().await, Some(Some(0)), "the late announcement starts it");

		let new = origin.publish("a/source", Default::default()).unwrap();
		let mut replacement = crate::container::Producer::new(new.create_track("audio", info()).unwrap(), wire());
		replacement.write(raw_frame(1_000_000, b"new", true)).unwrap();
		track.write(raw_frame(24_000, b"old", true)).unwrap();
		assert_eq!(read().await, Some(Some(24_000)), "the replacement is never read");

		drop(old);
		assert_eq!(read().await, Some(None), "its end ends the rendition");

		// A rendition still subscribing when its instance goes ends too.
		let mut late = ExportSource::for_audio(&source, "late", &config, Duration::from_secs(10))
			.unwrap()
			.unwrap();
		let ended = tokio::time::timeout(Duration::from_secs(1), kio::wait(|waiter| late.poll_read(waiter))).await;
		assert!(
			matches!(ended, Ok(Ok(None))),
			"{:?}",
			ended.map(|read| read.map(|_| ()))
		);
		drop((track, replacement, new));
	}

	/// A sibling's failure still fails the export when its instance goes, whether its media is
	/// malformed, its track is aborted with a protocol error, or its producers went away
	/// unfinished: only an orderly shutdown of the instance is the rendition's end.
	#[tokio::test(start_paused = true)]
	async fn a_sibling_failure_outlives_its_instance() {
		use std::time::Duration;

		for failure in ["malformed", "violation", "dropped"] {
			let origin = crate::source::produce_origin();
			let _catalog = origin.publish("a/pub", Default::default()).unwrap();
			tokio::task::yield_now().await;
			let source = crate::source::resolved(&origin, "a/pub").await;
			let config = audio(Some("./source"));
			let mut export = ExportSource::for_audio(&source, "audio", &config, Duration::from_secs(10))
				.unwrap()
				.unwrap();

			let sibling = origin.publish("a/source", Default::default()).unwrap();
			let track = sibling
				.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
				.unwrap();
			// Subscribe before anything is written, so the failure and the instance's end arrive
			// together.
			let settle = tokio::time::timeout(Duration::from_millis(100), kio::wait(|waiter| export.poll_read(waiter)));
			assert!(settle.await.is_err(), "nothing is written yet");
			let mut group = track.append_group().unwrap();
			let held = match failure {
				"malformed" => {
					// An empty Legacy audio frame is malformed.
					group
						.write_frame(moq_net::Timestamp::from_micros(0).unwrap(), bytes::Bytes::new())
						.unwrap();
					Some((group, track))
				}
				"violation" => {
					track.clone().abort(moq_net::Error::ProtocolViolation).unwrap();
					Some((group, track))
				}
				_ => {
					drop((group, track));
					None
				}
			};
			drop(sibling);
			tokio::time::sleep(Duration::from_millis(10)).await;
			assert!(export.instance_ended(&kio::Waiter::noop()), "the instance has gone");

			let read = tokio::time::timeout(Duration::from_secs(1), kio::wait(|waiter| export.poll_read(waiter)))
				.await
				.expect("the read settles");
			assert!(
				read.is_err(),
				"{failure}: {:?}",
				read.map(|frame| frame.map(|frame| frame.timestamp))
			);
			drop(held);
		}
	}

	/// A sibling served over a session ends the rendition cleanly when its publisher goes, which
	/// the session reports as its own cancellation.
	#[tokio::test(start_paused = true)]
	async fn a_remote_sibling_ends_with_its_instance() {
		use std::time::Duration;

		use crate::container::test_util::{Link, raw_frame};

		let remote = crate::source::produce_origin();
		let origin = crate::source::produce_origin();
		let _catalog = origin.publish("a/pub", Default::default()).unwrap();
		tokio::task::yield_now().await;
		let source = crate::source::resolved(&origin, "a/pub").await;
		let _link = Link::new(&remote, &origin).await;
		let sibling = remote.publish("a/source", Default::default()).unwrap();
		let mut track = crate::container::Producer::new(
			sibling
				.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
				.unwrap(),
			HangContainer::Legacy(crate::container::Kind::Audio),
		);
		track.write(raw_frame(0, b"x", true)).unwrap();
		let mut export = ExportSource::for_audio(&source, "audio", &audio(Some("./source")), Duration::from_secs(10))
			.unwrap()
			.unwrap();
		let mut read = async || {
			tokio::time::timeout(Duration::from_secs(1), kio::wait(|waiter| export.poll_read(waiter)))
				.await
				.expect("the read settles")
				.unwrap()
				.map(|frame| frame.timestamp.as_micros())
		};
		assert_eq!(read().await, Some(0));

		drop(sibling);
		assert_eq!(read().await, None, "its end ends the rendition");
		drop(track);
	}

	/// The requested budget must be present on the first SUBSCRIBE, a live start's too. Updating
	/// it after SUBSCRIBE_OK cannot recover backlog the publisher already skipped, nor the groups a
	/// zero budget drops as superseded before the update lands.
	#[tokio::test]
	async fn latency_is_sent_with_the_initial_subscription() {
		for live_start in [false, true] {
			let live = Live::avc3();
			let max_delay = std::time::Duration::from_secs(10);
			let export = ExportSource::for_video(&live.source().await, live.track.name(), &video(None), max_delay)
				.unwrap()
				.expect("fixture should produce a video rendition");
			let mut export = match live_start {
				true => export.live(),
				false => export,
			};

			let observed = kio::wait(|waiter| {
				let _ = export.poll_read(waiter);
				match live.track.subscription() {
					Some(subscription) => Poll::Ready(subscription.max_delay),
					None => Poll::Pending,
				}
			})
			.await;

			assert_eq!(observed, Some(max_delay), "live start: {live_start}");
		}
	}

	/// A live start reads from the newest group, though the budget delivers older ones too.
	#[tokio::test(start_paused = true)]
	async fn a_live_start_reads_from_the_newest_group() {
		use crate::container::test_util::raw_frame;

		let mut live = Live::audio(audio(None));
		for timestamp in [0, 24_000, 48_000] {
			live.track.write(raw_frame(timestamp, b"audio", true)).unwrap();
		}
		let mut export = ExportSource::for_audio(
			&live.source().await,
			live.track.name(),
			&audio(None),
			std::time::Duration::from_secs(10),
		)
		.unwrap()
		.unwrap()
		.live();

		let frame = kio::wait(|waiter| export.poll_read(waiter)).await.unwrap().unwrap();
		assert_eq!(frame.timestamp.as_micros(), 48_000);
		live.track.write(raw_frame(72_000, b"audio", true)).unwrap();
		let frame = kio::wait(|waiter| export.poll_read(waiter)).await.unwrap().unwrap();
		assert_eq!(frame.timestamp.as_micros(), 72_000);
		assert_eq!(export.skips(), 0, "nothing below the start counts as skipped");
	}
	/// A live source picks the newest group without narrowing what the publisher sends: a
	/// narrower budget on the wire, widened after the first read, lets the publisher skip the
	/// groups of a burst written before the widening reaches it, a gap no update recovers.
	#[tokio::test(start_paused = true)]
	async fn a_live_start_loses_no_group_the_publisher_bursts() {
		use std::time::Duration;

		use crate::container::Container as _;
		use crate::container::test_util::{Link, origin, raw_frame};

		let (publisher, subscriber) = (origin(1), origin(2));
		let _link = Link::new(&publisher, &subscriber).await;

		let broadcast = publisher.create_broadcast("room").unwrap();
		let track = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		broadcast.announce(Default::default()).unwrap();
		let consumer = subscriber.consume();
		let remote = consumer.follow("room", None).unwrap().available().await.unwrap();
		let source = crate::Source::new(consumer, remote);
		let mut export = ExportSource::for_audio(&source, "audio", &audio(None), Duration::from_secs(1))
			.unwrap()
			.unwrap()
			.live();

		// One 24 ms frame per group, as the TS import publishes MPEG-1 audio.
		let wire = HangContainer::Legacy(crate::container::Kind::Audio);
		let write = |sequence: u64| {
			let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
			wire.write(&mut group, &[raw_frame(sequence * 24_000, b"audio", true)])
				.unwrap();
			group.finish().unwrap();
		};
		let mut read = async || {
			tokio::time::timeout(Duration::from_millis(100), kio::wait(|waiter| export.poll_read(waiter)))
				.await
				.ok()
				.map(|frame| frame.unwrap().map(|frame| frame.timestamp.as_micros()))
		};

		assert_eq!(read().await, None, "subscribed, with nothing published yet");
		write(0);
		tokio::time::sleep(Duration::from_millis(10)).await;
		// A PES carrying several frames publishes them in one pass, before the publisher's
		// session runs again, and the export reads the first group in the meantime.
		for sequence in 1..4 {
			write(sequence);
		}
		let mut timestamps = Vec::new();
		while let Some(Some(timestamp)) = read().await {
			timestamps.push(timestamp);
		}
		for sequence in 4..8 {
			write(sequence);
		}
		track.finish().unwrap();
		while let Some(Some(timestamp)) = read().await {
			timestamps.push(timestamp);
		}

		let expected: Vec<u128> = (0..8).map(|sequence| sequence * 24_000).collect();
		assert_eq!(timestamps, expected, "the burst reached the export whole");
		assert_eq!(export.skips(), 0);
	}

	/// A live start begins at the publisher's newest group, even when an older group of the
	/// backlog the budget serves arrives first. Starting at that older group reads it, then
	/// skips the groups after it once the next PES moves the live edge a budget past them: a
	/// gap right after the first frame. Behind a relay holding only that first group, the
	/// relay passes on the newest group its source named.
	#[tokio::test(start_paused = true)]
	async fn a_live_start_behind_a_backlog_starts_at_the_newest_group() {
		use std::time::Duration;

		use crate::container::Container as _;
		use crate::container::test_util::{Link, origin, raw_frame};

		for relay in [false, true] {
			let (publisher, subscriber) = (origin(1), origin(3));
			// SETUP takes the first uni stream; the second is the backlog's oldest group.
			let (held, _downstream) = match relay {
				true => {
					let middle = origin(2);
					let held = Link::held(&publisher, &middle, 2).await;
					(held, Some(Link::new(&middle, &subscriber).await))
				}
				false => (Link::held(&publisher, &subscriber, 2).await, None),
			};

			let broadcast = publisher.create_broadcast("room").unwrap();
			let track = broadcast
				.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
				.unwrap();
			broadcast.announce(Default::default()).unwrap();
			let consumer = subscriber.consume();
			let remote = consumer.follow("room", None).unwrap().available().await.unwrap();
			let source = crate::Source::new(consumer, remote);

			// MPEG-1 audio from TS: one 24 ms frame per group, a PES of five written at once.
			let wire = HangContainer::Legacy(crate::container::Kind::Audio);
			let pes = |pes: u64| {
				for sequence in pes * 5..pes * 5 + 5 {
					let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
					wire.write(&mut group, &[raw_frame(sequence * 24_000, b"audio", true)])
						.unwrap();
					group.finish().unwrap();
				}
			};
			// About two seconds of it, so the budget's backlog reaches back about 450 ms.
			for n in 0..17 {
				pes(n);
				tokio::time::sleep(Duration::from_millis(120)).await;
			}

			let mut export = ExportSource::for_audio(&source, "audio", &audio(None), Duration::from_millis(450))
				.unwrap()
				.unwrap()
				.live();
			let mut read = async || {
				tokio::time::timeout(Duration::from_millis(100), kio::wait(|waiter| export.poll_read(waiter)))
					.await
					.ok()
					.map(|frame| frame.unwrap().map(|frame| frame.timestamp.as_micros()))
			};
			let mut timestamps = Vec::new();
			// Only the backlog's oldest group has landed.
			if let Some(Some(timestamp)) = read().await {
				timestamps.push(timestamp);
			}
			// The next PES moves the live edge on before the rest of the backlog lands.
			pes(17);
			tokio::time::sleep(Duration::from_millis(120)).await;
			pes(18);
			held.open();
			while let Some(Some(timestamp)) = read().await {
				timestamps.push(timestamp);
			}
			track.finish().unwrap();
			while let Some(Some(timestamp)) = read().await {
				timestamps.push(timestamp);
			}

			// Group 84 was the newest when the export joined.
			let expected: Vec<u128> = (84..95).map(|sequence| sequence * 24_000).collect();
			assert_eq!(timestamps, expected, "relay: {relay}");
			assert_eq!(export.skips(), 0, "relay: {relay}");
		}
	}

	/// A source continuing a rendition moved off a track that delivered up to `read` starts at
	/// the first keyframe past it. Only the first group read may overlap: a later group still
	/// behind it is a track that goes back in time.
	#[tokio::test(start_paused = true)]
	async fn a_continued_source_starts_past_what_its_previous_track_delivered() {
		use crate::container::test_util::{Moving, SPS, marked_frame};

		for (read, first) in [(900, Some(1_000_000)), (1_500, None)] {
			let mut moving = Moving::new();
			let source = moving.source().await;
			let max_delay = Duration::from_secs(10);
			let mut previous =
				ExportSource::for_video(&source, Moving::RENDITION, &Moving::config("a", None, SPS), max_delay)
					.unwrap()
					.unwrap();
			previous.skips = 2;
			previous.latest = Some(Duration::from_millis(read));
			let mut next =
				ExportSource::for_video(&source, Moving::RENDITION, &Moving::config("b", None, SPS), max_delay)
					.unwrap()
					.unwrap()
					.continuing(&previous);
			let mut poll = async || {
				tokio::time::timeout(Duration::from_millis(100), kio::wait(|waiter| next.poll_read(waiter))).await
			};

			// A keyframe at zero then deltas up to 967 ms, past 900 ms but no place to start.
			for i in 0..30 {
				moving.b.write(marked_frame(i, 0xb1)).unwrap();
			}
			assert!(poll().await.is_err(), "the first group overlaps: read {read}");
			for i in 30..60 {
				moving.b.write(marked_frame(i, 0xb1)).unwrap();
			}
			match (poll().await.unwrap(), first) {
				(Ok(Some(frame)), Some(first)) => assert_eq!(frame.timestamp.as_micros(), first),
				(
					Err(crate::Error::RenditionRewound {
						rendition,
						start,
						read: behind,
					}),
					None,
				) => {
					assert_eq!(rendition, Moving::RENDITION);
					assert_eq!((start, behind), (Duration::from_secs(1), Duration::from_millis(read)));
				}
				(result, _) => panic!("read {read}: {result:?}"),
			}
			assert_eq!(next.skips(), 3, "the move counts as a skip");
		}
	}

	async fn audio_export() -> (moq_net::track::Producer, ExportSource) {
		use std::time::Duration;
		let live = Live::audio(audio(None));
		let mut export = ExportSource::for_audio(
			&live.source().await,
			live.track.name(),
			&audio(None),
			Duration::from_secs(10),
		)
		.unwrap()
		.unwrap();
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let consumer = Consumer::new(
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10))),
			HangContainer::Legacy(crate::container::Kind::Audio),
		);
		export.state = SourceState::Active(Box::new(consumer));
		(track, export)
	}

	/// A gap that loses the rest of a group already begun is a cut; whole groups missing after
	/// one that ended are not.
	#[tokio::test(start_paused = true)]
	async fn only_a_gap_inside_a_group_cuts_it() {
		use crate::container::Container as _;
		use crate::container::test_util::raw_frame;

		let (track, mut export) = audio_export().await;
		let wire = HangContainer::Legacy(crate::container::Kind::Audio);
		let group = |sequence: u64, timestamp: u64| {
			let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
			wire.write(&mut group, &[raw_frame(timestamp, b"audio", true)]).unwrap();
			group
		};
		// Group 1 is missing whole; group 3 is cut short after its first frame.
		group(0, 0).finish().unwrap();
		group(2, 40_000).finish().unwrap();
		let cut = group(3, 60_000);
		group(4, 80_000).finish().unwrap();
		track.finish().unwrap();

		let waiter = kio::Waiter::noop();
		let read = |export: &mut ExportSource| {
			let Poll::Ready(Ok(Some(frame))) = export.poll_read(&waiter) else {
				panic!("frame ready")
			};
			(frame.timestamp.as_micros(), export.skips(), export.cuts())
		};
		assert_eq!(read(&mut export), (0, 0, 0));
		assert_eq!(read(&mut export), (40_000, 1, 0), "group 1 is missing whole");
		assert_eq!(read(&mut export), (60_000, 1, 0));
		cut.abort(moq_net::Error::Old).unwrap();
		assert_eq!(read(&mut export), (80_000, 2, 1), "group 3 was cut short");
	}

	#[tokio::test(start_paused = true)]
	async fn a_gap_then_end_reaches_the_exporter_in_order() {
		use crate::container::Container as _;
		use crate::container::test_util::raw_frame;

		let (track, mut export) = audio_export().await;
		let wire = HangContainer::Legacy(crate::container::Kind::Audio);
		for (sequence, timestamp, payload) in [(0, 0, b"audio".as_slice()), (2, 1_000_000, b"".as_slice())] {
			let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
			wire.write(&mut group, &[raw_frame(timestamp, payload, true)]).unwrap();
			group.finish().unwrap();
		}
		track.finish().unwrap();
		let waiter = kio::Waiter::noop();
		assert!(matches!(
			export.poll_event(&waiter),
			Poll::Ready(Ok(Some(Event::Frame { .. })))
		));
		assert_eq!((export.skips(), export.markers()), (0, 0));
		assert!(matches!(
			export.poll_event(&waiter),
			Poll::Ready(Ok(Some(Event::GroupEnd)))
		));
		assert_eq!((export.skips(), export.markers()), (0, 0));
		assert!(matches!(
			export.poll_event(&waiter),
			Poll::Ready(Ok(Some(Event::GroupEnd)))
		));
		assert_eq!((export.skips(), export.markers()), (1, 0), "the gap precedes the end");
		assert!(matches!(
			export.poll_event(&waiter),
			Poll::Ready(Ok(Some(Event::FrameEnd { opens: true, .. })))
		));
		assert_eq!((export.skips(), export.markers()), (1, 0));
		assert!(matches!(
			export.poll_event(&waiter),
			Poll::Ready(Ok(Some(Event::GroupEnd)))
		));
		assert_eq!(
			(export.skips(), export.markers()),
			(2, 1),
			"the end is crossed without a successor frame"
		);
	}
	#[tokio::test(start_paused = true)]
	async fn final_audio_packets_keep_the_stopped_encoders_timeline() {
		use crate::container::Container as _;
		use crate::container::test_util::raw_frame;

		let (track, mut export) = audio_export().await;
		let wire = HangContainer::Legacy(crate::container::Kind::Audio);
		for (sequence, frames) in [
			(0, vec![raw_frame(0, b"audio", true)]),
			(
				1,
				vec![raw_frame(50_000, b"", true), raw_frame(40_000, b"drain", false)],
			),
			(2, vec![raw_frame(100_000, b"resumed", true)]),
		] {
			let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
			wire.write(&mut group, &frames).unwrap();
			group.finish().unwrap();
		}
		track.finish().unwrap();
		let waiter = kio::Waiter::noop();
		for (timestamp, generation) in [(0, 0), (40_000, 0), (100_000, 1)] {
			let Poll::Ready(Ok(Some(frame))) = export.poll_read(&waiter) else {
				panic!("frame ready")
			};
			assert_eq!(frame.timestamp.as_micros(), timestamp);
			assert_eq!(
				(export.skips(), export.markers()),
				(generation, generation),
				"only the next group starts a new timeline"
			);
		}
	}
}
