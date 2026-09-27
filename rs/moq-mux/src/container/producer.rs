use bytes::Bytes;

use super::{Container, Frame, Kind};

fn add_micros(timestamp: moq_net::Timestamp, extra: moq_net::Timestamp) -> Option<moq_net::Timestamp> {
	let micros = timestamp.as_micros().saturating_add(extra.as_micros());
	let micros = u64::try_from(micros).ok()?;
	moq_net::Timestamp::from_micros(micros).ok()
}

fn timestamp_lt(left: moq_net::Timestamp, right: moq_net::Timestamp) -> bool {
	left.as_micros() < right.as_micros()
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
/// [`discontinuity`](Self::discontinuity) publishes a marker group of one empty frame, for
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
	inner: moq_net::track::Producer,
	container: C,
	group: Option<moq_net::group::Producer>,
	buffer: Vec<Frame>,

	buffer_duration: std::time::Duration,

	/// Sequence to use for the next group opened by [`Self::write`].
	/// Set by [`Self::seek`] and consumed on the next group creation.
	pending_sequence: Option<u64>,

	/// Records each group open (sequence + keyframe timestamp) into this rendition's
	/// timeline track, when the producer was built with one.
	recorder: Option<crate::timeline::Recorder>,

	/// The furthest presentation point written, i.e. `max(timestamp + duration)`. Reported to
	/// `recorder` on each [`cut`](Self::cut), since the last group of a track has no successor
	/// to bound it and its segment would otherwise be published a group short. Also the base
	/// for a duration marker when the caller does not pass a bound.
	end: Option<moq_net::Timestamp>,

	/// Exclusive presentation end of finished groups. A frame below this is refused.
	live_edge: Option<moq_net::Timestamp>,

	/// Duration of the frame that last raised [`end`](Self::end), if it had one. Distinguishes
	/// an exclusive presentation point from a max timestamp that still needs an estimate.
	last_duration: Option<moq_net::Timestamp>,

	/// Previous timestamp within the group and cadence observed within this epoch.
	previous_timestamp: Option<moq_net::Timestamp>,
	cadence: Option<moq_net::Timestamp>,
	/// A presentation endpoint cannot bound the decode-order tail after reordering.
	reordered: bool,

	/// Measures bitrate, reorder, batch, and explicit encoder flush observations for the catalog.
	/// A caller without a published rendition simply never reads the estimate.
	estimator: crate::catalog::Estimator,

	/// Peak-hold claim on the connection allocator, when one was supplied.
	/// Named `bandwidth` so it is not confused with the catalog-gate [`Reserved`].
	bandwidth: Option<crate::catalog::Claim>,

	/// The catalog rendition this media producer owns, when it was created through a catalog.
	rendition: Option<Box<dyn Rendition<R>>>,
}

trait Rendition<R>: Send {
	fn name(&self) -> &str;
	fn timestamp(&self, hint: Option<moq_net::Timestamp>) -> crate::Result<moq_net::Timestamp>;
	fn set(&mut self, config: R) -> crate::Result<()>;
	fn config(&self) -> crate::Result<R>;
	fn replace(&mut self, config: R) -> crate::Result<()>;
	fn estimate(&mut self, estimate: crate::catalog::Estimate) -> crate::Result<()>;
}

impl<E, R> Rendition<R> for crate::catalog::tracks::Rendition<E, R>
where
	E: crate::catalog::hang::CatalogExt,
	R: crate::catalog::RenditionConfig<E>,
{
	fn name(&self) -> &str {
		self.name()
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

	fn estimate(&mut self, estimate: crate::catalog::Estimate) -> crate::Result<()> {
		self.estimate(estimate)
	}
}

impl<C: Container> Producer<C> {
	/// Create a Producer wrapping the given moq-lite producer, muxing into `container`.
	///
	/// A plain media track by default: no buffering, no timeline. Add buffering with
	/// [`with_buffer`](Self::with_buffer); the timeline recorder is wired by the catalog (see
	/// [`catalog::Producer::video`](crate::catalog::Producer::video) and its sibling APIs.
	pub fn new(track: moq_net::track::Producer, container: C) -> Self {
		Self {
			inner: track,
			container,
			group: None,
			buffer: Vec::new(),
			buffer_duration: std::time::Duration::ZERO,
			pending_sequence: None,
			recorder: None,
			end: None,
			live_edge: None,
			last_duration: None,
			previous_timestamp: None,
			cadence: None,
			reordered: false,
			estimator: crate::catalog::Estimator::new(),
			bandwidth: None,
			rendition: None,
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
			pending_sequence: None,
			recorder: None,
			end: None,
			live_edge: None,
			last_duration: None,
			previous_timestamp: None,
			cadence: None,
			reordered: false,
			estimator: rendition.estimator(),
			bandwidth: None,
			rendition: Some(Box::new(rendition)),
		}
	}

	/// Publish or replace this track's catalog config.
	pub fn set(&mut self, config: R) -> crate::Result<()> {
		self.rendition.as_mut().ok_or(crate::Error::NotPublished)?.set(config)
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

	/// Resolve a timestamp on the broadcast's shared clock.
	pub fn timestamp(&self, hint: Option<moq_net::Timestamp>) -> crate::Result<moq_net::Timestamp> {
		self.rendition
			.as_ref()
			.ok_or(crate::Error::NotPublished)?
			.timestamp(hint)
	}

	fn publish_estimate(&mut self) -> crate::Result<()> {
		if let Some(rendition) = self.rendition.as_mut() {
			rendition.estimate(self.estimator.estimate())?;
		}
		Ok(())
	}

	/// Record when a locally encoded frame reached the transport. Imported media must leave
	/// this clock observation out and rely on its container batch and reorder measurements.
	pub fn flush(&mut self, timestamp: moq_net::Timestamp, now: std::time::Instant) -> crate::Result<()> {
		self.estimator.flush(timestamp, now);
		self.publish_estimate()
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

	/// The jitter and bitrate measured from the frames written so far.
	///
	/// A catalog-owned producer publishes this automatically. See
	/// [`Estimator`](crate::catalog::Estimator).
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

	/// Buffer up to `duration` of frames into each container frame.
	///
	/// When non-zero, frames are buffered and flushed together once the buffered duration exceeds
	/// it, or a keyframe arrives, packing multiple samples into one container frame (e.g. a CMAF
	/// moof+mdat). Zero (the default) flushes each frame immediately.
	///
	/// This is the publisher-side counterpart to the consumer's max age, and the one
	/// knob here that genuinely *adds* delay.
	pub fn with_buffer(mut self, duration: std::time::Duration) -> Self {
		self.buffer_duration = duration;
		self
	}

	/// Report each group open (sequence, timestamp, keyframe) through `recorder`, enrolling
	/// this track in the broadcast's timeline so consumers can index the media without
	/// downloading it.
	///
	/// Mint the recorder from the broadcast's [`timeline::Producer`](crate::timeline::Producer).
	pub fn with_recorder(mut self, recorder: crate::timeline::Recorder) -> Self {
		self.recorder = Some(recorder);
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
	/// A later [`write`](Self::write) below this is refused. Trusted sources that must
	/// re-anchor (a PES resync, a capture restart) clamp to it rather than rewind.
	pub fn live_edge(&self) -> Option<moq_net::Timestamp> {
		self.live_edge
	}

	/// The lowest timestamp the next [`write`](Self::write) accepts: the live edge, or for a
	/// keyframe, the edge once the group it closes is counted too.
	pub(crate) fn floor(&self, keyframe: bool) -> Option<moq_net::Timestamp> {
		match (self.live_edge, self.end.filter(|_| keyframe)) {
			(Some(edge), Some(end)) if timestamp_lt(edge, end) => Some(end),
			(edge, end) => edge.or(end),
		}
	}

	/// Write a frame to the track.
	///
	/// A keyframe closes any open group and starts a new one. A non-keyframe extends the current
	/// group; if no group is open it returns [`MissingKeyframe`](super::MissingKeyframe), so a caller
	/// joining mid-stream can skip frames until the first keyframe. A source where every frame is
	/// independently decodable (audio) marks only the first frame of each group a keyframe (see
	/// [`needs_keyframe`](Self::needs_keyframe)) so the group spans more than one frame.
	///
	/// A timestamp below the live edge earlier groups reached returns
	/// [`TimestampRewind`](super::TimestampRewind) without writing, the way an oversized
	/// frame is refused. B-frames and open-GOP leading pictures still qualify when they sit
	/// above that edge.
	pub fn write(&mut self, frame: Frame) -> crate::Result<()> {
		// A keyframe cuts the previous group, using its timestamp as the boundary
		// where the previous group's content ends. Cut first so this group's live
		// edge includes what we just closed, then refuse a rewind against that.
		if frame.keyframe {
			let rewound = self
				.previous_timestamp
				.is_some_and(|previous| timestamp_lt(frame.timestamp, previous));
			self.cut((!rewound).then_some(frame.timestamp))?;
			if rewound {
				self.cadence = None;
			}
		}

		if self.live_edge.is_some_and(|edge| timestamp_lt(frame.timestamp, edge)) {
			return Err(super::TimestampRewind.into());
		}

		// Start a new group if needed; the first frame of a group must be a keyframe.
		if self.group.is_none() {
			if !frame.keyframe {
				// No group yet and this delta can't anchor one. The caller (e.g. a
				// mid-stream join) decides whether to skip until the first keyframe.
				return Err(super::MissingKeyframe.into());
			}
			let group = match self.pending_sequence.take() {
				Some(sequence) => self.inner.create_group(moq_net::group::Info { sequence })?,
				None => self.inner.append_group()?,
			};

			// Report the group the moment it opens: its start is this frame's timestamp. The
			// timeline absorbs publish failures itself (it is an optional sidecar),
			// so reporting can't abort the media write.
			if let Some(recorder) = self.recorder.as_mut() {
				recorder.record(group.sequence, frame.timestamp, frame.keyframe);
			}

			self.group = Some(group);
		}

		// Buffer or write the frame.
		if self.buffer_duration.is_zero() {
			let group = self.group.as_mut().unwrap();
			let (timestamp, duration, bytes) = (frame.timestamp, frame.duration, frame.payload.len());
			self.container.write(group, &[frame])?;

			// Only what the container accepted is measured. A rejected frame (too large for the
			// group, a timestamp that won't convert) leaves the producer usable, and the estimate's
			// extrema never fall, so counting one would inflate the catalog for good.
			self.estimator.write(timestamp, bytes);
			self.observe_end(timestamp, duration);
		} else {
			// Buffered frames are measured on the way in instead. The flush that eventually writes
			// them takes the track down with it when it fails, so there is nothing to unwind.
			self.estimator.write(frame.timestamp, frame.payload.len());
			self.observe_end(frame.timestamp, frame.duration);
			self.buffer.push(frame);

			// Flush if the buffered span has reached the buffer duration. Compute
			// min/max across the buffer rather than first/last: frames within a track
			// are in *decode* order, and B-frames have non-monotonic PTS, so
			// `last - first` can shrink as a B-frame lands between two earlier-PTS
			// frames. The min/max pair captures the actual presentation span.
			if self.buffer.len() >= 2 {
				let mut iter = self.buffer.iter().map(|f| std::time::Duration::from(f.timestamp));
				let first = iter.next().unwrap();
				let (min, max) = iter.fold((first, first), |(min, max), d| (min.min(d), max.max(d)));
				if max.saturating_sub(min) >= self.buffer_duration {
					self.flush_buffer(None)?;
				}
			}
		}

		Ok(())
	}

	/// Cut the current group, flushing buffered frames and closing it.
	///
	/// `end` bounds the final buffered frame when the publisher knows where the
	/// group's content stops. A video track that writes duration markers appends an
	/// empty frame at that bound, or at the last timestamp plus its estimated
	/// duration. Reordered groups omit this marker because their presentation end
	/// does not bound the last frame in decode order. The next [`write`](Self::write)
	/// must be a keyframe. An explicit bound before the last ordered video frame
	/// returns [`InvalidEnd`](super::InvalidEnd) without flushing or closing the group.
	pub fn cut(&mut self, end: Option<moq_net::Timestamp>) -> crate::Result<()> {
		let marker_at = end.or_else(|| self.estimated_end());
		self.close(end, marker_at)
	}

	/// Close the current group, ending a video track's group with a duration marker at `marker_at`.
	fn close(&mut self, end: Option<moq_net::Timestamp>, marker_at: Option<moq_net::Timestamp>) -> crate::Result<()> {
		if self.container.kind() == Kind::Video
			&& !self.reordered
			&& let Some((end, previous)) = end.zip(self.previous_timestamp)
			&& timestamp_lt(end, previous)
		{
			return Err(super::InvalidEnd.into());
		}

		// Before the flush, which can fail: an unbounded cut leaves the measurement open to fold
		// into the next group anyway, so cutting often costs the catalog nothing.
		self.estimator.cut(end);
		self.claim();

		// Tell the timeline where this group's content stops: the duration marker when we
		// write one, else the caller's bound, else the furthest point we wrote.
		if let Some(recorder) = self.recorder.as_mut()
			&& let Some(end) = marker_at.or(end).max(self.end)
		{
			recorder.end(end);
		}

		let tail_end = marker_at.filter(|_| !self.reordered);
		self.flush_buffer(tail_end)?;
		if let Some(group) = self.group.as_mut() {
			self.container.finish_group(group, tail_end)?;
		}
		if let Some(group) = self.group.take() {
			group.finish()?;
		}
		if let Some(end) = self.end {
			self.raise_live_edge(end);
		}
		self.end = None;
		self.last_duration = None;
		self.previous_timestamp = None;
		self.reordered = false;
		self.publish_estimate()?;
		Ok(())
	}

	fn raise_live_edge(&mut self, timestamp: moq_net::Timestamp) {
		if self.live_edge.is_none_or(|edge| !timestamp_lt(timestamp, edge)) {
			self.live_edge = Some(timestamp);
		}
	}

	/// Raise the furthest presentation point written, for [`cut`](Self::cut) to report.
	fn observe_end(&mut self, timestamp: moq_net::Timestamp, duration: Option<moq_net::Timestamp>) {
		self.reordered |= self.previous_timestamp.is_some_and(|previous| timestamp < previous);
		if let Some(previous) = self.previous_timestamp
			&& let Ok(delta) = timestamp.checked_sub(previous)
			&& !delta.is_zero()
		{
			self.cadence = Some(delta);
		}
		self.previous_timestamp = Some(timestamp);

		// Timestamp and duration can be at different scales, so add them in micros; the
		// sub-microsecond rounding that costs is far below a segment boundary.
		let micros = timestamp.as_micros() + duration.map(|d| d.as_micros()).unwrap_or(0);
		let Ok(micros) = u64::try_from(micros) else { return };
		let Ok(end) = moq_net::Timestamp::from_micros(micros) else {
			return;
		};
		if self.end.is_none_or(|prev| end >= prev) {
			self.end = Some(end);
			self.last_duration = duration.filter(|duration| !duration.is_zero());
		}
	}

	/// Exclusive group end from a known duration or the observed sample cadence.
	fn estimated_end(&self) -> Option<moq_net::Timestamp> {
		let last = self.end?;
		if self.last_duration.is_some() {
			return Some(last);
		}
		add_micros(last, self.cadence?)
	}

	/// Close the current group (if any) and open the next group at the given sequence.
	///
	/// The next [`write`](Self::write) must be a keyframe and will land in a group with
	/// `sequence`. Useful for joining mid-stream.
	pub fn seek(&mut self, sequence: u64) -> crate::Result<()> {
		self.cut(None)?;
		self.pending_sequence = Some(sequence);
		Ok(())
	}

	/// Publish a marker group standing for a break in the timeline: content stopped, and
	/// whatever comes next does not continue it.
	///
	/// Call this whenever the timeline is about to jump -- pausing an encoder, switching
	/// source, resuming on a re-anchored clock. Without it a break is invisible: the next
	/// group looks exactly like the one that would have followed, and a consumer bounding a
	/// sample by the next group's first frame hands it the entire gap as its duration. That
	/// produced a 2405 second video sample out of a publisher that had been paused 40 minutes
	/// (moq-dev/moq.pro#814). Consecutive sequence numbers can't rule a pause out, so this
	/// marker is the only thing that can say one happened.
	///
	/// It also fixes what a subscriber joining mid-break sees. A subscription starts at the
	/// track's latest group, and creating this one advances that -- so a late joiner lands on
	/// the marker and waits for real media, instead of being served the group from *before*
	/// the break as though it were live.
	///
	/// Audio and video write one empty frame at the exclusive end of the previous epoch.
	/// That object exists on moq-transport, so the live edge moves immediately; a consumer
	/// MUST NOT submit it to a decoder. Data tracks skip a sequence with no object, because
	/// an empty payload is data. The next [`write`](Self::write) opens the group after the
	/// marker and must continue forward from the live edge; it cannot rewind.
	///
	/// To bound the closing group's final frame, [`cut(end)`](Self::cut) before calling this.
	/// Otherwise the open group closes without a duration marker: what resumes may land sooner
	/// than one estimated frame later (a capture that reopens at once), and a guessed end past
	/// it would read as a rewind to every consumer.
	pub fn discontinuity(&mut self) -> crate::Result<()> {
		self.close(None, None)?;
		// Nothing is measured across the break: the frames still open on this side have no end, and
		// the gap to the far side is not a frame duration.
		self.estimator.discontinuity();
		self.cadence = None;
		if self.container.kind() == Kind::Data {
			// Empty is payload on a data track, so skip this sequence with no object.
			let skipped = match self.pending_sequence.take() {
				Some(sequence) => sequence,
				None => self.inner.latest().map_or(0, |s| s + 1),
			};
			self.pending_sequence = Some(skipped + 1);
			return Ok(());
		}
		let mut group = match self.pending_sequence.take() {
			Some(sequence) => self.inner.create_group(moq_net::group::Info { sequence })?,
			None => self.inner.append_group()?,
		};
		let timestamp = self.live_edge.unwrap_or(moq_net::Timestamp::ZERO);
		if let Some(recorder) = self.recorder.as_mut() {
			recorder.record(group.sequence, timestamp, false);
			recorder.end(timestamp);
		}
		self.container.write(
			&mut group,
			&[Frame {
				timestamp,
				payload: Bytes::new(),
				keyframe: false,
				duration: None,
			}],
		)?;
		group.finish()?;
		Ok(())
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
				&& let Ok(boundary) = boundary.convert(self.buffer[i].timestamp.scale())
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
		self.buffer.clear();

		Ok(())
	}

	/// Finish the track, flushing any buffered frames and closing any open group.
	pub fn finish(&mut self) -> crate::Result<()> {
		self.cut(None)?;
		self.inner.finish()?;
		self.publish_estimate()?;
		Ok(())
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
	fn replay() -> moq_net::track::Subscription {
		moq_net::track::Subscription::default().with_max_age(RECORDING_MAX_AGE)
	}

	/// The media track's full retention window, so readers started after publishing
	/// can still consume every retained group.
	const RECORDING_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(30);

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
		producer.finish().unwrap();

		let estimate = producer.estimate();
		assert_eq!(estimate.jitter, None, "PTS spacing alone is not flush delay");
		assert_eq!(estimate.bitrate, Some(1_600_000));
	}

	#[test]
	fn catalog_flush_measures_delay_across_renditions_and_jitter_within_each() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut tracks = Vec::new();
		for name in ["fast", "slow"] {
			let net = broadcast
				.create_track(name, catalog.track_info(hang::catalog::PRIORITY.video))
				.unwrap();
			let config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
			let mut track = catalog
				.video(net, Container::Legacy(crate::container::Kind::Video), config)
				.unwrap();
			track.write(frame(0, true)).unwrap();
			tracks.push(track);
		}

		let anchor = std::time::Instant::now();
		let ms = std::time::Duration::from_millis;
		let pts = |millis: u64| Timestamp::from_micros(millis * 1_000).unwrap();
		tracks[0].flush(pts(0), anchor).unwrap();
		// A constant 200ms offset behind the other rendition is delay, not jitter.
		tracks[1].flush(pts(0), anchor + ms(200)).unwrap();
		assert_eq!(catalog.snapshot().video.renditions["slow"].jitter, None);
		assert_eq!(catalog.snapshot().video.renditions["slow"].delay, Some(ms(200)));
		assert_eq!(catalog.snapshot().video.renditions["fast"].delay, None);

		// A frame flushed 60ms later than the slow rendition's own minimum is.
		tracks[1].flush(pts(40), anchor + ms(300)).unwrap();
		assert_eq!(catalog.snapshot().video.renditions["fast"].jitter, None);
		assert_eq!(catalog.snapshot().video.renditions["slow"].jitter, Some(ms(60)));
	}

	/// A passthrough producer claims nothing until the first window closes, then
	/// reserves that peak, raises the ceiling on a louder later window, and holds
	/// it when a quieter one follows.
	#[tokio::test]
	async fn a_passthrough_producer_ratchets_its_bandwidth_claim() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let _sub = track.consume();
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
		let _encoder_sub = encoder_track.consume();
		let encoder = allocator.reserve(&encoder_track.demand(), moq_net::bandwidth::Rate::from_bps(8_000_000));
		assert_eq!(
			encoder.peek(),
			Some(moq_net::bandwidth::Rate::from_bps(6_000_000)),
			"alone, the encoder takes the whole estimate"
		);

		let (_passthrough_broadcast, passthrough) = video_track();
		let _passthrough_sub = passthrough.consume();
		let mut producer =
			Producer::new(passthrough, Container::Legacy(crate::container::Kind::Data)).with_bandwidth(allocator);

		producer.write(sized_frame(0, true, 100_000)).unwrap();
		producer.write(sized_frame(500_000, false, 100_000)).unwrap();
		assert_eq!(
			encoder.peek(),
			Some(moq_net::bandwidth::Rate::from_bps(6_000_000)),
			"nothing claimed before the first window"
		);

		producer.cut(Some(Timestamp::from_micros(1_000_000).unwrap())).unwrap();
		assert_eq!(
			encoder.peek(),
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
		producer.finish().unwrap();

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
			match frames.iter().any(|f| f.timestamp.as_micros() == self.0 as u128) {
				true => Err(moq_net::Error::FrameTooLarge.into()),
				false => Ok(()),
			}
		}

		fn poll_read(
			&self,
			_group: &mut moq_net::group::Consumer,
			_waiter: &kio::Waiter,
		) -> std::task::Poll<Result<Option<Vec<Frame>>, Self::Error>> {
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
	async fn collect_groups(mut consumer: moq_net::track::Subscriber) -> Vec<usize> {
		let mut groups = Vec::new();
		while let Some(mut group) = consumer.recv_group().await.unwrap() {
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
		// The resumed clock jumps forty minutes, so both the retention window and the
		// drift budget have to cover it or the pre-discontinuity group reads as ancient.
		let discontinuity_max_age = std::time::Duration::from_secs(41 * 60);
		let info = hang::container::track_info(hang::catalog::PRIORITY.audio).with_max_age(discontinuity_max_age);
		let track = track_producer("test", info);
		let consumer = track.subscribe(moq_net::track::Subscription::default().with_max_age(discontinuity_max_age));
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Audio));

		producer.write(frame(0, true)).unwrap();
		producer.write(frame(10_000, false)).unwrap();
		producer.discontinuity().unwrap();
		// Resumed on a re-anchored clock, 40 minutes later.
		producer.write(frame(2_405_070_000, true)).unwrap();
		producer.finish().unwrap();

		let groups = collect_payloads(consumer).await;
		assert_eq!(groups.len(), 3);
		assert_eq!(groups[0], vec![(0, 2), (10_000, 2)]);
		assert_eq!(
			groups[1],
			vec![(10_000, 0)],
			"the marker is one empty frame at the exclusive end"
		);
		assert_eq!(groups[2][0], (2_405_070_000, 2));
	}

	/// A subscription starts at the track's LATEST group, and the marker advances it even
	/// though it carries nothing. So a subscriber arriving mid-break waits for real media
	/// rather than being handed the pre-break group as if it were live -- which is how a
	/// 40-minute-stale frame reached a VOD recording in moq-dev/moq.pro#814.
	#[tokio::test]
	async fn discontinuity_moves_the_live_edge_off_stale_content() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Audio));

		producer.write(frame(0, true)).unwrap();
		let stale = producer.track().latest();

		producer.discontinuity().unwrap();
		let edge = producer.track().latest();

		assert_ne!(edge, stale, "the marker group is the live edge now");
		assert_eq!(edge, stale.map(|s| s + 1));
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
		producer.finish().unwrap();

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
		producer.finish().unwrap();

		assert_eq!(collect_groups(consumer).await, vec![3, 2]);
	}

	/// Drain all groups, returning each group's (timestamp_micros, payload_len) pairs.
	async fn collect_payloads(mut consumer: moq_net::track::Subscriber) -> Vec<Vec<(u128, usize)>> {
		let mut groups = Vec::new();
		while let Some(mut group) = consumer.recv_group().await.unwrap() {
			let mut frames = Vec::new();
			while let Some(frame) = group.read_frame().await.unwrap() {
				let decoded = hang::container::Frame::decode(frame.payload).unwrap();
				frames.push((decoded.timestamp.as_micros(), decoded.payload.len()));
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
		producer.finish().unwrap();

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
		producer.finish().unwrap();

		assert_eq!(collect_payloads(consumer).await, vec![vec![(0, 2), (20_000, 2)]]);
	}

	/// LOC producers do not write the marker until skipping consumers have shipped.
	#[tokio::test]
	async fn loc_cut_writes_no_duration_marker() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Loc(crate::container::Kind::Video));

		producer.write(frame(0, true)).unwrap();
		producer
			.cut(Some(moq_net::Timestamp::from_micros(33_000).unwrap()))
			.unwrap();
		producer.finish().unwrap();

		let mut groups = Vec::new();
		let mut consumer = consumer;
		while let Some(mut group) = consumer.recv_group().await.unwrap() {
			let mut count = 0;
			while group.next_frame().await.unwrap().is_some() {
				count += 1;
			}
			groups.push(count);
		}
		assert_eq!(groups, vec![1], "LOC producers do not write the marker yet");
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
		producer.finish().unwrap();

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

	/// Drain all groups from a finished track, returning their sequence numbers.
	async fn collect_sequences(mut consumer: moq_net::track::Subscriber) -> Vec<u64> {
		let mut sequences = Vec::new();
		while let Some(group) = consumer.recv_group().await.unwrap() {
			sequences.push(group.sequence);
		}
		sequences
	}

	/// `seek(n)` opens the next group at sequence `n`.
	#[tokio::test]
	async fn seek_uses_explicit_sequence() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Data));

		producer.write(frame(0, true)).unwrap(); // seq 0
		producer.seek(42).unwrap();
		producer.write(frame(10_000, true)).unwrap(); // seq 42
		producer.finish().unwrap();

		assert_eq!(collect_sequences(consumer).await, vec![0, 42]);
	}

	/// `seek` is consumed on the next group creation; subsequent groups auto-increment from there.
	#[tokio::test]
	async fn seek_clears_pending_after_use() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Data));

		producer.seek(5).unwrap();
		producer.write(frame(0, true)).unwrap(); // seq 5
		producer.write(frame(10_000, true)).unwrap(); // seq 6 (auto-incremented)
		producer.finish().unwrap();

		assert_eq!(collect_sequences(consumer).await, vec![5, 6]);
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

		fn poll_read(
			&self,
			_group: &mut moq_net::group::Consumer,
			_waiter: &kio::Waiter,
		) -> std::task::Poll<Result<Option<Vec<Frame>>, Self::Error>> {
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
		producer.finish().unwrap();

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
		producer.finish().unwrap();

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

	#[tokio::test]
	async fn duration_marker_uses_cadence_not_batching_delay() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = track.subscribe(replay());
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		producer.burst(std::time::Duration::from_secs(1)).unwrap();
		producer.write(frame(0, true)).unwrap();
		producer.write(frame(20_000, false)).unwrap();
		producer.write(frame(40_000, false)).unwrap();
		producer.finish().unwrap();
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
		producer.finish().unwrap();
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
		producer.finish().unwrap();
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
		producer.finish().unwrap();
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
			tokio::time::timeout(std::time::Duration::from_millis(1), group.read_frame())
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
					.as_micros(),
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

	#[tokio::test]
	async fn a_group_below_the_live_edge_is_refused() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut producer = Producer::new(track, Container::Legacy(crate::container::Kind::Video));
		producer.write(frame(0, true)).unwrap();
		producer.write(frame(33_000, false)).unwrap();
		producer.cut(None).unwrap();
		let err = producer.write(frame(16_000, true)).unwrap_err();
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
		producer.finish().unwrap();
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
		producer.finish().unwrap();
		let groups = collect_payloads(consumer).await;
		assert_eq!(groups[1][0], (66_000, 2));
		assert_eq!(groups[1][1], (50_000, 2));
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
		producer.finish().unwrap();
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
		producer.finish().unwrap();

		let mut consumer =
			crate::container::Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Video));
		let mut timestamps = Vec::new();
		while let Some(frame) = consumer.read().await.unwrap() {
			timestamps.push(frame.timestamp.as_micros());
		}
		assert_eq!(timestamps, [1_000_000, 1_040_000, 1_050_000]);
	}
}
