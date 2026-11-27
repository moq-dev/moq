use bytes::Bytes;

use super::{Container, Frame, Kind};

fn add_micros(timestamp: moq_net::Timestamp, extra: moq_net::Timestamp) -> Option<moq_net::Timestamp> {
	let micros = timestamp.as_micros().ok()?.saturating_add(extra.as_micros().ok()?);
	let micros = u64::try_from(micros).ok()?;
	moq_net::Timestamp::from_micros(micros).ok()
}

/// Whether `left` is before `right` to the microsecond; false when either is unscaled.
fn timestamp_lt(left: moq_net::Timestamp, right: moq_net::Timestamp) -> bool {
	matches!((left.as_micros(), right.as_micros()), (Ok(left), Ok(right)) if left < right)
}

/// Whether `left` is before `right` at full precision, a tie across scales included as not
/// before; false when either is unscaled.
fn timestamp_before(left: moq_net::Timestamp, right: moq_net::Timestamp) -> bool {
	match (left.scale(), right.scale()) {
		(Some(lhs), Some(rhs)) => {
			(left.value() as u128) * (rhs.as_u64() as u128) < (right.value() as u128) * (lhs.as_u64() as u128)
		}
		_ => false,
	}
}

/// Refuse a frame without a time with [`TimestampMismatch`](moq_net::Error::TimestampMismatch):
/// media is timed, and a scale-less track's [unscaled](moq_net::Timestamp::unscaled) key is not.
fn timed(timestamp: moq_net::Timestamp, duration: Option<moq_net::Timestamp>) -> Result<(), moq_net::Error> {
	match timestamp.scale().is_some() && duration.is_none_or(|duration| duration.scale().is_some()) {
		true => Ok(()),
		false => Err(moq_net::Error::TimestampMismatch),
	}
}

/// A producer for media tracks that manages group boundaries.
///
/// Generic over `C: Container` to support different container encodings
/// (Legacy, CMAF, LOC). Use [`catalog::hang::Container`](crate::catalog::hang::Container)
/// to dispatch on the catalog at runtime.
///
/// ## Group Management
///
/// `keyframe = true` means "start a new group": it closes the previous group (if any) and opens a
/// new one, so every group begins with a keyframe. A non-keyframe extends the current group, and
/// writing one when no group is open is a protocol violation.
///
/// A stream where every frame is independently decodable (e.g. audio) still drives grouping through
/// this bit: mark only the *first* frame of each group a keyframe and the rest non-keyframes, so the
/// caller's [`cut`](Self::cut) / [`seek`](Self::seek) boundaries define the groups instead of every
/// frame opening its own (one QUIC stream per frame). [`needs_keyframe`](Self::needs_keyframe)
/// reports whether the next frame has to be one.
///
/// [`cut`](Self::cut) closes the current group early, ideally saying where its content
/// ends; the next write must be a keyframe. Reach for it when the following keyframe won't
/// supply that boundary in time, or to bound each group of an accumulating audio track.
/// [`discontinuity`](Self::discontinuity) publishes a marker group with no media, for
/// when the timeline is about to jump rather than merely continue. Timestamps never fall
/// below the live edge earlier groups reached.
///
/// ## Buffering
///
/// When the buffer duration is zero (default), each frame is written immediately as its
/// own container frame. When non-zero, frames are buffered and flushed together when:
/// - A keyframe arrives (flushes the previous group's buffer, starts new group),
/// - The buffered duration exceeds the configured duration,
/// - `finish()` is called.
///
/// This is useful for CMAF where multiple samples should be packed into one moof+mdat.
pub struct Producer<C: Container, R = ()> {
	/// The catalog rendition this media producer owns, when it was created through a catalog.
	/// Dropped before the track, so a handler asked for the track again finds the name free.
	rendition: Option<Box<dyn Rendition<R>>>,
	inner: moq_net::track::Producer,
	container: C,
	group: Option<moq_net::group::Producer>,
	buffer: Vec<Frame>,

	buffer_duration: std::time::Duration,

	/// Start to key the next group opened by [`Self::write`] with, instead of its keyframe's
	/// timestamp. Set by [`Self::seek`] and consumed on the next group creation.
	pending_start: Option<u64>,

	/// This track's place in the broadcast's join interval, for a video or audio track a catalog
	/// minted: it reports the longest group measured.
	member: Option<crate::catalog::interval::Member>,

	/// The furthest presentation point written, i.e. `max(timestamp + duration)`: the base for a
	/// duration marker when the caller does not pass a bound.
	end: Option<moq_net::Timestamp>,

	/// Exclusive presentation end of finished groups, where a payload with no timestamp of its
	/// own lands. Not a floor: a group may start below it.
	live_edge: Option<moq_net::Timestamp>,
	/// The latest frame timestamp written, which a discontinuity marker never falls below: a frame
	/// without a duration reaches only its own timestamp, so the live edge can sit right on it.
	newest: Option<moq_net::Timestamp>,
	/// The End of the latest group that ended whole, in the track's timescale. Its span runs to
	/// there, so no later group starts below it, a discontinuity marker included.
	ended: Option<moq_net::Timestamp>,
	/// The source and published timestamps of the keyframe that opened the open group, when its
	/// start moved it past a span. A frame at that source timestamp, such as the picture parameter
	/// sets precede, is published with it.
	moved: Option<(moq_net::Timestamp, moq_net::Timestamp)>,

	/// The source timestamp of the keyframe that opened the latest group. The next group may not
	/// start below it.
	start: Option<moq_net::Timestamp>,
	/// The start of the group before the latest one. A frame below it is a restart, not a splice.
	floor: Option<moq_net::Timestamp>,
	/// The newest frame of every group before the latest one: every frame of the latest group
	/// presents after it.
	ceiling: Option<moq_net::Timestamp>,
	/// A keyframe was dropped with no group open, so the frames decoding from it are dropped too,
	/// until a keyframe opens a group.
	skipping: bool,
	/// The latest keyframe written, whose leading pictures are the frames after it presenting
	/// before it.
	keyframe: Option<moq_net::Timestamp>,
	/// A leading picture of [`keyframe`](Self::keyframe) was dropped, so its later leading pictures
	/// are dropped too: they may reference it, and pictures presenting after the keyframe never
	/// reference a leading picture.
	tainted: bool,

	/// Duration of the frame that last raised [`end`](Self::end), if it had one. Distinguishes
	/// an exclusive presentation point from a max timestamp that still needs an estimate.
	last_duration: Option<moq_net::Timestamp>,

	/// Previous source and published timestamps within the group, and the cadence observed within
	/// this epoch.
	previous_timestamp: Option<(moq_net::Timestamp, moq_net::Timestamp)>,
	cadence: Option<moq_net::Timestamp>,
	/// A presentation endpoint cannot bound the decode-order tail after reordering.
	reordered: bool,

	/// Measures bitrate, reorder, batch, and explicit encoder flush observations for the catalog.
	/// A caller without a published rendition simply never reads the estimate.
	estimator: crate::catalog::Estimator,
	/// Frames at or before this are history caught up behind the live edge; see [`Self::history`].
	history: Option<moq_net::Timestamp>,

	/// Peak-hold claim on the connection allocator, when one was supplied.
	/// Named `bandwidth` so it is not confused with the catalog-gate [`Reserved`].
	bandwidth: Option<crate::catalog::Claim>,

	/// The counters the catalog reports for this track, when it was created through a catalog.
	sent: Option<std::sync::Arc<crate::stats::Sent>>,
}

/// A frame whose payload streams into its track as it arrives, opened by [`Producer::open`].
///
/// It holds the track's open group until [`Producer::commit`] returns it, so nothing else writes to
/// the track meanwhile. Dropping it unfinished aborts that group.
pub(crate) struct Streamed {
	payload: Payload,
	/// The timestamp the source gave, which the producer measures; a group start may have moved
	/// the one written.
	source: moq_net::Timestamp,
	timestamp: moq_net::Timestamp,
	duration: Option<moq_net::Timestamp>,
	keyframe: bool,
	size: usize,
}

/// Where a [`Streamed`] frame's payload goes.
#[expect(
	clippy::large_enum_variant,
	reason = "nearly every frame is written; boxing the writer would allocate for each"
)]
enum Payload {
	Written(moq_net::frame::Producer),
	/// The producer dropped the frame (see [`Producer::write`]); this many bytes are still to come.
	Dropped(usize),
}

impl Streamed {
	/// Append payload bytes, readable by subscribers at once.
	pub fn write(&mut self, chunk: &[u8]) -> crate::Result<()> {
		match &mut self.payload {
			Payload::Written(writer) => writer.write(chunk)?,
			Payload::Dropped(remaining) => {
				*remaining = remaining.checked_sub(chunk.len()).ok_or(moq_net::Error::WrongSize)?;
			}
		}
		Ok(())
	}

	/// Payload bytes still to write.
	pub fn remaining(&self) -> usize {
		match &self.payload {
			Payload::Written(writer) => writer.remaining(),
			Payload::Dropped(remaining) => *remaining,
		}
	}

	/// Abort the frame and its group with `err`.
	pub fn abort(self, err: moq_net::Error) {
		if let Payload::Written(writer) = self.payload {
			let _ = writer.abort(err);
		}
	}
}

impl<R: Clone + Send + 'static> Producer<crate::catalog::hang::Container, R> {
	/// Open a frame of `size` payload bytes, written through the returned [`Streamed`] as they
	/// arrive, for a source that learns a frame's size before its bytes.
	///
	/// Groups and refuses frames the way [`write`](Self::write) does; the frame is measured once
	/// [`commit`](Self::commit)ted. The group's storage takes the bytes as they are written, so a
	/// declared size commits no memory, and one past the group's limit is refused up front. A
	/// buffering producer, or CMAF, whose `moof` describes the payload, cannot stream.
	pub(crate) fn open(
		&mut self,
		timestamp: moq_net::Timestamp,
		keyframe: bool,
		duration: Option<moq_net::Timestamp>,
		size: usize,
	) -> crate::Result<Streamed> {
		if !self.buffer_duration.is_zero() || self.container.prefix(timestamp)?.is_none() {
			return Err(crate::Error::UnsupportedContainer("a streamed frame".into()));
		}
		timed(timestamp, duration)?;
		let source = timestamp;
		let Some(timestamp) = self.prepare(timestamp, keyframe)? else {
			return Ok(Streamed {
				payload: Payload::Dropped(size),
				source,
				timestamp,
				duration,
				keyframe,
				size,
			});
		};
		// At the timestamp the frame is written at, which a group start may have moved.
		let prefix = (self.container.prefix(timestamp)?)
			.ok_or_else(|| crate::Error::UnsupportedContainer("a streamed frame".into()))?;
		let group = self.group.take().expect("prepare opens a group");
		let total = prefix.len().checked_add(size).ok_or(moq_net::Error::FrameTooLarge)?;
		let mut writer = group.create_frame(moq_net::frame::Info {
			timestamp,
			size: total as u64,
		})?;
		writer.write(prefix)?;
		Ok(Streamed {
			payload: Payload::Written(writer),
			source,
			timestamp,
			duration,
			keyframe,
			size,
		})
	}

	/// Finish a frame [`open`](Self::open) returned once its payload is written, handing its group
	/// back to the track.
	pub(crate) fn commit(&mut self, frame: Streamed) -> crate::Result<()> {
		let writer = match frame.payload {
			Payload::Written(writer) => writer,
			Payload::Dropped(0) => return Ok(()),
			Payload::Dropped(_) => return Err(moq_net::Error::WrongSize.into()),
		};
		let group = writer.finish()?;
		self.group = Some(group);
		self.written(
			frame.source,
			frame.timestamp,
			frame.duration,
			frame.size,
			frame.keyframe,
		);
		Ok(())
	}
}

trait Rendition<R>: Send {
	fn name(&self) -> &str;
	fn timestamp(&self, hint: Option<moq_net::Timestamp>) -> crate::Result<moq_net::Timestamp>;
	fn set(&mut self, config: R) -> crate::Result<()>;
	fn config(&self) -> crate::Result<R>;
	fn replace(&mut self, config: R) -> crate::Result<()>;
	fn estimate(&mut self, reading: crate::catalog::Reading) -> crate::Result<()>;
	fn supply_bitrate(&mut self, bitrate: u64) -> crate::Result<()>;
	fn fresh(
		&self,
		container: crate::catalog::hang::Container,
		info: moq_net::track::Info,
		interval: Option<std::time::Duration>,
	) -> crate::Result<Producer<crate::catalog::hang::Container, R>>;
}

impl<E, R> Rendition<R> for crate::catalog::tracks::Rendition<E, R>
where
	E: crate::catalog::hang::CatalogExt,
	R: crate::catalog::RenditionConfig<E>,
{
	fn name(&self) -> &str {
		self.name()
	}

	fn fresh(
		&self,
		container: crate::catalog::hang::Container,
		info: moq_net::track::Info,
		interval: Option<std::time::Duration>,
	) -> crate::Result<Producer<crate::catalog::hang::Container, R>> {
		self.fresh(container, info, interval)
	}

	fn timestamp(&self, hint: Option<moq_net::Timestamp>) -> crate::Result<moq_net::Timestamp> {
		self.timestamp(hint)
	}

	fn set(&mut self, config: R) -> crate::Result<()> {
		self.set(config)
	}

	fn config(&self) -> crate::Result<R> {
		self.config()
	}

	fn replace(&mut self, config: R) -> crate::Result<()> {
		self.replace(config)
	}

	fn estimate(&mut self, reading: crate::catalog::Reading) -> crate::Result<()> {
		self.estimate(reading)
	}

	fn supply_bitrate(&mut self, bitrate: u64) -> crate::Result<()> {
		self.supply_bitrate(bitrate)
	}
}

impl<C: Container> Producer<C> {
	/// Create a Producer wrapping the given moq-lite producer, muxing into `container`.
	///
	/// A plain media track by default: no buffering, no timeline. Add buffering with
	/// [`with_buffer`](Self::with_buffer); the join interval is wired by the catalog (see
	/// [`catalog::Producer::video`](crate::catalog::Producer::video) and its sibling APIs).
	pub fn new(track: moq_net::track::Producer, container: C) -> Self {
		Self {
			inner: track,
			container,
			group: None,
			buffer: Vec::new(),
			buffer_duration: std::time::Duration::ZERO,
			pending_start: None,
			member: None,
			end: None,
			live_edge: None,
			newest: None,
			ended: None,
			moved: None,
			start: None,
			floor: None,
			ceiling: None,
			skipping: false,
			keyframe: None,
			tainted: false,
			last_duration: None,
			previous_timestamp: None,
			cadence: None,
			reordered: false,
			estimator: crate::catalog::Estimator::new(),
			history: None,
			bandwidth: None,
			rendition: None,
			sent: None,
		}
	}
}

impl<C: Container, R: Clone + Send + 'static> Producer<C, R>
where
	crate::Error: From<C::Error>,
{
	pub(crate) fn with_rendition<E>(
		track: moq_net::track::Producer,
		container: C,
		rendition: crate::catalog::tracks::Rendition<E, R>,
	) -> Self
	where
		E: crate::catalog::hang::CatalogExt,
		R: crate::catalog::RenditionConfig<E>,
	{
		Self {
			inner: track,
			container,
			group: None,
			buffer: Vec::new(),
			buffer_duration: std::time::Duration::ZERO,
			pending_start: None,
			member: None,
			end: None,
			live_edge: None,
			newest: None,
			ended: None,
			moved: None,
			start: None,
			floor: None,
			ceiling: None,
			skipping: false,
			keyframe: None,
			tainted: false,
			last_duration: None,
			previous_timestamp: None,
			cadence: None,
			reordered: false,
			estimator: rendition.estimator(),
			history: None,
			bandwidth: None,
			rendition: Some(Box::new(rendition)),
			sent: None,
		}
	}

	/// Publish or replace this track's catalog config.
	///
	/// Unset jitter and delay keep their published floor; explicitly lowering either is refused.
	pub fn set(&mut self, config: R) -> crate::Result<()> {
		self.rendition.as_mut().ok_or(crate::Error::NotPublished)?.set(config)
	}

	/// Advertise `bitrate` as the rendition's maximum, such as an encoder's retuned target, which
	/// later measurement no longer replaces. Every other published field stays as it is.
	pub fn set_bitrate(&mut self, bitrate: u64) -> crate::Result<()> {
		self.rendition
			.as_mut()
			.ok_or(crate::Error::NotPublished)?
			.supply_bitrate(bitrate)
	}

	/// Modify the published catalog config.
	///
	/// Estimate fields the config left to detection at [`set`](Self::set) stay owned by detection:
	/// an edit to them here is published but replaced by the next measurement, except that jitter
	/// and delay never drop below the published value. Call `set` with the field filled in to pin it.
	pub fn modify(&mut self) -> crate::Result<Guard<'_, R>> {
		let rendition = self.rendition.as_mut().ok_or(crate::Error::NotPublished)?;
		let config = rendition.config()?;
		Ok(Guard {
			rendition: rendition.as_mut(),
			config: Some(config),
		})
	}

	/// Resolve a timestamp on the broadcast's shared clock: `hint`, or the clock's reading now.
	///
	/// A reading comes from the clock rather than media, so one landing on the newest group's start,
	/// as a burst of untimed frames can, is moved one unit of the track's timescale past it: group
	/// starts strictly increase.
	pub fn timestamp(&self, hint: Option<moq_net::Timestamp>) -> crate::Result<moq_net::Timestamp> {
		let rendition = self.rendition.as_ref().ok_or(crate::Error::NotPublished)?;
		match hint {
			Some(pts) => rendition.timestamp(Some(pts)),
			None => Ok(crate::start::next(&self.inner, rendition.timestamp(None)?)?.1),
		}
	}

	fn publish_estimate(&mut self) -> crate::Result<()> {
		let reading = self.estimator.reading();
		if let Some(member) = self.member.as_mut() {
			member.report(reading.interval);
		}
		if let Some(rendition) = self.rendition.as_mut() {
			rendition.estimate(reading)?;
		}
		Ok(())
	}

	/// The catalog key owned by this producer.
	pub fn name(&self) -> &str {
		self.rendition
			.as_ref()
			.map_or(self.inner.name(), |rendition| rendition.name())
	}

	/// A watch-only handle to this track's subscriber demand.
	pub fn demand(&self) -> moq_net::track::Demand {
		self.inner.demand()
	}
}

impl Producer<crate::catalog::hang::Container, hang::catalog::VideoConfig> {
	/// Publish a decoder change under a fresh raw track name.
	pub(crate) fn reconfigure(&mut self, config: hang::catalog::VideoConfig) -> crate::Result<()> {
		let rendition = self.rendition.as_ref().ok_or(crate::Error::NotPublished)?;
		let previous = match rendition.config() {
			Ok(previous) => previous,
			Err(crate::Error::NotPublished) => return self.set(config),
			Err(err) => return Err(err),
		};
		if previous.same_identity(&config) {
			return self.set(config);
		}
		let container = crate::catalog::hang::Container::try_from(&config)?;
		let interval = self.member.as_ref().and_then(|member| member.interval());
		let mut next = rendition.fresh(container, self.inner.info().clone(), interval)?;
		next.buffer_duration = self.buffer_duration;
		// A seek keys the next group, which the successor opens.
		next.pending_start = self.pending_start;
		next.set(config)?;
		self.end_with(None)?;
		*self = next;
		Ok(())
	}
}

/// A published rendition config edited in place.
pub struct Guard<'a, R> {
	rendition: &'a mut dyn Rendition<R>,
	config: Option<R>,
}

impl<R> Guard<'_, R> {
	/// Publish the edited config and return any error.
	pub fn commit(mut self) -> crate::Result<()> {
		let config = self.config.take().expect("config is present until commit");
		self.rendition.replace(config)
	}
}

impl<R> std::ops::Deref for Guard<'_, R> {
	type Target = R;

	fn deref(&self) -> &Self::Target {
		self.config.as_ref().expect("config is present until commit")
	}
}

impl<R> std::ops::DerefMut for Guard<'_, R> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		self.config.as_mut().expect("config is present until commit")
	}
}

impl<R> Drop for Guard<'_, R> {
	fn drop(&mut self) {
		let Some(config) = self.config.take() else {
			return;
		};
		if let Err(err) = self.rendition.replace(config) {
			tracing::error!(%err, "failed to publish modified rendition");
		}
	}
}

impl<C: Container, R: Clone + Send + 'static> Producer<C, R>
where
	crate::Error: From<C::Error>,
{
	#[cfg(test)]
	fn bandwidth_ceiling(&self) -> Option<moq_net::bandwidth::Rate> {
		self.bandwidth.as_ref().and_then(|claim| claim.ceiling())
	}

	/// The jitter, bitrate, delay, and framerate measured from the frames written so far.
	///
	/// A catalog-owned producer publishes this automatically.
	pub fn estimate(&self) -> crate::catalog::Estimate {
		self.estimator.estimate()
	}

	/// Record a frame's reorder delay (`PTS - DTS`), raising the measured jitter to the decode
	/// buffer a B-frame stream needs.
	///
	/// Frames carry no decode time, so a caller that demuxed one (a container importer) supplies it.
	pub fn reorder(&mut self, delay: moq_net::Timestamp) {
		self.estimator.reorder(delay);
	}

	/// Mark frames at or before `until` as retained history being caught up, rather than live
	/// frames written late.
	///
	/// History never measures [`delay`](crate::catalog::Estimate::delay) against the other tracks:
	/// a publisher replaying a backlog behind the live edge would otherwise advertise the backlog's
	/// age as playout delay for the rest of the track.
	pub fn history(&mut self, until: moq_net::Timestamp) {
		self.history = Some(until);
	}

	/// Record the media duration emitted together by a container importer.
	pub(crate) fn burst(&mut self, duration: std::time::Duration) -> crate::Result<()> {
		self.estimator.burst(duration);
		self.publish_estimate()
	}

	/// Whether the next [`write`](Self::write) has to be a keyframe, i.e. no group is currently open
	/// (at the start, or after a [`cut`](Self::cut) / [`seek`](Self::seek)).
	///
	/// A stream where every frame is independently decodable (audio) uses this to mark only the first
	/// frame of each group a keyframe: `write(Frame { keyframe: producer.needs_keyframe(), .. })`, so
	/// grouping follows the caller's `cut`/`seek` boundaries rather than opening a group per frame.
	pub fn needs_keyframe(&self) -> bool {
		self.group.is_none()
	}

	/// One past the track's largest group start, even an empty group's: the lowest start a group
	/// after what the track holds can take.
	pub(crate) fn next_start(&self) -> u64 {
		self.inner.largest().map_or(0, |largest| largest.group + 1)
	}

	/// Buffer up to `duration` of frames into each container frame.
	///
	/// When non-zero, frames are buffered and flushed together once the buffered duration exceeds
	/// it, or a keyframe arrives, packing multiple samples into one container frame (e.g. a CMAF
	/// moof+mdat). Zero (the default) flushes each frame immediately.
	///
	/// This is the publisher-side counterpart to the consumer's max delay, and the one
	/// knob here that genuinely *adds* delay. A batch's oldest frame reaches the transport
	/// as late as the batch's span, so the catalog advertises that span as `jitter`.
	pub fn with_buffer(mut self, duration: std::time::Duration) -> Self {
		self.buffer_duration = duration;
		self
	}

	/// Report the longest group measured through `member` into the broadcast's join interval.
	pub(crate) fn with_member(mut self, member: Option<crate::catalog::interval::Member>) -> Self {
		self.member = member;
		self
	}

	/// Claim this track's peak-hold catalog bitrate on `allocator`.
	///
	/// A passthrough track has no configured ceiling, so it reserves the measured
	/// maximum instead: nothing until the first 1 s window closes, then only
	/// upward. A co-resident encoder targets what is left. The claim is named
	/// `bandwidth` so it is not confused with the catalog-gate `Reserved`.
	pub fn with_bandwidth(mut self, allocator: moq_net::bandwidth::Allocator) -> Self {
		self.bandwidth = Some(crate::catalog::Claim::new(allocator));
		self
	}

	/// Count every frame this track writes into `sent`, which its catalog reports.
	pub(crate) fn with_sent(mut self, sent: std::sync::Arc<crate::stats::Sent>) -> Self {
		self.sent = Some(sent);
		self
	}

	/// Report `frames` frames the source produced but never handed to this track, such as ones an
	/// encoder fell behind on, in its catalog's [`stats`](crate::catalog::Producer::stats).
	pub fn report_skipped(&self, frames: u64) {
		if let Some(sent) = &self.sent {
			sent.skip(frames);
		}
	}

	/// Report the encoder's latest target bitrate in its catalog's
	/// [`stats`](crate::catalog::Producer::stats). The catalog's `bitrate` stays the maximum.
	pub fn report_bitrate(&self, bitrate: moq_net::bandwidth::Rate) {
		if let Some(sent) = &self.sent {
			sent.target(bitrate);
		}
	}

	/// Count a written frame. Only video keyframes count as keyframes: elsewhere the bit only
	/// marks where a group starts.
	fn count(&self, bytes: usize, keyframe: bool) {
		if let Some(sent) = &self.sent {
			let keyframe = keyframe && self.container.kind() == Kind::Video;
			sent.add(1, bytes, keyframe.into());
		}
	}

	/// Raise the standing claim when the catalog estimate has a new peak.
	fn claim(&mut self) {
		let Some(bandwidth) = self.bandwidth.as_mut() else {
			return;
		};
		bandwidth.update(&self.inner.demand(), self.estimator.estimate().bitrate);
	}

	/// The underlying moq-lite track producer. Read-only; mutating it directly
	/// would sidestep group/keyframe invariants.
	pub fn track(&self) -> &moq_net::track::Producer {
		&self.inner
	}

	/// The exclusive presentation end earlier groups have reached, if any.
	///
	/// A source with no timestamp of its own (a section, a PES without PTS) lands on it. It is
	/// not a floor: a keyframe may start below it (see [`write`](Self::write)).
	pub fn live_edge(&self) -> Option<moq_net::Timestamp> {
		self.live_edge
	}

	/// Write a frame to the track.
	///
	/// A keyframe closes any open group and starts a new one. A non-keyframe extends the current
	/// group; if no group is open it returns [`MissingKeyframe`](super::MissingKeyframe), so a caller
	/// joining mid-stream can skip frames until the first keyframe. A source where every frame is
	/// independently decodable (audio) marks only the first frame of each group a keyframe (see
	/// [`needs_keyframe`](Self::needs_keyframe)) so the group spans more than one frame.
	///
	/// A group starts at its keyframe's timestamp, in the track's timescale, and every frame of a
	/// group presents after every frame of the groups before it. Frames within a group may still
	/// reorder (B-frames, open-GOP leading pictures). A keyframe that doesn't start after the newest
	/// frame written continues the open group, as at a splice whose keyframe dips below the old
	/// content's last frames. A later frame presenting at or before the previous groups' newest,
	/// such as a leading picture after a splice, is dropped and counted as
	/// [skipped](Self::report_skipped), and so is a keyframe at or before the newest frame with no
	/// group open, such as after a [`cut`](Self::cut), with the frames after it until a keyframe
	/// opens a group.
	///
	/// Group starts strictly increase, and no group starts inside the span of one that ended whole
	/// at a [`cut`](Self::cut)'s end. So with no group open, a keyframe past the newest frame but at
	/// the latest group's start, such as media resuming right at a
	/// [discontinuity](Self::discontinuity)'s marker, or below that end, such as one a cadence or a
	/// rounded duration estimated past it, is stamped at the first start past both. A frame at that
	/// keyframe's own timestamp, such as the picture its parameter sets precede, is stamped with
	/// it, and the group's End is still estimated from the source's timestamps, so a move never
	/// adds up across groups.
	///
	/// Group starts never go backwards: a keyframe below the start of the latest group, or any
	/// frame below the start of the group before its own, returns
	/// [`TimestampRewind`](super::TimestampRewind) with the refused timestamp and that start,
	/// without writing, the way an oversized frame is refused. That is a restart, which is a new
	/// broadcast.
	pub fn write(&mut self, mut frame: Frame) -> crate::Result<()> {
		timed(frame.timestamp, frame.duration)?;
		let source = frame.timestamp;
		let Some(timestamp) = self.prepare(frame.timestamp, frame.keyframe)? else {
			return Ok(());
		};
		frame.timestamp = timestamp;

		// Buffer or write the frame.
		if self.buffer_duration.is_zero() {
			let group = self.group.as_mut().unwrap();
			let (duration, bytes) = (frame.duration, frame.payload.len());
			let keyframe = frame.keyframe;
			self.container.write(group, &[frame])?;
			// Only what the container accepted is measured. A rejected frame (too large for the
			// group, a timestamp that won't convert) leaves the producer usable, and the estimate's
			// extrema never fall, so counting one would inflate the catalog for good.
			self.written(source, timestamp, duration, bytes, keyframe);
		} else {
			// Buffered frames count toward the rates on the way in instead. The flush that eventually
			// writes them takes the track down with it when it fails, so there is nothing to unwind.
			// Their place in the write order waits for that flush, which is when the transport has them.
			self.estimator.rate(source, frame.payload.len());
			self.observe_end(source, timestamp, frame.duration);
			self.buffer.push(frame);

			// Flush if the buffered span has reached the buffer duration.
			if self.buffered_span() >= self.buffer_duration {
				self.flush_buffer(None)?;
			}
		}

		Ok(())
	}

	/// Check `timestamp` against the groups before it and open a group for a keyframe, returning
	/// the timestamp to write the frame at, or `None` to drop it.
	fn prepare(&mut self, timestamp: moq_net::Timestamp, keyframe: bool) -> crate::Result<Option<moq_net::Timestamp>> {
		// A frame sharing the moved keyframe's source timestamp goes with it, as the picture after
		// its parameter sets does.
		let source = timestamp;
		let timestamp = match self.moved {
			Some((moved, at)) if self.group.is_some() && timestamp == moved => at,
			_ => timestamp,
		};
		let floor = match keyframe {
			true => self.start,
			false => self.floor,
		};
		if let Some(floor) = floor
			&& timestamp_lt(timestamp, floor)
		{
			return Err(super::TimestampRewind { timestamp, floor }.into());
		}
		// A seek's start is checked on the track the keyframe lands on, before anything can drop
		// it, so a seek below the latest group's start refuses this keyframe rather than a later
		// one. A seek has no group open, so cutting it changes nothing this depends on.
		let sought = match (keyframe, self.pending_start) {
			(true, Some(start)) => Some((start, self.sought(start)?)),
			_ => None,
		};

		// Every frame of a group presents after every frame of the groups before it, so a keyframe
		// that doesn't start after the newest frame written continues the open group: a splice whose
		// keyframe dips below the old content's last frames, as a TS content join does, publishes
		// one longer group. With no group open, as after bytes were lost, it has nothing to continue
		// and is dropped like the frames that follow it, until a keyframe past the newest frame.
		if keyframe
			&& let Some(newest) = self.newest
			&& !timestamp_lt(newest, timestamp)
		{
			return match self.group {
				Some(_) => Ok(self.continue_keyframe(timestamp)),
				None => {
					self.skipping = true;
					Ok(self.drop_frame(timestamp))
				}
			};
		}

		// A keyframe opens a group keyed by its timestamp, which has to start after every group the
		// track holds. With one open, a keyframe that doesn't, such as parameter sets and the picture
		// they precede written as two keyframes at one timestamp, continues it instead. With none
		// open, nor may it start inside the span of the group a cut ended whole, which runs to its
		// End: one at the latest group's start, such as media resuming right where a break's marker
		// sits, or below that End, as after an End estimated from a cadence or a rounded duration,
		// is stamped at the first start past both. One below the latest group's start is refused,
		// before anything changes.
		let mut timestamp = timestamp;
		// A start moved past a span takes a keyframe below it along, as a keyframe the track's
		// spans hold is moved, so it never presents below its own group's start.
		if let Some((start, sought)) = sought
			&& sought != start
			&& let Some(scale) = self.inner.info().timescale
			&& crate::start::key(&self.inner, timestamp)? < sought
		{
			timestamp = moq_net::Timestamp::new(sought, scale).map_err(|_| moq_net::Error::TimestampMismatch)?;
		}
		let sought = sought.map(|(_, sought)| sought);
		let start = match (keyframe && self.pending_start.is_none(), &self.group) {
			(false, _) => None,
			(true, Some(_)) => match crate::start::opens(&self.inner, timestamp)? {
				Some(start) => Some(start),
				None => return Ok(self.continue_keyframe(timestamp)),
			},
			(true, None) => {
				let (start, at) = crate::start::resume(&self.inner, timestamp, self.ended)?;
				if at != timestamp {
					tracing::debug!(?timestamp, ?at, "a keyframe starts past the group before it");
				}
				timestamp = at;
				Some(start)
			}
		};

		// A keyframe cuts the previous group, its timestamp the boundary where that group's content
		// ends.
		if keyframe {
			self.cut(Some(timestamp))?;
		}

		// Start a new group if needed; the first frame of a group must be a keyframe.
		if self.group.is_none() {
			if !keyframe {
				if self.skipping {
					return Ok(self.drop_frame(timestamp));
				}
				// No group yet and this delta can't anchor one. The caller (e.g. a
				// mid-stream join) decides whether to skip until the first keyframe.
				return Err(super::MissingKeyframe.into());
			}
			let group = match (self.pending_start.take().and(sought), start) {
				(Some(start), _) => self.inner.create_group(moq_net::group::Info { start })?,
				(None, Some(start)) => {
					// A track's first group says nothing came before it, so a reader can tell it
					// is the first: a decoder starting there needs no warmup.
					if self.inner.largest().is_none() {
						self.inner.skip_groups(0..start);
					}
					self.inner.append_group(timestamp)?
				}
				(None, None) => unreachable!("a keyframe without a pending start has one"),
			};

			self.floor = self.start;
			self.ceiling = self.newest;
			self.skipping = false;
			// Measured from the source, so a moved start never adds up across groups.
			self.start = Some(source);
			self.moved = (timestamp != source).then_some((source, timestamp));
			self.group = Some(group);
			self.keyframe = Some(timestamp);
			self.tainted = false;
		} else if self.ceiling.is_some_and(|ceiling| !timestamp_lt(ceiling, timestamp)) {
			// A frame presenting with the groups before its own, such as a leading picture after a
			// splice, is dropped: it references pictures from before the splice, or shows new
			// content while the old still plays.
			self.tainted = true;
			return Ok(self.drop_frame(timestamp));
		} else if self.tainted && self.keyframe.is_some_and(|keyframe| timestamp_lt(timestamp, keyframe)) {
			// A later leading picture of the same keyframe may reference the dropped one.
			return Ok(self.drop_frame(timestamp));
		}

		Ok(Some(timestamp))
	}

	/// Note a keyframe that continues the open group, whose leading pictures are judged afresh.
	fn continue_keyframe(&mut self, timestamp: moq_net::Timestamp) -> Option<moq_net::Timestamp> {
		self.keyframe = Some(timestamp);
		self.tainted = false;
		Some(timestamp)
	}

	/// Count a frame that presents with the groups before it as skipped rather than writing it,
	/// returning `None` for [`prepare`](Self::prepare) to report.
	fn drop_frame(&self, timestamp: moq_net::Timestamp) -> Option<moq_net::Timestamp> {
		tracing::debug!(
			?timestamp,
			"dropping a frame that presents before an earlier group ends"
		);
		self.report_skipped(1);
		None
	}

	/// Measure and report a frame the group accepted unbuffered.
	fn written(
		&mut self,
		source: moq_net::Timestamp,
		at: moq_net::Timestamp,
		duration: Option<moq_net::Timestamp>,
		bytes: usize,
		keyframe: bool,
	) {
		self.estimator.rate(source, bytes);
		self.order(source);
		self.count(bytes, keyframe);
		self.observe_end(source, at, duration);
	}

	/// Place a written frame in the write order, unless it is history; see [`Self::history`].
	fn order(&mut self, timestamp: moq_net::Timestamp) {
		if self.history.is_none_or(|until| timestamp_lt(until, timestamp)) {
			self.estimator.order(timestamp);
		}
	}

	/// Cut the current group, flushing buffered frames and closing it.
	///
	/// `end` bounds the final buffered frame when the publisher knows where the
	/// group's content stops. A video track that writes duration markers appends an
	/// empty frame at that bound, or at the last timestamp plus its estimated
	/// duration. Reordered groups omit this marker because their presentation end
	/// does not bound the last frame in decode order. An unreordered media group, or one given an
	/// explicit bound, ends whole there, so the next group starts at or after it (see
	/// [`write`](Self::write)). The next write must be a keyframe. An explicit bound
	/// below a frame the group holds, in the track's timescale, returns
	/// [`InvalidEnd`](super::InvalidEnd) without flushing or closing the group.
	pub fn cut(&mut self, end: Option<moq_net::Timestamp>) -> crate::Result<()> {
		let marker_at = end.or_else(|| self.estimated_end());
		self.close(end, marker_at, None)
	}

	/// Close the current group, ending a video track's group with a duration marker at `marker_at`.
	///
	/// The group ends whole at `whole`, else at a bound `end` the caller knows, such as the next
	/// keyframe, else where a media track's duration marker goes, so a reader can tell the group
	/// starting there continues it.
	fn close(
		&mut self,
		end: Option<moq_net::Timestamp>,
		marker_at: Option<moq_net::Timestamp>,
		whole: Option<moq_net::Timestamp>,
	) -> crate::Result<()> {
		// A group never ends below a frame of its own, compared in the track's timescale as
		// `finish_at` checks it.
		if self.group.is_some()
			&& let Some((end, newest)) = end.zip(self.newest)
			&& let (Ok(end), Ok(newest)) = (
				crate::start::key(&self.inner, end),
				crate::start::key(&self.inner, newest),
			) && end < newest
		{
			return Err(super::InvalidEnd.into());
		}

		// Before the flush, which can fail: an unbounded cut leaves the measurement open to fold
		// into the next group anyway, so cutting often costs the catalog nothing.
		self.estimator.cut(end);
		self.claim();

		// The group's own state ends with it, whether or not closing it succeeds.
		let reordered = std::mem::take(&mut self.reordered);
		// Where this group's content stops: the duration marker when we write one, else the
		// caller's bound, else the furthest point we wrote.
		let reached = match (marker_at.or(end), self.end.take()) {
			(Some(bound), Some(written)) => Some(super::later(written, bound)),
			(bound, written) => bound.or(written),
		};
		self.last_duration = None;
		self.previous_timestamp = None;

		let tail_end = marker_at.filter(|_| !reordered);
		// Media ends whole where its last frame does, as the duration marker says. A data track's
		// items have no cadence to estimate that from.
		let estimated = tail_end.filter(|_| self.container.kind() != Kind::Data);
		if let Err(err) = self.close_group(whole.or(end).or(estimated), tail_end, reached) {
			// A group that fails to close fails with the cause, rather than staying open for the next
			// write to continue.
			self.buffer.clear();
			if let Some(group) = self.group.take() {
				let _ = group.abort(err.reason());
			}
			return Err(err);
		}
		self.publish_estimate()
	}

	/// Flush the buffer into the open group and finish it, raising the live edge to `reached`. With
	/// a `bound`, the group ends whole there rather than only closing.
	fn close_group(
		&mut self,
		bound: Option<moq_net::Timestamp>,
		tail_end: Option<moq_net::Timestamp>,
		reached: Option<moq_net::Timestamp>,
	) -> crate::Result<()> {
		self.flush_buffer(tail_end)?;
		if let Some(group) = self.group.as_mut() {
			self.container.finish_group(group, tail_end)?;
		}
		if let Some(group) = self.group.take() {
			match bound.and_then(|bound| crate::start::end(&self.inner, group.start, bound)) {
				Some(end) => {
					group.finish_at(end.value())?;
					self.ended = Some(end);
				}
				None => {
					group.finish()?;
				}
			}
			// Only a closed group reaches anything; a cut with none open names a point nothing reached.
			if let Some(end) = reached {
				self.raise_live_edge(end);
			}
		}
		Ok(())
	}

	fn raise_live_edge(&mut self, timestamp: moq_net::Timestamp) {
		if self.live_edge.is_none_or(|edge| !timestamp_lt(timestamp, edge)) {
			self.live_edge = Some(timestamp);
		}
	}

	/// Raise the furthest presentation point written, for [`cut`](Self::cut) to report, from the
	/// `source` timestamp of a frame written `at`. Estimates follow the source, so a keyframe a group
	/// start moved never pushes the next estimated End, and the move after it, further.
	fn observe_end(
		&mut self,
		timestamp: moq_net::Timestamp,
		at: moq_net::Timestamp,
		duration: Option<moq_net::Timestamp>,
	) {
		// Either order stepping back is a reorder: a frame after a keyframe a group start moved can
		// publish below it, so the source's order alone misses it.
		self.reordered |= self
			.previous_timestamp
			.is_some_and(|(previous, published)| timestamp < previous || timestamp_before(at, published));
		if let Some((previous, _)) = self.previous_timestamp
			&& let Ok(delta) = timestamp.checked_sub(previous)
			&& !delta.is_zero()
		{
			self.cadence = Some(delta);
		}
		self.previous_timestamp = Some((timestamp, at));
		// At full precision, so the group's floor is its highest frame as `finish_at` sees it.
		self.newest = Some(self.newest.map_or(at, |newest| super::later(newest, at)));

		// Timestamp and duration can be at different scales, so add them in micros; the
		// sub-microsecond rounding that costs is far below a segment boundary.
		let Ok(micros) = timestamp.as_micros() else { return };
		let Ok(extra) = duration.map_or(Ok(0), |duration| duration.as_micros()) else {
			return;
		};
		let Ok(micros) = u64::try_from(micros + extra) else {
			return;
		};
		let Ok(end) = moq_net::Timestamp::from_micros(micros) else {
			return;
		};
		if duration.is_some() {
			self.estimator.reach(end);
		}
		if self.end.is_none_or(|prev| end >= prev) {
			self.end = Some(end);
			self.last_duration = duration.filter(|duration| !duration.is_zero());
		}
	}

	/// Exclusive group end from a known duration or the observed sample cadence, never below the
	/// newest frame, which a sum rounded to the microsecond can fall under on a finer track.
	fn estimated_end(&self) -> Option<moq_net::Timestamp> {
		let last = self.end?;
		let end = match self.last_duration {
			Some(_) => last,
			None => add_micros(last, self.cadence?)?,
		};
		Some(self.newest.map_or(end, |newest| super::later(end, newest)))
	}

	/// Close the current group (if any) and open the next group at the given start.
	///
	/// The next [`write`](Self::write) must be a keyframe and will land in a group keyed `start`,
	/// in the track's timescale, rather than at its own timestamp. The groups after it are keyed
	/// by their keyframes again, which have to start after it. Useful for joining mid-stream.
	///
	/// The closing group ends whole where its last frame does, as [`cut(None)`](Self::cut)
	/// estimates it, but not past `start`, which no group's span may hold. A `start` at or below a
	/// frame the track holds, or inside the span of a group that ended whole, keys the group at the
	/// first start past both instead, as [`write`](Self::write) moves a keyframe, and a keyframe
	/// below that start is stamped at it, with a frame at its timestamp. One that doesn't pass the
	/// latest group's start refuses the next keyframe with
	/// [`TimestampRewind`](super::TimestampRewind). A decoder change takes `start` to a fresh
	/// track, which holds nothing.
	pub fn seek(&mut self, start: u64) -> crate::Result<()> {
		let mut end = self.estimated_end();
		if let (Some(estimate), Some(newest), Some(scale)) = (end, self.newest, self.inner.info().timescale)
			&& crate::start::key(&self.inner, estimate)? > start
			&& crate::start::key(&self.inner, newest)? < start
		{
			end = Some(moq_net::Timestamp::new(start, scale).map_err(|_| moq_net::Error::TimestampMismatch)?);
		}
		self.close(None, end, None)?;
		self.pending_start = Some(start);
		Ok(())
	}

	/// Where a group a [`seek`](Self::seek) keyed `start` starts: `start`, or the first start past
	/// every frame the track holds and the span of the group that ended whole last, as a keyframe
	/// is moved. Refused with [`TimestampRewind`](super::TimestampRewind) when `start` doesn't pass
	/// the latest group's start.
	fn sought(&self, start: u64) -> crate::Result<u64> {
		let Some(scale) = self.inner.info().timescale else {
			return Ok(start);
		};
		let next = self.next_start();
		if start < next {
			let at = |key: u64| moq_net::Timestamp::new(key, scale).map_err(|_| moq_net::Error::TimestampMismatch);
			return Err(super::TimestampRewind {
				timestamp: at(start)?,
				floor: at(next)?,
			}
			.into());
		}
		let key = |timestamp: Option<moq_net::Timestamp>| {
			timestamp
				.map(|timestamp| crate::start::key(&self.inner, timestamp))
				.transpose()
		};
		let past_newest = key(self.newest)?.map_or(0, |newest| newest.saturating_add(1));
		Ok(start.max(past_newest).max(key(self.ended)?.unwrap_or(0)))
	}

	/// Publish a marker group standing for a break in the timeline: content stopped, and
	/// whatever comes next does not continue it.
	///
	/// Call this whenever the timeline is about to jump -- pausing an encoder, switching
	/// source, resuming on a re-anchored clock. Without it a break is invisible: the next
	/// group looks exactly like the one that would have followed, and a consumer bounding a
	/// sample by the next group's first frame hands it the entire gap as its duration. That
	/// produced a 2405 second video sample out of a publisher that had been paused 40 minutes.
	/// Group starts alone can't rule a pause out, so this
	/// marker is the only thing that can say one happened.
	///
	/// It also fixes what a subscriber joining mid-break sees. A subscription starts at the
	/// track's latest group, and creating this one advances that -- so a late joiner lands on
	/// the marker and waits for real media, instead of being served the group from *before*
	/// the break as though it were live.
	///
	/// Audio and video write one empty frame one unit of the track's timescale before the exclusive
	/// end of the previous epoch, so what resumes at that end starts after it, but never below the
	/// latest frame written: a frame without a duration ends where it starts, so what resumes then
	/// has to start after that frame. Nor below the end a [`cut`](Self::cut) gave the closing group,
	/// whose span runs to there: the marker starts at that end. That object exists on
	/// moq-transport, so the live edge moves immediately; a consumer MUST NOT submit it to a
	/// decoder. An empty payload is data on a data track, so its marker is one object with no
	/// bytes at all, not even the container's timestamp. The next [`write`](Self::write) opens the
	/// group after the marker, which it has to start after: group starts strictly increase, so a
	/// keyframe at the marker is stamped one unit past it.
	///
	/// Before anything is written there is no timeline to break, so it publishes nothing.
	///
	/// To bound the closing group's final frame, [`cut(end)`](Self::cut) before calling this.
	/// Otherwise the open group closes without a duration marker: what resumes may land sooner
	/// than one estimated frame later (a capture that reopens at once), and a guessed end past
	/// it would read as a rewind to every consumer.
	pub fn discontinuity(&mut self) -> crate::Result<()> {
		// The live edge once the open group closes, which raises it to the furthest point written.
		let edge = match (&self.group, self.end) {
			(Some(_), Some(end)) if self.live_edge.is_none_or(|edge| !timestamp_lt(end, edge)) => Some(end),
			_ => self.live_edge,
		};
		// The marker starts one unit of the track's timescale before the live edge, so media
		// resuming right at the edge still starts after it, but not below the latest frame, which
		// it would otherwise claim to end before. Or one unit past the newest group when that
		// doesn't pass the group's start. The open group ends whole there, so a reader can tell
		// the marker follows it.
		let marker = match (self.pending_start, edge) {
			// A seek keyed the next group. Its marker is stamped at the live edge, or one unit past
			// the newest group when the edge is unknown or doesn't pass it, since starts strictly
			// increase.
			(Some(start), edge) => {
				let start = self.sought(start)?;
				let edge = edge.unwrap_or(moq_net::Timestamp::ZERO);
				let timestamp = match self.inner.info().timescale {
					Some(_) => crate::start::next(&self.inner, edge)?.1,
					None => edge,
				};
				Some((start, timestamp, false))
			}
			// Nothing was written, so there is no timeline to break.
			(None, None) => None,
			(None, Some(edge)) => {
				// Nor inside the span of a group a cut ended whole, which runs to its End.
				let newest = self.newest.unwrap_or(moq_net::Timestamp::ZERO);
				let floor = self.ended.map_or(newest, |ended| super::later(newest, ended));
				let (start, timestamp) = crate::start::before(&self.inner, edge, floor)?;
				Some((start, timestamp, true))
			}
		};
		let whole = marker.and_then(|(_, timestamp, appended)| appended.then_some(timestamp));
		self.close(None, None, whole)?;
		// Nothing is measured across the break: the frames still open on this side have no end, and
		// the gap to the far side is not a frame duration.
		self.estimator.discontinuity();
		self.cadence = None;
		let Some((start, timestamp, appended)) = marker else {
			return Ok(());
		};
		self.pending_start = None;
		let mut group = match appended {
			true => self.inner.append_group(timestamp)?,
			false => self.inner.create_group(moq_net::group::Info { start })?,
		};
		match self.container.kind() {
			Kind::Data => group.write_frame(timestamp, Bytes::new())?,
			Kind::Audio | Kind::Video => self.container.write(
				&mut group,
				&[Frame {
					timestamp,
					payload: Bytes::new(),
					keyframe: false,
					duration: None,
				}],
			)?,
		}
		group.finish()?;
		Ok(())
	}

	/// The presentation span of the buffered frames, zero for fewer than two.
	///
	/// Taken across the buffer rather than first to last: frames within a track are in *decode*
	/// order, and B-frames have non-monotonic PTS, so `last - first` can shrink as a B-frame lands
	/// between two earlier-PTS frames.
	fn buffered_span(&self) -> std::time::Duration {
		let mut timestamps = self
			.buffer
			.iter()
			.filter_map(|frame| std::time::Duration::try_from(frame.timestamp).ok());
		let Some(first) = timestamps.next() else {
			return std::time::Duration::ZERO;
		};
		let (min, max) = timestamps.fold((first, first), |(min, max), ts| (min.min(ts), max.max(ts)));
		max - min
	}

	/// Flush any buffered frames into the current group without closing it.
	///
	/// Backfills the per-sample duration the source didn't provide. A CMAF fragment
	/// reconstructs each sample's DTS by accumulating durations, so every non-final
	/// sample packed into one fragment needs one or the decoder collapses their
	/// timestamps. Frames are in decode order, so a sample's duration is the gap to the
	/// next buffered sample; the final sample borrows `next` (the timestamp of the
	/// keyframe that rolled the group over), which is already in hand so this adds no
	/// latency. Frames that already carry a duration (e.g. fMP4 passthrough) keep it,
	/// and a backwards gap (a B-frame whose successor presents earlier) is left unset.
	/// Containers that don't use per-frame durations (Legacy, LOC) ignore the field.
	/// The boundary is converted into the frame's own scale first, so the duration
	/// stays exact in native ticks; a micros round-trip would quantize scales like
	/// 90 kHz (3003 ticks is not a whole number of micros) and fMP4 would refuse
	/// the inexact `trun` conversion.
	fn flush_buffer(&mut self, next: Option<moq_net::Timestamp>) -> crate::Result<()> {
		if self.buffer.is_empty() {
			return Ok(());
		}

		for i in 0..self.buffer.len() {
			if self.buffer[i].duration.is_some() {
				continue;
			}
			let boundary = self.buffer.get(i + 1).map(|f| f.timestamp).or(next);
			if let Some(boundary) = boundary
				&& let Some(scale) = self.buffer[i].timestamp.scale()
				&& let Ok(boundary) = boundary.convert(scale)
				&& let Ok(duration) = boundary.checked_sub(self.buffer[i].timestamp)
			{
				self.buffer[i].duration = Some(duration);
			}
		}

		let group = match &mut self.group {
			Some(group) => group,
			None => return Ok(()),
		};

		self.container.write(group, &self.buffer)?;
		// The batch reaches the transport at once, so its earliest frame is as late as the span to its
		// latest. That is jitter, not delay: the latest frame isn't late at all.
		let span = self.buffered_span();
		self.estimator.burst(span);
		// Counted only once written: an abort discards the buffer, and those frames were never sent.
		let mut buffer = std::mem::take(&mut self.buffer);
		for frame in &buffer {
			self.order(frame.timestamp);
			self.count(frame.payload.len(), frame.keyframe);
		}
		buffer.clear();
		self.buffer = buffer;

		Ok(())
	}

	/// Finish the track, flushing any buffered frames and closing any open group, and return
	/// the [`Finished`] track.
	///
	/// A finish that fails ends the track with the cause instead, so it can't be retried.
	pub fn finish(mut self) -> crate::Result<Finished<C, R>> {
		self.end_with(None)?;
		Ok(Finished { producer: self })
	}

	/// [`finish`](Self::finish) the track with `err`, for a source that failed after
	/// producing media worth keeping.
	///
	/// Unlike [`abort`](Self::abort), every group written stays readable and keeps being
	/// served; a reader reaching the end gets `err` instead of the end of stream. Over IETF
	/// moq-transport, a remote subscriber is told only of a generic internal error.
	pub fn fail(mut self, err: moq_net::Error) -> crate::Result<Finished<C, R>> {
		self.end_with(Some(err))?;
		Ok(Finished { producer: self })
	}

	/// Flush and end the track, cleanly or with `err`, for an owner that keeps this producer.
	pub(crate) fn end_with(&mut self, err: Option<moq_net::Error>) -> crate::Result<()> {
		if let Err(cause) = self.cut(None) {
			// A track that cannot close its last group ends with the cause rather than staying open.
			let _ = self.inner.clone().abort(cause.reason());
			return Err(cause);
		}
		// The producer keeps its own clone, whose appends the end refuses.
		match err {
			None => drop(self.inner.clone().finish()?),
			Some(err) => drop(self.inner.clone().fail(err)?),
		}
		let published = self.publish_estimate();
		// A track that ended before measuring declares what it saw, and no longer holds the join
		// interval back.
		if let Some(mut member) = self.member.take() {
			member.finish(self.estimator.partial_interval());
		}
		// A finished track may be kept around, but its media stops holding the other tracks back.
		self.estimator.discontinuity();
		published
	}

	/// [`finish`](Self::finish) the track unless it already ended, for an owner about to drop it.
	pub(crate) fn end(&mut self) -> crate::Result<()> {
		if self.inner.finished().is_some() || self.inner.is_closed() {
			return Ok(());
		}
		self.end_with(None)
	}

	/// Abort the track and any open group with the given error.
	///
	/// The counterpart to [`Self::finish`] for a failed teardown: consumers observe
	/// `err` instead of the generic [`moq_net::Error::Dropped`] a bare drop surfaces,
	/// so the real cause (a disconnect, a decode failure) reaches them. Any buffered
	/// frames are discarded, not flushed. Consumes the producer.
	pub fn abort(mut self, err: moq_net::Error) {
		self.buffer.clear();
		if let Some(group) = self.group.take() {
			let _ = group.abort(err.clone());
		}
		let _ = self.inner.abort(err);
	}

	/// Create a consumer for this track.
	pub fn consume(&self) -> moq_net::track::Subscriber {
		self.inner.subscribe(None)
	}
}

/// A media track whose producer [finished](Producer::finish) it: every frame is written, and it
/// can still be read or aborted, but not written to.
///
/// Dropping it releases the track like dropping a [`Producer`]: it leaves its broadcast's listing,
/// and a catalog rendition it owns leaves the catalog.
///
/// ```compile_fail,E0599
/// # fn cannot_write(track: moq_mux::container::Finished<moq_mux::catalog::hang::Container>) {
/// track.discontinuity().unwrap();
/// # }
/// ```
#[must_use = "dropping it releases the track and its catalog rendition"]
pub struct Finished<C: Container, R = ()> {
	producer: Producer<C, R>,
}

impl<C: Container, R: Clone + Send + 'static> Finished<C, R>
where
	crate::Error: From<C::Error>,
{
	/// The catalog key owned by this track; see [`Producer::name`].
	pub fn name(&self) -> &str {
		self.producer.name()
	}

	/// A watch-only handle to this track's subscriber demand.
	pub fn demand(&self) -> moq_net::track::Demand {
		self.producer.inner.demand()
	}

	/// The properties the track was created with.
	pub fn info(&self) -> &moq_net::track::Info {
		self.producer.inner.info()
	}

	/// Create a consumer for this track.
	pub fn consume(&self) -> moq_net::track::Subscriber {
		self.producer.inner.subscribe(None)
	}

	/// The jitter, bitrate, delay, and framerate measured from every frame written; see
	/// [`Producer::estimate`].
	pub fn estimate(&self) -> crate::catalog::Estimate {
		self.producer.estimate()
	}

	/// Abort the track with `err`; see [`moq_net::track::Finished::abort`]. Readers still draining get the end
	/// the track declared instead once every group below it finished: the clean end after [`Producer::finish`],
	/// or the error [`Producer::fail`] gave.
	pub fn abort(self, err: moq_net::Error) {
		self.producer.abort(err);
	}
}

impl<C: Container, R> std::ops::Deref for Producer<C, R> {
	type Target = moq_net::track::Producer;

	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::catalog::hang::Container;
	use moq_net::Timestamp;

	/// Mint a standalone track for tests via a throwaway broadcast, since tracks are
	/// born from their broadcast (no public `track::Producer::new`).
	fn track_producer(
		name: impl Into<std::sync::Arc<str>>,
		info: impl Into<Option<moq_net::track::Info>>,
	) -> moq_net::track::Producer {
		moq_net::broadcast::Info::new()
			.produce()
			.create_track(name, info)
			.unwrap()
	}

	/// A replay window wide enough to read a whole batch back.
	///
	/// These tests write every group up front and only then read, which the default
	/// [`std::time::Duration::ZERO`](std::time::Duration::ZERO) budget collapses to the live
	/// edge: history has to be asked for.
	/// Oldest first, as they are read back.
	fn replay() -> moq_net::track::Subscription {
		moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
			moq_net::track::Start::Live {
				min: Some(RECORDING_MAX_DELAY),
				max: Some(RECORDING_MAX_DELAY),
			},
		)
		.with_order(moq_net::track::Order::Ascending)])
	}

	/// The media track's full max age, so readers started after publishing
	/// can still consume every fresh group.
	const RECORDING_MAX_DELAY: std::time::Duration = std::time::Duration::from_secs(30);

	fn frame(timestamp_us: u64, keyframe: bool) -> Frame {
		Frame {
			timestamp: Timestamp::from_micros(timestamp_us).unwrap(),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe,
			duration: None,
		}
	}

	fn sized_frame(timestamp_us: u64, keyframe: bool, bytes: usize) -> Frame {
		Frame {
			payload: Bytes::from(vec![0u8; bytes]),
			..frame(timestamp_us, keyframe)
		}
	}

	/// The catalog estimate falls out of the writes themselves: a publisher never records anything
	/// by hand, it just hands `estimate()` to its rendition.
	#[tokio::test]
	async fn writes_measure_bitrate_without_inventing_jitter() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Data));

		// 25ms frames of 5 kB, a keyframe every 10, over more than the bitrate window.
		for i in 0..80u64 {
			producer.write(sized_frame(i * 25_000, i % 10 == 0, 5_000)).unwrap();
		}
		let producer = producer.finish().unwrap();

		let estimate = producer.estimate();
		assert_eq!(estimate.jitter, None, "PTS spacing alone is not jitter");
		assert_eq!(estimate.bitrate, Some(1_600_000));
	}

	type Rendition = Producer<Container, hang::catalog::VideoConfig>;

	/// Video renditions sharing one catalog, each opened with a keyframe at 0.
	fn renditions(names: &[&str]) -> (moq_net::broadcast::Producer, crate::catalog::Producer, Vec<Rendition>) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut tracks = Vec::new();
		for name in names {
			let net = broadcast
				.create_track(*name, catalog.track_info(hang::catalog::PRIORITY.video))
				.unwrap();
			let config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
			let mut track = catalog
				.video(net, Container::Legacy(crate::container::Kind::Video), config)
				.unwrap();
			track.write(frame(0, true)).unwrap();
			tracks.push(track);
		}
		(broadcast, catalog, tracks)
	}

	#[test]
	fn catalog_write_order_measures_delay_across_renditions() {
		let (_broadcast, catalog, mut tracks) = renditions(&["fast", "slow"]);

		// The slow rendition writes each frame after the fast one has written 200ms past it, for longer
		// than the window delay is measured over.
		for i in 1..40u64 {
			tracks[0].write(frame(i * 40_000 + 200_000, true)).unwrap();
			tracks[1].write(frame(i * 40_000, true)).unwrap();
		}
		for track in &mut tracks {
			track.cut(None).unwrap();
		}

		let ms = std::time::Duration::from_millis;
		let renditions = catalog.snapshot().video.renditions;
		assert_eq!(renditions["slow"].delay, Some(ms(200)));
		assert_eq!(renditions["fast"].delay, None);
		assert_eq!(renditions["slow"].jitter, None, "a constant trail is not jitter");
	}

	#[test]
	fn reconfigure_keeps_floors_until_the_track_identity_changes() {
		let ms = std::time::Duration::from_millis;
		for codec in ["avc3.42c01e", "hev1.1.6.L93.B0"] {
			let (_broadcast, catalog, mut tracks) = renditions(&["fast", "slow"]);
			let slow = &mut tracks[1];
			// Start a new identity, as an importer does when it resolves its codec.
			let mut config = hang::catalog::VideoConfig::new(codec.parse::<hang::catalog::VideoCodec>().unwrap());
			config.coded_width = Some(640);
			config.coded_height = Some(360);
			slow.reconfigure(config.clone()).unwrap();
			let name = slow.name().to_string();
			tracks[1].reorder(Timestamp::from_millis(100).unwrap());
			for i in 1..40u64 {
				tracks[0].write(frame(i * 40_000 + 200_000, true)).unwrap();
				tracks[1].write(frame(i * 40_000, true)).unwrap();
			}
			tracks[1].cut(None).unwrap();
			let measured = catalog.snapshot().video.renditions[&name].clone();
			assert_eq!((measured.jitter, measured.delay), (Some(ms(100)), Some(ms(200))));

			// Match an external catalog edit, such as the HLS conformance fixture's raise.
			let mut edit = tracks[1].modify().unwrap();
			edit.jitter = Some(ms(300));
			edit.delay = Some(ms(400));
			edit.commit().unwrap();
			config.coded_width = Some(1280);
			config.coded_height = Some(720);
			tracks[1].reconfigure(config.clone()).unwrap();
			assert_eq!(tracks[1].name(), name);
			let current = catalog.snapshot().video.renditions[&name].clone();
			assert_eq!((current.coded_width, current.coded_height), (Some(1280), Some(720)));
			assert_eq!((current.jitter, current.delay), (Some(ms(300)), Some(ms(400))));

			for (jitter, delay) in [(Some(ms(250)), None), (None, Some(ms(350)))] {
				let mut lower = config.clone();
				lower.jitter = jitter;
				lower.delay = delay;
				let err = tracks[1].reconfigure(lower).unwrap_err();
				assert!(matches!(
					(jitter, err),
					(Some(_), crate::Error::JitterDecreased) | (None, crate::Error::DelayDecreased)
				));
				assert_eq!(catalog.snapshot().video.renditions[&name], current);
			}

			// A new identity owns a new floor, even when it supplies lower values.
			config.codec = hang::catalog::VideoCodec::VP8;
			config.jitter = Some(ms(20));
			config.delay = Some(ms(30));
			tracks[1].reconfigure(config).unwrap();
			assert_ne!(tracks[1].name(), name);
			let current = catalog.snapshot().video.renditions[tracks[1].name()].clone();
			assert_eq!((current.jitter, current.delay), (Some(ms(20)), Some(ms(30))));
		}
	}

	/// History caught up behind the live edge is not a late write: only what follows it measures delay.
	#[test]
	fn history_measures_no_delay() {
		let (_broadcast, catalog, mut tracks) = renditions(&["live", "replay"]);
		tracks[0].write(frame(2_000_000, true)).unwrap();

		tracks[1].history(moq_net::Timestamp::from_micros(1_960_000).unwrap());
		for i in 1..=49u64 {
			tracks[1].write(frame(i * 40_000, true)).unwrap();
		}
		tracks[1].cut(None).unwrap();
		assert_eq!(catalog.snapshot().video.renditions["replay"].delay, None);

		// Live frames past the history that still trail the other rendition are late writes.
		for i in 0..40u64 {
			tracks[0].write(frame(2_040_000 + i * 40_000, true)).unwrap();
			tracks[1].write(frame(2_020_000 + i * 40_000, true)).unwrap();
		}
		tracks[1].cut(None).unwrap();
		assert_eq!(tracks[1].estimate().delay, Some(std::time::Duration::from_millis(20)));
	}

	/// Buffered frames reach the transport when the buffer flushes, so a second-long buffer's oldest
	/// frame trails the other rendition by the batch's span, and its newest by nothing: the span is
	/// jitter, not delay.
	#[test]
	fn a_buffered_rendition_flushes_its_span_as_jitter() {
		let (_broadcast, catalog, tracks) = renditions(&["live", "buffered"]);
		let [mut live, buffered] = <[Rendition; 2]>::try_from(tracks).ok().unwrap();
		let mut buffered = buffered.with_buffer(std::time::Duration::from_secs(1));

		for i in 1..=25u64 {
			live.write(frame(i * 40_000, false)).unwrap();
			buffered.write(frame(i * 40_000, false)).unwrap();
		}
		buffered.cut(None).unwrap();

		let buffered = &catalog.snapshot().video.renditions["buffered"];
		assert_eq!(
			(buffered.jitter, buffered.delay),
			(Some(std::time::Duration::from_millis(960)), None)
		);
	}

	/// A finished rendition may be kept around, but its media stops holding the others back.
	#[test]
	fn a_finished_rendition_stops_holding_others_back() {
		let (_broadcast, catalog, tracks) = renditions(&["ended", "live"]);
		let Ok([mut ended, mut live]) = <[_; 2]>::try_from(tracks) else {
			unreachable!("two renditions")
		};
		ended.write(frame(10_000_000, true)).unwrap();
		let _ended = ended.finish().unwrap();

		live.write(frame(1_000_000, true)).unwrap();
		assert_eq!(catalog.snapshot().video.renditions["live"].delay, None);
	}

	/// A passthrough producer claims nothing until the first window closes, then
	/// reserves that peak, raises the ceiling on a louder later window, and holds
	/// it when a quieter one follows.
	#[tokio::test]
	async fn a_passthrough_producer_ratchets_its_bandwidth_claim() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let _sub = track.subscribe(None);
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Data))
			.with_bandwidth(moq_net::bandwidth::Allocator::unlimited());

		producer.write(sized_frame(0, true, 100_000)).unwrap();
		producer.write(sized_frame(500_000, false, 100_000)).unwrap();
		assert_eq!(producer.bandwidth_ceiling(), None, "the first window has not closed");

		producer.cut(Some(Timestamp::from_micros(1_000_000).unwrap())).unwrap();
		assert_eq!(
			producer.bandwidth_ceiling(),
			Some(moq_net::bandwidth::Rate::from_bps(1_600_000))
		);

		producer.write(sized_frame(1_000_000, true, 25_000)).unwrap();
		producer.cut(Some(Timestamp::from_micros(2_000_000).unwrap())).unwrap();
		assert_eq!(
			producer.bandwidth_ceiling(),
			Some(moq_net::bandwidth::Rate::from_bps(1_600_000)),
			"a quieter window never hands the room away"
		);

		producer.write(sized_frame(2_000_000, true, 250_000)).unwrap();
		producer.cut(Some(Timestamp::from_micros(3_000_000).unwrap())).unwrap();
		assert_eq!(
			producer.bandwidth_ceiling(),
			Some(moq_net::bandwidth::Rate::from_bps(2_000_000))
		);
	}

	/// A passthrough want at its peak lowers a co-resident encoder's grant by
	/// exactly that amount, which is the whole reason the import claims at all.
	#[tokio::test]
	async fn a_passthrough_peak_lowers_a_coresident_encoder_grant() {
		let estimate = moq_net::bandwidth::Producer::new();
		let allocator = moq_net::bandwidth::Allocator::new(estimate.consume());
		estimate
			.set(Some(moq_net::bandwidth::Rate::from_bps(6_000_000)))
			.unwrap();

		fn video_track() -> (moq_net::broadcast::Producer, moq_net::track::Producer) {
			let broadcast = moq_net::broadcast::Info::new().produce();
			let track = broadcast
				.create_track("t", hang::container::track_info(hang::catalog::PRIORITY.video))
				.unwrap();
			(broadcast, track)
		}

		let (_encoder_broadcast, encoder_track) = video_track();
		let _encoder_sub = encoder_track.subscribe(None);
		let encoder = allocator.reserve(&encoder_track.demand(), moq_net::bandwidth::Rate::from_bps(8_000_000));
		assert_eq!(
			encoder.rate(),
			Some(moq_net::bandwidth::Rate::from_bps(6_000_000)),
			"alone, the encoder takes the whole estimate"
		);

		let (_passthrough_broadcast, passthrough) = video_track();
		let _passthrough_sub = passthrough.subscribe(None);
		let mut producer =
			Producer::new(passthrough, Container::Legacy(crate::container::Kind::Data)).with_bandwidth(allocator);

		producer.write(sized_frame(0, true, 100_000)).unwrap();
		producer.write(sized_frame(500_000, false, 100_000)).unwrap();
		assert_eq!(
			encoder.rate(),
			Some(moq_net::bandwidth::Rate::from_bps(6_000_000)),
			"nothing claimed before the first window"
		);

		producer.cut(Some(Timestamp::from_micros(1_000_000).unwrap())).unwrap();
		assert_eq!(
			encoder.rate(),
			Some(moq_net::bandwidth::Rate::from_bps(4_400_000)),
			"the encoder's grant drops by the passthrough peak (1.6 Mbps)"
		);
	}

	/// One group per frame (how the importer facade drives audio) closes each group with an
	/// unbounded `cut`, which leaves the measurement open for the next frame's timestamp to close.
	/// Dropping it there instead would leave an audio track's bitrate permanently undetectable.
	#[tokio::test]
	async fn per_frame_groups_still_measure_bitrate() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Data));

		// 40ms packets of 5 kB: 1 Mbps.
		for i in 0..40u64 {
			let keyframe = producer.needs_keyframe();
			producer.write(sized_frame(i * 40_000, keyframe, 5_000)).unwrap();
			producer.cut(None).unwrap();
		}

		assert_eq!(producer.estimate().bitrate, Some(1_000_000));
	}

	/// A break in the timeline is not a frame duration, so the estimate never spans one. Without the
	/// reset a paused publisher would advertise the whole gap as the buffer a player must hold.
	#[tokio::test]
	async fn discontinuity_is_not_measured_across() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Data));

		producer.write(sized_frame(0, true, 5_000)).unwrap();
		producer.discontinuity().unwrap();
		// Resumed 40 minutes later, on a re-anchored clock.
		producer.write(sized_frame(2_405_070_000, true, 5_000)).unwrap();
		let producer = producer.finish().unwrap();

		assert_eq!(producer.estimate().jitter, None);
		assert_eq!(producer.estimate().bitrate, None);
	}

	/// Write-only container that rejects the frame at a given timestamp, standing in for one the
	/// container can't encode (over the group size limit, a timestamp that won't convert).
	#[derive(Clone)]
	struct RejectAt(u64);

	impl super::Container for RejectAt {
		type Error = crate::Error;

		fn write(&self, _group: &mut moq_net::group::Producer, frames: &[Frame]) -> Result<(), Self::Error> {
			match frames
				.iter()
				.any(|f| f.timestamp.as_micros().unwrap() == self.0 as u128)
			{
				true => Err(moq_net::Error::FrameTooLarge.into()),
				false => Ok(()),
			}
		}

		fn decode(&self, _frame: moq_net::frame::Frame) -> Result<Vec<Frame>, Self::Error> {
			unreachable!("RejectAt is write-only")
		}
	}

	/// A rejected frame leaves the producer usable, so the caller can carry on. Its bytes never
	/// reached the wire, and the estimate's maximum never falls, so counting them would pin an
	/// inflated bitrate on the catalog permanently.
	#[tokio::test]
	async fn a_rejected_frame_is_not_measured() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut producer = Producer::new(track, RejectAt(40_000));

		// 40ms frames of 5 kB (1 Mbps), except one 500 kB frame the container turns away.
		for i in 0..40u64 {
			let bytes = if i == 1 { 500_000 } else { 5_000 };
			let result = producer.write(sized_frame(i * 40_000, true, bytes));
			assert_eq!(result.is_err(), i == 1, "only the rejected frame fails");
		}

		assert_eq!(producer.estimate().bitrate, Some(1_000_000));
	}

	/// Reorder delay is the one input the writes can't reveal, since frames carry no decode time.
	#[tokio::test]
	async fn reorder_raises_the_measured_jitter() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Data));

		producer.write(frame(0, true)).unwrap();
		producer.write(frame(16_000, false)).unwrap();
		assert_eq!(producer.estimate().jitter, None, "PTS spacing alone is not flush delay");

		producer.reorder(Timestamp::from_micros(48_000).unwrap());
		assert_eq!(
			producer.estimate().jitter,
			Some(std::time::Duration::from_millis(48)),
			"a B-frame stream needs the deeper decode buffer"
		);
	}

	/// Drain all groups from a finished track, returning their frame counts.
	async fn collect_groups(consumer: moq_net::track::Subscriber) -> Vec<usize> {
		// Key order, oldest first, whatever the subscription's Group Order.
		let mut consumer = consumer.ordered();
		let mut groups = Vec::new();
		while let Some(mut group) = consumer.next_group().await.unwrap() {
			let mut count = 0;
			while group.next_frame().await.unwrap().is_some() {
				count += 1;
			}
			groups.push(count);
		}
		groups
	}

	/// A discontinuity lands as its own marker group (one empty frame) between the
	/// content either side, so a consumer can see the break instead of inferring
	/// continuity from adjacent sequences.
	#[tokio::test]
	async fn discontinuity_publishes_an_empty_group() {
		// The resumed clock jumps forty minutes, so both the max age and the
		// drift budget have to cover it or the pre-discontinuity group reads as ancient.
		let discontinuity_max_age = std::time::Duration::from_secs(41 * 60);
		let info = hang::container::track_info(hang::catalog::PRIORITY.audio).with_max_age(discontinuity_max_age);
		let track = track_producer("test", info);
		let consumer =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(discontinuity_max_age),
						max: Some(discontinuity_max_age),
					},
				)]),
			);
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Audio));

		producer.write(frame(0, true)).unwrap();
		producer.write(frame(10_000, false)).unwrap();
		producer.discontinuity().unwrap();
		// Resumed on a re-anchored clock, 40 minutes later.
		producer.write(frame(2_405_070_000, true)).unwrap();
		let _finished = producer.finish().unwrap();

		let groups = collect_payloads(consumer).await;
		assert_eq!(groups.len(), 3);
		assert_eq!(groups[0], vec![(0, 2), (10_000, 2)]);
		assert_eq!(
			groups[1],
			vec![(10_000, 0)],
			"the marker is one empty frame at the last frame, which has no duration to end later"
		);
		assert_eq!(groups[2][0], (2_405_070_000, 2));
	}

	/// A subscription starts at the track's LATEST group, and the marker advances it even
	/// though it carries nothing. So a subscriber arriving mid-break waits for real media
	/// rather than being handed the pre-break group as if it were live -- which is how a
	/// 40-minute-stale frame once reached a VOD recording.
	#[test]
	fn discontinuity_moves_the_live_edge_off_stale_content() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Audio));

		producer.write(frame(0, true)).unwrap();
		let stale = producer.track().largest().map(|largest| largest.group);

		producer.discontinuity().unwrap();
		let fresh = producer.track().largest().map(|largest| largest.group);

		assert!(fresh > stale, "the marker group is the live edge now");
	}

	/// The marker sits at the duration marker a cut wrote, where the cut ended its group whole: not
	/// a unit before it, inside that group's span, nor at the last frame below it, which a consumer
	/// bounding groups by their reach would read as a rewind.
	#[tokio::test]
	async fn discontinuity_after_a_cut_starts_at_the_duration_marker() {
		for (end, marker) in [(Some(25_000), 25_000), (None, 20_000)] {
			let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
			let consumer = track.subscribe(replay());
			let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));

			producer.write(frame(0, true)).unwrap();
			assert_eq!(producer.live_edge(), None, "no group has ended yet");
			producer.write(frame(10_000, false)).unwrap();
			producer.cut(end.map(|us| Timestamp::from_micros(us).unwrap())).unwrap();
			// With no group open, a cut's bound is a point nothing reached.
			producer.cut(Some(Timestamp::from_micros(90_000).unwrap())).unwrap();
			producer.discontinuity().unwrap();
			let _finished = producer.finish().unwrap();

			let groups = collect_payloads(consumer).await;
			assert_eq!(
				groups[0].last(),
				Some(&(marker, 0)),
				"cut({end:?}) writes its duration marker"
			);
			assert_eq!(groups[1], vec![(marker, 0)], "cut({end:?}) then discontinuity");
		}
	}

	/// Explicit keyframe closes the current group and starts a new one.
	#[tokio::test]
	async fn keyframe_closes_group_immediately() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Data));

		producer.write(frame(0, true)).unwrap(); // first frame must be a keyframe
		producer.write(frame(10_000, false)).unwrap();
		producer.write(frame(20_000, true)).unwrap(); // keyframe → new group
		producer.write(frame(30_000, false)).unwrap();
		let _finished = producer.finish().unwrap();

		assert_eq!(collect_groups(consumer).await, vec![2, 2]);
	}

	/// `needs_keyframe` tracks whether a group is open, so an audio importer can mark only the first
	/// frame of each group a keyframe (`keyframe: producer.needs_keyframe()`) and accumulate the rest
	/// into that group until it cuts or seeks. Regression guard for the "one group (one QUIC stream)
	/// per audio packet" storm.
	#[tokio::test]
	async fn needs_keyframe_drives_audio_grouping() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Data));

		// Drive grouping off `needs_keyframe`, as the audio importers do: the first frame of each
		// group is a keyframe, the rest are not, so they accumulate into ONE group...
		for ts in [0, 10_000, 20_000] {
			let keyframe = producer.needs_keyframe();
			producer.write(frame(ts, keyframe)).unwrap();
		}
		producer.cut(None).unwrap();
		// ...until the caller draws a boundary, which opens the next group.
		for ts in [30_000, 40_000] {
			let keyframe = producer.needs_keyframe();
			producer.write(frame(ts, keyframe)).unwrap();
		}
		let _finished = producer.finish().unwrap();

		assert_eq!(collect_groups(consumer).await, vec![3, 2]);
	}

	/// Drain all groups, returning each group's (timestamp_micros, payload_len) pairs.
	async fn collect_payloads(consumer: moq_net::track::Subscriber) -> Vec<Vec<(u128, usize)>> {
		// Key order, oldest first, whatever the subscription's Group Order.
		let mut consumer = consumer.ordered();
		let mut groups = Vec::new();
		while let Some(mut group) = consumer.next_group().await.unwrap() {
			let mut frames = Vec::new();
			while let Some(frame) = group.read_frame().await.unwrap() {
				let decoded = hang::container::Frame::decode(frame.payload).unwrap();
				frames.push((decoded.timestamp.as_micros().unwrap(), decoded.payload.len()));
			}
			groups.push(frames);
		}
		groups
	}

	/// A video group ends with an empty frame at the next keyframe's timestamp.
	#[tokio::test]
	async fn cut_writes_a_duration_marker_at_the_callers_bound() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));

		producer.write(frame(0, true)).unwrap();
		producer.write(frame(10_000, false)).unwrap();
		producer
			.cut(Some(moq_net::Timestamp::from_micros(15_000).unwrap()))
			.unwrap();
		producer.write(frame(20_000, true)).unwrap();
		let _finished = producer.finish().unwrap();

		let groups = collect_payloads(consumer).await;
		assert_eq!(groups[0], vec![(0, 2), (10_000, 2), (15_000, 0)]);
		assert_eq!(groups[1][0], (20_000, 2));
		assert_eq!(groups[1].last().unwrap().1, 0, "finish closes the last group");
	}

	/// Audio never writes a duration marker, even at finish.
	#[tokio::test]
	async fn audio_cut_writes_no_duration_marker() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Audio));

		producer.write(frame(0, true)).unwrap();
		producer.write(frame(20_000, false)).unwrap();
		let _finished = producer.finish().unwrap();

		assert_eq!(collect_payloads(consumer).await, vec![vec![(0, 2), (20_000, 2)]]);
	}

	/// Drain all LOC groups, returning each group's (timestamp_micros, payload_len) pairs.
	async fn collect_loc_payloads(consumer: moq_net::track::Subscriber) -> Vec<Vec<(u64, usize)>> {
		// Key order, oldest first, whatever the subscription's Group Order.
		let mut consumer = consumer.ordered();
		let mut groups = Vec::new();
		while let Some(mut group) = consumer.next_group().await.unwrap() {
			let mut frames = Vec::new();
			while let Some(frame) = group.read_frame().await.unwrap() {
				let decoded = moq_loc::decode(frame.payload).unwrap();
				frames.push((decoded.timestamp, decoded.payload.len()));
			}
			groups.push(frames);
		}
		groups
	}

	/// A LOC video group ends with an empty frame at the next keyframe's timestamp, like Legacy.
	#[tokio::test]
	async fn loc_cut_writes_a_duration_marker_at_the_callers_bound() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Loc(crate::container::Kind::Video));

		producer.write(frame(0, true)).unwrap();
		producer.write(frame(10_000, false)).unwrap();
		producer
			.cut(Some(moq_net::Timestamp::from_micros(15_000).unwrap()))
			.unwrap();
		producer.write(frame(20_000, true)).unwrap();
		let _finished = producer.finish().unwrap();

		let groups = collect_loc_payloads(consumer).await;
		assert_eq!(groups[0], vec![(0, 2), (10_000, 2), (15_000, 0)]);
		assert_eq!(groups[1][0], (20_000, 2));
		assert_eq!(groups[1].last().unwrap().1, 0, "finish closes the last group");
	}

	/// LOC audio never writes a duration marker, even at finish.
	#[tokio::test]
	async fn loc_audio_writes_no_duration_marker() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Loc(crate::container::Kind::Audio));

		producer.write(frame(0, true)).unwrap();
		producer.write(frame(20_000, false)).unwrap();
		producer
			.cut(Some(moq_net::Timestamp::from_micros(30_000).unwrap()))
			.unwrap();
		producer.write(frame(40_000, true)).unwrap();
		let _finished = producer.finish().unwrap();

		assert_eq!(
			collect_loc_payloads(consumer).await,
			vec![vec![(0, 2), (20_000, 2)], vec![(40_000, 2)]]
		);
	}

	/// LOC data tracks never write a duration marker, so an empty data frame stays data.
	#[tokio::test]
	async fn loc_data_writes_no_duration_marker() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Loc(crate::container::Kind::Data));

		producer.write(frame(0, true)).unwrap();
		producer
			.cut(Some(moq_net::Timestamp::from_micros(15_000).unwrap()))
			.unwrap();
		producer.write(frame(20_000, true)).unwrap();
		let _finished = producer.finish().unwrap();

		assert_eq!(
			collect_loc_payloads(consumer).await,
			vec![vec![(0, 2)], vec![(20_000, 2)]]
		);
	}

	/// `cut()` flushes the current group immediately; the next write must be a keyframe.
	#[tokio::test]
	async fn cut_closes_immediately() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Data));

		producer.write(frame(0, true)).unwrap();
		producer.write(frame(10_000, false)).unwrap();
		producer
			.cut(Some(moq_net::Timestamp::from_micros(15_000).unwrap()))
			.unwrap();
		producer.write(frame(20_000, true)).unwrap();
		let _finished = producer.finish().unwrap();

		assert_eq!(collect_groups(consumer).await, vec![2, 1]);
	}

	/// Writing a non-keyframe with no open group returns MissingKeyframe.
	#[test]
	fn first_frame_must_be_keyframe() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Data));

		let err = producer.write(frame(0, false)).unwrap_err();
		assert!(matches!(err, crate::Error::MissingKeyframe(_)));
	}

	/// Drain all groups from a finished track, returning their starts.
	async fn collect_starts(consumer: moq_net::track::Subscriber) -> Vec<u64> {
		// Key order, oldest first, whatever the subscription's Group Order.
		let mut consumer = consumer.ordered();
		let mut starts = Vec::new();
		while let Some(group) = consumer.next_group().await.unwrap() {
			starts.push(group.start);
		}
		starts
	}

	/// `seek(n)` opens the next group at start `n`.
	#[tokio::test]
	async fn seek_uses_explicit_start() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Data));

		producer.write(frame(0, true)).unwrap(); // start 0
		producer.seek(42).unwrap();
		producer.write(frame(10_000, true)).unwrap(); // start 42
		let _finished = producer.finish().unwrap();

		assert_eq!(collect_starts(consumer).await, vec![0, 42]);
	}

	/// A seek made before a decoder identity change keys the successor's first group.
	#[tokio::test]
	async fn a_seek_carries_across_a_new_identity() {
		let (_broadcast, _catalog, mut tracks) = renditions(&["v"]);
		let track = &mut tracks[0];
		track.seek(42).unwrap();
		let config = hang::catalog::VideoConfig::new("avc3.42c01e".parse::<hang::catalog::VideoCodec>().unwrap());
		track.reconfigure(config).unwrap();
		assert_ne!(track.name(), "v", "a new identity mints a new track");
		let consumer = track.inner.subscribe(replay());
		track.write(frame(10_000, true)).unwrap();
		track.cut(None).unwrap();
		drop(track.inner.clone().finish().unwrap());

		assert_eq!(collect_starts(consumer).await, vec![42]);
	}

	/// `seek` is consumed on the next group creation; subsequent groups start at their keyframes.
	#[tokio::test]
	async fn seek_clears_pending_after_use() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Data));

		producer.seek(5).unwrap();
		producer.write(frame(0, true)).unwrap(); // start 5
		producer.write(frame(10_000, true)).unwrap(); // start 10000, its keyframe
		let _finished = producer.finish().unwrap();

		assert_eq!(collect_starts(consumer).await, vec![5, 10_000]);
	}

	/// A marker after a seek takes the seek's start, stamped past the groups before it even when
	/// nothing says where their content ended.
	#[tokio::test]
	async fn discontinuity_after_a_seek_takes_its_start() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Audio));

		producer.write(frame(0, true)).unwrap();
		producer.seek(100_000).unwrap();
		producer.discontinuity().unwrap();
		let _finished = producer.finish().unwrap();

		assert_eq!(collect_starts(consumer).await, vec![0, 100_000]);
	}

	/// Records the frames handed to each `write`, so tests can inspect the
	/// durations the producer backfilled. Write-only.
	#[derive(Clone, Default)]
	struct Recording(std::rc::Rc<std::cell::RefCell<Vec<Vec<Frame>>>>);

	impl super::Container for Recording {
		type Error = crate::Error;

		fn write(&self, _group: &mut moq_net::group::Producer, frames: &[Frame]) -> Result<(), Self::Error> {
			self.0.borrow_mut().push(frames.to_vec());
			Ok(())
		}

		fn decode(&self, _frame: moq_net::frame::Frame) -> Result<Vec<Frame>, Self::Error> {
			unreachable!("Recording is write-only")
		}
	}

	/// The keyframe that rolls a group over backfills the duration of the previous
	/// group's last frame, without buffering an extra frame.
	#[tokio::test]
	async fn keyframe_backfills_batched_durations() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let recording = Recording::default();
		let mut producer = Producer::new(track, recording.clone()).with_buffer(std::time::Duration::from_secs(10));

		producer.write(frame(0, true)).unwrap(); // group 0 opens
		producer.write(frame(33_000, false)).unwrap(); // buffered
		producer.write(frame(66_000, true)).unwrap(); // rolls group 0 over -> flush with next = 66ms
		let _finished = producer.finish().unwrap();

		let writes = recording.0.borrow();
		let group0 = &writes[0];
		assert_eq!(group0.len(), 2);
		// The first sample's duration is the gap to the next buffered sample: 33ms - 0.
		assert_eq!(group0[0].duration, Some(Timestamp::from_micros(33_000).unwrap()));
		// The last sample's duration is backfilled from the next keyframe: 66ms - 33ms.
		assert_eq!(group0[1].duration, Some(Timestamp::from_micros(33_000).unwrap()));
	}

	/// A `cut(None)` boundary is micro-scale (the estimated end), so without converting
	/// it into the frame's own scale the `checked_sub` refuses and the last frame
	/// silently keeps `duration: None`. 90 kHz frames, as the TS importer feeds them.
	#[tokio::test]
	async fn cut_none_backfills_the_last_frame_duration_at_native_scale() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let recording = Recording::default();
		let mut producer = Producer::new(track, recording.clone()).with_buffer(std::time::Duration::from_secs(10));

		let scale = moq_net::Timescale::new(90_000).unwrap();
		let frame_at = |ticks: u64, keyframe: bool| Frame {
			timestamp: Timestamp::new(ticks, scale).unwrap(),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe,
			duration: None,
		};

		// 3003 ticks is not a whole number of micros, so the micro-scale estimate
		// truncates on the way through.
		producer.write(frame_at(0, true)).unwrap();
		producer.write(frame_at(3003, false)).unwrap();
		producer.write(frame_at(6006, false)).unwrap();
		producer.cut(None).unwrap();
		let _finished = producer.finish().unwrap();

		let writes = recording.0.borrow();
		let group = &writes[0];
		assert_eq!(group.len(), 3);
		let duration = |ticks: u64| Timestamp::new(ticks, scale).unwrap();
		assert_eq!(group[0].duration, Some(duration(3003)));
		assert_eq!(group[1].duration, Some(duration(3003)));
		assert_eq!(
			group[2].duration,
			Some(duration(3002)),
			"the truncated estimate still lands on exact native ticks instead of going missing"
		);

		// Every duration converts into the fMP4 track timescale without remainder;
		// a micros round-trip would refuse here with SampleDurationInexact.
		let info = crate::container::fmp4::FragmentInfo {
			track_id: 1,
			timescale: scale,
			sequence_number: 0,
			kind: crate::container::fmp4::Kind::Video,
		};
		crate::container::fmp4::encode_fragment(info, group).unwrap();
	}

	/// Writes each frame onto the group at its own timescale, so the group's frames keep
	/// precision a microsecond container would drop.
	struct Stamped;

	impl super::Container for Stamped {
		type Error = crate::Error;

		fn write(&self, group: &mut moq_net::group::Producer, frames: &[Frame]) -> Result<(), Self::Error> {
			for frame in frames {
				group.write_frame(frame.timestamp, frame.payload.clone())?;
			}
			Ok(())
		}

		fn decode(&self, _frame: moq_net::frame::Frame) -> Result<Vec<Frame>, Self::Error> {
			unreachable!("Stamped is write-only")
		}

		fn kind(&self) -> Kind {
			Kind::Video
		}
	}

	/// A cut bound below the newest frame by less than a microsecond is still below it in the
	/// track's timescale, where `finish_at` checks it, so the cut is refused and the group stays
	/// open. An estimate summed in microseconds would fall below it too, so it ends at the frame.
	#[tokio::test]
	async fn a_bound_below_a_frame_by_under_a_microsecond_is_refused() {
		let nanos = moq_net::Timescale::new(1_000_000_000).unwrap();
		let info = hang::container::track_info(hang::catalog::PRIORITY.video).with_timescale(nanos);
		let track = track_producer("test", info);
		let mut consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Stamped);
		let at = |nanoseconds: u64, keyframe: bool| Frame {
			timestamp: Timestamp::new(nanoseconds, nanos).unwrap(),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe,
			duration: None,
		};
		producer.write(at(1_000_000, true)).unwrap();
		// Within one microsecond of the newest frame, which still counts as the newer.
		producer.write(at(2_000_001, false)).unwrap();
		producer.write(at(2_000_002, false)).unwrap();
		let below = producer.cut(Some(Timestamp::new(2_000_001, nanos).unwrap()));
		assert!(matches!(below, Err(crate::Error::InvalidEnd(_))), "{below:?}");
		assert!(!producer.needs_keyframe(), "the refused cut leaves the group open");
		producer.cut(None).unwrap();

		let mut group = consumer.recv_group().await.unwrap().unwrap();
		let mut frames = 0;
		while group.read_frame().await.unwrap().is_some() {
			frames += 1;
		}
		assert_eq!(frames, 3);
		assert_eq!(group.end(), Some(2_000_002), "the estimate ends at the newest frame");
	}

	#[tokio::test]
	async fn duration_marker_uses_cadence_not_batching_delay() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		producer.burst(std::time::Duration::from_secs(1)).unwrap();
		producer.write(frame(0, true)).unwrap();
		producer.write(frame(20_000, false)).unwrap();
		producer.write(frame(40_000, false)).unwrap();
		let _finished = producer.finish().unwrap();
		assert_eq!(collect_payloads(consumer).await[0].last(), Some(&(60_000, 0)));
	}

	#[tokio::test]
	async fn reordered_group_does_not_mark_the_decode_tail_with_the_presentation_end() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		for (index, timestamp) in [0, 120_000, 40_000, 80_000].into_iter().enumerate() {
			producer.write(frame(timestamp, index == 0)).unwrap();
		}
		producer.write(frame(160_000, true)).unwrap();
		producer.write(frame(200_000, false)).unwrap();
		producer.cut(Some(Timestamp::from_micros(240_000).unwrap())).unwrap();
		let _finished = producer.finish().unwrap();
		let groups = collect_payloads(consumer).await;
		assert_eq!(groups[0], vec![(0, 2), (120_000, 2), (40_000, 2), (80_000, 2)]);
		assert_eq!(
			groups[1].last(),
			Some(&(240_000, 0)),
			"the next group can mark its tail"
		);
	}

	#[tokio::test]
	async fn duration_marker_follows_a_slower_cadence() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		for (index, timestamp) in [0, 16_000, 32_000, 65_000, 98_000].into_iter().enumerate() {
			producer.write(frame(timestamp, index == 0)).unwrap();
		}
		let _finished = producer.finish().unwrap();
		assert_eq!(collect_payloads(consumer).await[0].last(), Some(&(131_000, 0)));
	}

	#[tokio::test]
	async fn an_unknown_tail_at_the_previous_endpoint_uses_current_cadence() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		let mut first = frame(0, true);
		first.duration = Some(Timestamp::from_micros(40_000).unwrap());
		producer.write(first).unwrap();
		producer.write(frame(40_000, false)).unwrap();
		let _finished = producer.finish().unwrap();
		assert_eq!(collect_payloads(consumer).await[0].last(), Some(&(80_000, 0)));
	}

	#[tokio::test(start_paused = true)]
	async fn backwards_cut_is_rejected_before_flushing_or_closing() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video))
			.with_buffer(std::time::Duration::from_secs(1));
		producer.write(frame(20_000, true)).unwrap();
		let mut group = consumer.recv_group().await.unwrap().unwrap();
		assert!(producer.cut(Some(Timestamp::from_micros(10_000).unwrap())).is_err());
		assert!(!producer.needs_keyframe());
		assert!(
			kio::clock::timeout(std::time::Duration::from_millis(1), group.read_frame())
				.await
				.is_err()
		);
		producer.write(frame(30_000, false)).unwrap();
		producer.cut(Some(Timestamp::from_micros(35_000).unwrap())).unwrap();
		let mut timestamps = Vec::new();
		while let Some(frame) = group.read_frame().await.unwrap() {
			timestamps.push(
				hang::container::Frame::decode(frame.payload)
					.unwrap()
					.timestamp
					.as_micros()
					.unwrap(),
			);
		}
		assert_eq!(timestamps, vec![20_000, 30_000, 35_000]);
	}

	#[tokio::test]
	async fn a_rewound_keyframe_is_refused() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		producer.write(frame(20_000, true)).unwrap();
		producer.write(frame(30_000, false)).unwrap();
		let err = producer.write(frame(0, true)).unwrap_err();
		assert!(matches!(err, crate::Error::TimestampRewind(_)));
	}

	/// The media frames of each group, without duration markers.
	fn media(groups: Vec<Vec<(u128, usize)>>) -> Vec<Vec<u128>> {
		groups
			.into_iter()
			.map(|group| {
				group
					.into_iter()
					.filter(|(_, len)| *len > 0)
					.map(|(ts, _)| ts)
					.collect()
			})
			.collect()
	}

	/// #3798's join: an IDR one frame below the open group's newest frame, but above its start,
	/// continues that group, so every frame of the next group presents after it.
	#[tokio::test]
	async fn a_keyframe_overlapping_the_open_group_continues_it() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		producer.write(frame(100_000, true)).unwrap();
		producer.write(frame(133_000, false)).unwrap();
		producer.write(frame(166_000, false)).unwrap();
		producer.write(frame(150_000, true)).unwrap();
		producer.write(frame(183_000, false)).unwrap();
		producer.write(frame(216_000, true)).unwrap();
		let _finished = producer.finish().unwrap();
		assert_eq!(
			media(collect_payloads(consumer).await),
			[vec![100_000, 133_000, 166_000, 150_000, 183_000], vec![216_000]]
		);
	}

	/// A splice whose keyframe lands just past the old content: a leading picture that would
	/// present at or before the old content's newest frame is dropped, with the keyframe's later
	/// leading pictures, which may reference it. The pictures presenting after the keyframe publish.
	#[tokio::test]
	async fn a_leading_picture_at_or_before_the_previous_group_is_dropped() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		producer.write(frame(100_000, true)).unwrap();
		producer.write(frame(166_000, false)).unwrap();
		producer.write(frame(133_000, false)).unwrap();
		producer.write(frame(200_000, true)).unwrap();
		producer.write(frame(150_000, false)).unwrap();
		producer.write(frame(166_000, false)).unwrap();
		producer.write(frame(183_000, false)).unwrap();
		producer.write(frame(233_000, false)).unwrap();
		let _finished = producer.finish().unwrap();
		assert_eq!(
			media(collect_payloads(consumer).await),
			[vec![100_000, 166_000, 133_000], vec![200_000, 233_000]]
		);
	}

	/// After a cut there is no open group for a keyframe at or before the newest frame to continue,
	/// so it is dropped with the frames decoding from it, until a keyframe past the newest frame.
	#[tokio::test]
	async fn a_keyframe_overlapping_a_closed_group_is_dropped() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		producer.write(frame(100_000, true)).unwrap();
		producer.write(frame(133_000, false)).unwrap();
		producer.write(frame(166_000, false)).unwrap();
		producer.cut(None).unwrap();
		producer.write(frame(150_000, true)).unwrap();
		assert!(producer.needs_keyframe());
		producer.write(frame(183_000, false)).unwrap();
		producer.write(frame(200_000, true)).unwrap();
		let _finished = producer.finish().unwrap();
		assert_eq!(
			media(collect_payloads(consumer).await),
			[vec![100_000, 133_000, 166_000], vec![200_000]]
		);
	}

	/// Group starts strictly increase, so a keyframe at the open group's start continues that group
	/// rather than opening one.
	#[tokio::test]
	async fn a_keyframe_at_the_previous_group_start_continues_it() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Audio));
		producer.write(frame(100_000, true)).unwrap();
		producer.write(frame(100_000, true)).unwrap();
		producer.write(frame(120_000, true)).unwrap();
		let _finished = producer.finish().unwrap();
		assert_eq!(collect_starts(consumer).await, vec![100_000, 120_000]);
	}

	/// A frame below the start of the group before its own reaches back past a whole group.
	#[tokio::test]
	async fn a_frame_below_the_previous_group_start_is_refused() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		producer.write(frame(100_000, true)).unwrap();
		producer.write(frame(133_000, false)).unwrap();
		producer.write(frame(200_000, true)).unwrap();
		let err = producer.write(frame(90_000, false)).unwrap_err();
		assert!(matches!(err, crate::Error::TimestampRewind(_)));
	}

	#[tokio::test]
	async fn b_frames_within_a_group_are_accepted() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		for (index, timestamp) in [0, 66_000, 33_000].into_iter().enumerate() {
			producer.write(frame(timestamp, index == 0)).unwrap();
		}
		let _finished = producer.finish().unwrap();
		let groups = collect_payloads(consumer).await;
		assert_eq!(groups[0][..3], [(0, 2), (66_000, 2), (33_000, 2)]);
	}

	#[tokio::test]
	async fn open_gop_leading_pictures_above_the_previous_group_are_accepted() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		producer.write(frame(0, true)).unwrap();
		producer.write(frame(33_000, false)).unwrap();
		producer.write(frame(66_000, true)).unwrap();
		producer.write(frame(50_000, false)).unwrap();
		let _finished = producer.finish().unwrap();
		let groups = collect_payloads(consumer).await;
		assert_eq!(groups[1][0], (66_000, 2));
		assert_eq!(groups[1][1], (50_000, 2));
	}

	/// Random reordering, splices, and cuts: every frame of a group presents after every frame of
	/// the groups before it, and a frame is dropped only when it presents at or before a frame
	/// published ahead of it.
	#[test]
	fn groups_never_overlap_on_random_streams() {
		use proptest::prelude::*;
		use proptest::test_runner::{Config, TestRunner};

		let mut runner = TestRunner::new(Config {
			cases: 256,
			failure_persistence: None,
			..Default::default()
		});
		// Per frame: its offset from a 33ms cadence, whether it is a keyframe, and whether a cut
		// precedes it.
		let stream = proptest::collection::vec(
			(
				-150_000i64..150_000,
				proptest::bool::weighted(0.2),
				proptest::bool::weighted(0.03),
			),
			1..120,
		);
		runner
			.run(&stream, |frames| {
				let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
				let mut consumer = track.subscribe(replay());
				let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
				let mut accepted = Vec::new();
				for (index, (offset, keyframe, cut)) in frames.into_iter().enumerate() {
					if cut {
						producer.cut(None).unwrap();
					}
					let timestamp = (1_000_000 + 33_000 * index as i64 + offset) as u64;
					let written = producer.write(Frame {
						timestamp: Timestamp::from_micros(timestamp).unwrap(),
						payload: Bytes::copy_from_slice(&(index as u32).to_be_bytes()),
						keyframe,
						duration: None,
					});
					if written.is_ok() {
						accepted.push((index as u32, timestamp as u128, keyframe));
					}
				}
				let _finished = producer.finish().unwrap();

				let mut groups: Vec<Vec<(u32, u128)>> = Vec::new();
				let mut spans = Vec::new();
				futures::executor::block_on(async {
					while let Some(mut group) = consumer.recv_group().await.unwrap() {
						let mut frames = Vec::new();
						while let Some(frame) = group.read_frame().await.unwrap() {
							let decoded = hang::container::Frame::decode(frame.payload).unwrap();
							if let Ok(index) = <[u8; 4]>::try_from(decoded.payload.as_ref()) {
								frames.push((u32::from_be_bytes(index), decoded.timestamp.as_micros().unwrap()));
							}
						}
						groups.push(frames);
						spans.push((group.start, group.end()));
					}
				});

				// No group starts inside the span of a group that ended whole.
				for pair in spans.windows(2) {
					let ((_, end), (next, _)) = (pair[0], pair[1]);
					prop_assert!(end.is_none_or(|end| next >= end), "{spans:?}");
				}

				let mut ceiling = None;
				for group in &groups {
					for &(_, timestamp) in group {
						prop_assert!(ceiling.is_none_or(|ceiling| timestamp > ceiling), "{groups:?}");
					}
					ceiling = group.iter().map(|&(_, timestamp)| timestamp).max().max(ceiling);
				}

				// Published frames are the accepted ones in order, less those at or before a frame
				// published ahead of them, those after a dropped keyframe until one is published, and
				// the leading pictures of a keyframe after one of them was dropped.
				let published: Vec<(u32, u128)> = groups.concat();
				let mut next = published.iter().peekable();
				let (mut newest, mut skipping) = (None, false);
				let (mut latest, mut tainted) = (None, false);
				for &(index, timestamp, keyframe) in &accepted {
					if let Some(&(_, stamped)) = next.next_if(|&&(published, _)| published == index) {
						// A keyframe opening a group past a whole group's End is stamped there.
						prop_assert!(stamped >= timestamp && (keyframe || stamped == timestamp));
						newest = newest.max(Some(stamped));
						skipping &= !keyframe;
						if keyframe {
							(latest, tainted) = (Some(stamped), false);
						}
						prop_assert!(
							keyframe || !tainted || latest.is_none_or(|latest| timestamp >= latest),
							"frame {index} at {timestamp} kept after a leading picture of {latest:?} was dropped"
						);
					} else {
						let leading = tainted && !keyframe && latest.is_some_and(|latest| timestamp < latest);
						prop_assert!(
							newest.is_some_and(|newest| timestamp <= newest) || (skipping && !keyframe) || leading,
							"frame {index} at {timestamp} was dropped above {newest:?}"
						);
						skipping |= keyframe;
						tainted |= !keyframe && latest.is_some_and(|latest| timestamp < latest);
					}
				}
				prop_assert!(next.peek().is_none(), "published a frame never accepted");
				Ok(())
			})
			.unwrap();
	}

	#[tokio::test]
	async fn duration_marker_resets_after_discontinuity() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		producer.write(frame(1_000_000, true)).unwrap();
		producer.write(frame(1_010_000, false)).unwrap();
		producer.discontinuity().unwrap();
		producer.write(frame(1_020_000, true)).unwrap();
		producer.write(frame(1_040_000, false)).unwrap();
		let _finished = producer.finish().unwrap();
		let groups = collect_payloads(consumer).await;
		assert_eq!(groups.last().unwrap().last(), Some(&(1_060_000, 0)));
	}

	/// A capture that reopens at once resumes sooner than one frame after the break. Guessing
	/// the closing group's end from its cadence would put a duration marker past the resumed
	/// keyframe, which a consumer reads as a rewind and refuses.
	#[tokio::test]
	async fn a_prompt_resume_after_a_discontinuity_is_not_a_rewind() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let subscriber = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		producer.write(frame(1_000_000, true)).unwrap();
		producer.write(frame(1_040_000, false)).unwrap();
		producer.discontinuity().unwrap();
		// Resumed 10ms later, inside the 40ms cadence measured before the break.
		producer.write(frame(1_050_000, true)).unwrap();
		let _finished = producer.finish().unwrap();

		let mut consumer =
			crate::container::Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Video));
		let mut timestamps = Vec::new();
		while let Some(frame) = consumer.read_frame().await.unwrap() {
			timestamps.push(frame.timestamp.as_micros().unwrap());
		}
		assert_eq!(timestamps, [1_000_000, 1_040_000, 1_050_000]);
	}

	/// A published group as a copy receives it: its start, its frames, and its End.
	type Published = (u64, Vec<moq_net::frame::Frame>, Option<u64>);

	/// Every group `consumer` reads, with its frames and End.
	async fn collect_published(consumer: moq_net::track::Subscriber) -> Vec<Published> {
		// Key order, oldest first, whatever the subscription's Group Order.
		let mut consumer = consumer.ordered();
		let mut groups = Vec::new();
		while let Some(mut group) = consumer.next_group().await.unwrap() {
			let mut frames = Vec::new();
			while let Some(frame) = group.read_frame().await.unwrap() {
				frames.push(frame);
			}
			groups.push((group.start, frames, group.end()));
		}
		groups
	}

	/// Replay `groups` into a fresh copy in `order`, as a lite-07 relay creates what arrives,
	/// returning what the copy refused. A GROUP_END accounts for the starts inside its span, which
	/// the copy then refuses, as the relay's subscription does.
	fn copy(groups: &[Published], order: impl IntoIterator<Item = usize>) -> moq_net::Result<()> {
		let copy = track_producer("copy", hang::container::track_info(hang::catalog::PRIORITY.video));
		for index in order {
			let (start, frames, end) = &groups[index];
			let mut group = copy.create_group(moq_net::group::Info { start: *start })?;
			for frame in frames {
				group.write_frame(frame.timestamp, frame.payload.clone())?;
			}
			match end {
				Some(end) => {
					group.finish_at(*end)?;
					copy.skip_groups(start + 1..*end);
				}
				None => drop(group.finish()?),
			}
		}
		Ok(())
	}

	/// The marker a discontinuity publishes after a cut starts at the End the cut gave its group,
	/// outside that group's span, so a lite-07 copy takes it whichever arrives first. One a unit
	/// inside the span is refused when it arrives second. Media resuming right at the marker is
	/// stamped one unit past it.
	#[tokio::test]
	async fn a_marker_after_a_cut_lands_outside_the_span_it_ended() {
		for (end, at) in [(Some(25_000), 25_000), (None, 20_000)] {
			let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
			let consumer = track.subscribe(replay());
			let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
			producer.write(frame(0, true)).unwrap();
			producer.write(frame(10_000, false)).unwrap();
			producer.cut(end.map(|us| Timestamp::from_micros(us).unwrap())).unwrap();
			producer.discontinuity().unwrap();
			producer.write(frame(at, true)).unwrap();
			producer.write(frame(at + 10_000, false)).unwrap();
			let _finished = producer.finish().unwrap();

			let groups = collect_published(consumer).await;
			let starts: Vec<_> = groups.iter().map(|(start, _, _)| *start).collect();
			assert_eq!(starts, [0, at, at + 1], "cut({end:?})");
			assert_eq!(groups[0].2, Some(at), "the cut ends its group whole");
			let resumed = groups[2].1[0].timestamp.as_micros().unwrap();
			assert_eq!(
				resumed,
				u128::from(at + 1),
				"the resumed keyframe is stamped past the marker"
			);

			for order in [[0, 1, 2], [1, 0, 2], [2, 1, 0]] {
				copy(&groups, order).unwrap_or_else(|err| panic!("cut({end:?}) in {order:?}: {err}"));
			}
			let inside = [groups[0].clone(), (at - 1, Vec::new(), None)];
			assert!(
				copy(&inside, [0, 1]).is_err(),
				"a marker inside the span arriving second"
			);
		}
	}

	/// An explicit end below a frame the group holds is refused for every kind, without closing
	/// the group, rather than closing it without the End it meant.
	#[tokio::test]
	async fn a_cut_below_a_written_frame_is_refused_for_every_kind() {
		use crate::container::Kind;
		let cases: [(Kind, &[u64], u64, u64); 4] = [
			(Kind::Audio, &[0, 20_000], 10_000, 40_000),
			(Kind::Data, &[0, 20_000], 10_000, 20_000),
			(Kind::Video, &[0, 40_000], 20_000, 80_000),
			// Reordered: the newest frame presents before the last one decoded.
			(Kind::Video, &[0, 120_000, 40_000, 80_000], 100_000, 120_000),
		];
		for (kind, timestamps, below, end) in cases {
			let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
			let consumer = track.subscribe(replay());
			let mut producer = Producer::new(track, Container::Legacy(kind));
			for (index, timestamp) in timestamps.iter().enumerate() {
				producer.write(frame(*timestamp, index == 0)).unwrap();
			}
			let refused = producer.cut(Some(Timestamp::from_micros(below).unwrap()));
			assert!(
				matches!(refused, Err(crate::Error::InvalidEnd(_))),
				"{kind:?}: {refused:?}"
			);
			assert!(!producer.needs_keyframe(), "{kind:?}: the group stays open");
			producer.cut(Some(Timestamp::from_micros(end).unwrap())).unwrap();
			let _finished = producer.finish().unwrap();

			let groups = collect_published(consumer).await;
			assert_eq!(groups.len(), 1, "{kind:?}");
			assert_eq!(
				groups[0].2,
				Some(end),
				"{kind:?}: the group ends whole where it was cut"
			);
		}
	}

	/// A keyframe past the newest frame but below the End a cut estimated from the cadence, as a
	/// faster cadence or a duration rounded up overshoots, starts at that End rather than inside
	/// the span it closed, and the rest of its group follows.
	#[tokio::test]
	async fn a_keyframe_below_an_estimated_end_starts_at_it() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		producer.write(frame(100_000, true)).unwrap();
		producer.write(frame(166_000, false)).unwrap();
		producer.cut(None).unwrap();
		producer.write(frame(200_000, true)).unwrap();
		producer.write(frame(233_000, false)).unwrap();
		producer.write(frame(266_000, false)).unwrap();
		let _finished = producer.finish().unwrap();

		let groups = collect_published(consumer).await;
		assert_eq!(
			groups[0].2,
			Some(232_000),
			"the cut estimates one 66ms frame past the last"
		);
		assert_eq!(groups[1].0, 232_000, "the keyframe starts at the End");
		let decoded: Vec<_> = (groups[1].1.iter())
			.map(|frame| hang::container::Frame::decode(frame.payload.clone()).unwrap())
			.filter(|frame| !frame.payload.is_empty())
			.map(|frame| frame.timestamp.as_micros().unwrap())
			.collect();
		assert_eq!(decoded, [232_000, 233_000, 266_000]);
		copy(&groups, [1, 0]).unwrap();
	}

	/// A source whose stated durations overshoot its spacing moves each keyframe at most by that
	/// overshoot: the next End is estimated from where the source put the frame, not where its
	/// group start moved it, so the moves never add up into a rewind. Past twice the spacing, a
	/// keyframe at or below the newest frame is dropped rather than refused.
	#[tokio::test]
	async fn moved_keyframes_never_add_up_across_groups() {
		for (duration, starts) in [
			(144_000, vec![0, 144_000, 244_000, 344_000, 444_000, 544_000]),
			(250_000, vec![0, 250_000, 350_000, 550_000]),
		] {
			let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
			let consumer = track.subscribe(replay());
			let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Audio));
			for i in 0..6 {
				let frame = Frame {
					duration: Some(Timestamp::from_micros(duration).unwrap()),
					..frame(i * 100_000, true)
				};
				producer.write(frame).unwrap();
				producer.cut(None).unwrap();
			}
			let _finished = producer.finish().unwrap();

			let groups = collect_published(consumer).await;
			let published: Vec<_> = groups.iter().map(|(start, _, _)| *start).collect();
			assert_eq!(published, starts, "{duration}us frames every 100ms");
			copy(&groups, (0..groups.len()).rev()).unwrap();
		}
	}

	/// Parameter sets written as a keyframe ahead of their picture at one timestamp move with it:
	/// the picture continues the group the parameter sets opened, at the same moved start.
	#[tokio::test]
	async fn a_frame_at_a_moved_keyframes_timestamp_moves_with_it() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		producer.write(frame(100_000, true)).unwrap();
		producer.write(frame(166_000, false)).unwrap();
		producer.cut(None).unwrap();
		// The parameter sets, then their picture.
		producer.write(frame(200_000, true)).unwrap();
		producer.write(frame(200_000, true)).unwrap();
		producer.write(frame(233_000, false)).unwrap();
		let _finished = producer.finish().unwrap();

		assert_eq!(
			media(collect_payloads(consumer).await),
			[vec![100_000, 166_000], vec![232_000, 232_000, 233_000]]
		);
	}

	/// A seek can't key a group at or below a frame the track holds, nor inside the span of a group
	/// that ended whole, as the group it closes does when its last frame passes the seek: the group
	/// starts at the first start past both instead, as a keyframe would. One that doesn't pass the
	/// latest group's start is a rewind.
	#[tokio::test]
	async fn a_seek_inside_what_the_track_holds_starts_past_it() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		producer.write(frame(0, true)).unwrap();
		producer.write(frame(66_000, false)).unwrap();
		// The group ends whole one 66ms frame past its last, at 132ms, which holds the seek.
		producer.seek(50_000).unwrap();
		producer.write(frame(200_000, true)).unwrap();
		producer.write(frame(233_000, false)).unwrap();
		producer.seek(0).unwrap();
		let rewind = producer.write(frame(300_000, true)).unwrap_err();
		assert!(
			matches!(&rewind, crate::Error::TimestampRewind(rewind) if rewind.floor.as_micros().unwrap() == 132_001),
			"{rewind:?}"
		);
		let _finished = producer.finish().unwrap();

		let groups = collect_published(consumer).await;
		let spans: Vec<_> = groups.iter().map(|(start, _, end)| (*start, *end)).collect();
		assert_eq!(spans, [(0, Some(132_000)), (132_000, Some(266_000))]);
		copy(&groups, [1, 0]).unwrap();
	}

	/// A seek's start moved past the span of the group it closed takes a keyframe below that span's
	/// End along, as `write` moves one, so the keyframe never presents below its own group's start.
	#[tokio::test]
	async fn a_seek_moved_past_a_span_takes_its_keyframe_along() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		producer.write(frame(0, true)).unwrap();
		producer.write(frame(66_000, false)).unwrap();
		// The group ends whole at 132ms, past the seek and the keyframe after it.
		producer.seek(50_000).unwrap();
		producer.write(frame(100_000, true)).unwrap();
		// The picture its parameter sets precede goes with it.
		producer.write(frame(100_000, false)).unwrap();
		producer.write(frame(166_000, false)).unwrap();
		let _finished = producer.finish().unwrap();

		let groups = collect_published(consumer).await;
		let spans: Vec<_> = groups.iter().map(|(start, _, end)| (*start, *end)).collect();
		assert_eq!(spans[0], (0, Some(132_000)));
		assert_eq!(spans[1].0, 132_000);
		let timestamps: Vec<_> = groups[1]
			.1
			.iter()
			.map(|frame| frame.timestamp.as_micros().unwrap())
			.collect();
		assert_eq!(timestamps[..3], [132_000, 132_000, 166_000]);
		copy(&groups, [1, 0]).unwrap();
	}

	/// A seek below the latest group's start refuses the next keyframe even when that keyframe
	/// overlaps the frames written and would otherwise be dropped.
	#[tokio::test]
	async fn a_seek_below_the_latest_start_refuses_an_overlapping_keyframe() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		producer.write(frame(100_000, true)).unwrap();
		producer.write(frame(166_000, false)).unwrap();
		producer.seek(90_000).unwrap();
		// Past the group's start but below its newest frame, so dropped were the seek valid.
		let rewind = producer.write(frame(150_000, true)).unwrap_err();
		assert!(
			matches!(&rewind, crate::Error::TimestampRewind(rewind) if rewind.timestamp.as_micros().unwrap() == 90_000),
			"{rewind:?}"
		);
	}

	/// Published order is compared at the track's own precision: on a nanosecond track, a frame a
	/// nanosecond below its moved keyframe reorders the group though both share a microsecond.
	#[tokio::test]
	async fn a_frame_a_nanosecond_below_a_moved_keyframe_reorders_its_group() {
		let nanos = moq_net::Timescale::new(1_000_000_000).unwrap();
		let info = hang::container::track_info(hang::catalog::PRIORITY.video).with_timescale(nanos);
		let track = track_producer("test", info);
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Stamped);
		let at = |nanoseconds: u64, keyframe: bool| Frame {
			timestamp: Timestamp::new(nanoseconds, nanos).unwrap(),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe,
			duration: None,
		};
		producer.write(at(1_000_000, true)).unwrap();
		producer.write(at(1_500_000, false)).unwrap();
		producer.cut(Some(Timestamp::new(2_000_002, nanos).unwrap())).unwrap();
		// Below the End the cut set, so moved to it, while the frame after it stays a nanosecond
		// below.
		producer.write(at(1_900_000, true)).unwrap();
		producer.write(at(2_000_001, false)).unwrap();
		producer.cut(None).unwrap();
		let _finished = producer.finish().unwrap();

		let groups = collect_published(consumer).await;
		let timestamps: Vec<_> = groups[1].1.iter().map(|frame| frame.timestamp.value()).collect();
		assert_eq!(timestamps, [2_000_002, 2_000_001]);
		assert_eq!(groups[1].2, None, "a reordered group ends without an estimated End");
	}

	/// A frame after a moved keyframe that publishes below it reorders the group as published, so
	/// the group's presentation end no longer bounds its decode-order tail and no marker says so.
	#[tokio::test]
	async fn a_frame_published_below_a_moved_keyframe_reorders_its_group() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		producer.write(frame(0, true)).unwrap();
		producer.write(frame(100_000, false)).unwrap();
		producer.cut(Some(Timestamp::from_micros(232_000).unwrap())).unwrap();
		// Below the End the cut set, so moved to it, while the frame after it stays.
		producer.write(frame(200_000, true)).unwrap();
		producer.write(frame(220_000, false)).unwrap();
		producer.cut(None).unwrap();
		let _finished = producer.finish().unwrap();

		let groups = collect_payloads(consumer).await;
		assert_eq!(groups[1], vec![(232_000, 2), (220_000, 2)]);
	}

	/// A seek keys the next group itself, so the group it closes ends whole at the estimate from its
	/// cadence only when that doesn't pass the seek's start, and at the start otherwise.
	#[tokio::test]
	async fn a_seek_caps_the_estimated_end_at_its_start() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		producer.write(frame(0, true)).unwrap();
		producer.write(frame(66_000, false)).unwrap();
		// One 66ms frame past the last is 132ms, past the seek.
		producer.seek(100_000).unwrap();
		producer.write(frame(100_000, true)).unwrap();
		producer.write(frame(133_000, false)).unwrap();
		// 166ms leaves a gap before the seek.
		producer.seek(500_000).unwrap();
		producer.write(frame(500_000, true)).unwrap();
		let _finished = producer.finish().unwrap();

		let groups = collect_published(consumer).await;
		let spans: Vec<_> = groups.iter().map(|(start, _, end)| (*start, *end)).collect();
		assert_eq!(spans[..2], [(0, Some(100_000)), (100_000, Some(166_000))]);
		copy(&groups, [1, 0, 2]).unwrap();
	}

	/// An open GOP's leading pictures present after every frame of the group before theirs but
	/// before their keyframe, so inside the span of that group, which ends whole at the keyframe.
	/// That breaks nothing: each span holds no other group's start, and every frame of a group
	/// presents before every frame of a higher one.
	#[tokio::test]
	async fn open_gop_spans_hold_no_other_start() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		// Decode order, in milliseconds: each keyframe's leading pictures reference the P-frame
		// before it.
		let decode_order = [
			(0, true),
			(90, false),
			(30, false),
			(60, false),
			(180, false),
			(120, false),
			(150, false),
			(270, true),
			(210, false),
			(240, false),
			(360, false),
			(300, false),
			(330, false),
			(450, true),
			(390, false),
		];
		for (millis, keyframe) in decode_order {
			producer.write(frame(millis * 1_000, keyframe)).unwrap();
		}
		let _finished = producer.finish().unwrap();

		let groups = collect_published(consumer).await;
		let starts: Vec<_> = groups.iter().map(|(start, _, _)| *start).collect();
		assert_eq!(starts, [0, 270_000, 450_000]);
		assert_eq!(groups[0].2, Some(270_000), "a group ends whole at the next keyframe");
		for (start, _, end) in &groups {
			let Some(end) = end else { continue };
			assert!(
				!starts.iter().any(|other| other > start && other < end),
				"[{start}, {end}) holds another start: {starts:?}"
			);
		}
		let presented: Vec<Vec<u128>> = (groups.iter())
			.map(|(_, frames, _)| {
				frames
					.iter()
					.map(|frame| frame.timestamp.as_micros().unwrap())
					.collect()
			})
			.collect();
		assert_eq!(presented[1], [270_000, 210_000, 240_000, 360_000, 300_000, 330_000]);
		for pair in presented.windows(2) {
			let (lower, higher) = (pair[0].iter().max().unwrap(), pair[1].iter().min().unwrap());
			assert!(lower < higher, "{presented:?}");
		}
		copy(&groups, [2, 1, 0]).unwrap();
	}
}
