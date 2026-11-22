//! Encode raw frames and publish them as a moq video track, following the uplink's grant.

use std::sync::Arc;
use std::task::Poll;

use moq_mux::catalog::hang::CatalogExt;
use moq_mux::rate;
use moq_net::bandwidth::Rate;

use super::Sink;
use super::encoder::{Codec, Config};
use super::publisher::{Finished, Publisher};
use crate::{Error, Frame};

/// Encode and publication policy for [`Producer`].
///
/// `#[non_exhaustive]`: construct via [`Options::new`] and set fields, so new knobs can be added
/// without breaking callers.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Options {
	/// Track name to publish under. `None` derives a unique one from the codec (`0.avc3`, then
	/// `1.avc3`, ...). Subscribers find it through the catalog either way.
	pub track: Option<String>,
	/// The encoder: the geometry every written frame has, the codec, the backend, and the
	/// [`bitrate`](Config::bitrate), which is the ceiling the grant never raises it past.
	pub config: Config,
	/// The connection's bandwidth, as an allocator over
	/// [`Session::send_bandwidth`](moq_net::Session::send_bandwidth) (or
	/// `moq_tokio::Connection::send_bandwidth`, which survives reconnects).
	///
	/// The track reserves its ceiling against it and, while someone watches, retunes the encoder
	/// to its share between frames, per the default [`rate::Policy`], so a closing uplink gets a
	/// softer picture instead of a stalled one. Pass the same allocator to every sender on the
	/// connection, audio included, so their bitrates sum to the uplink instead of each matching it.
	///
	/// Defaults to [`Allocator::unlimited`](moq_net::bandwidth::Allocator::unlimited), which holds
	/// the ceiling regardless of congestion.
	pub bandwidth: moq_net::bandwidth::Allocator,
	/// The next lower rendition of the same source, when this one is encoded beside it.
	///
	/// Makes this rendition a simulcast rung: while the grant can no longer fund it, it stops
	/// encoding, drops what is written to it, and advertises itself disabled, so the lower
	/// renditions keep their share, and it resumes on a keyframe once the grant recovers. It takes
	/// the track priority one below the lower rendition's, so the allocator fills that one first.
	/// Needs a [`bandwidth`](Self::bandwidth) to follow, and a ceiling above the lower one's.
	pub lower: Option<Rung>,
}

impl Options {
	/// Encode with `config`, holding its bitrate.
	pub fn new(config: Config) -> Self {
		Self {
			track: None,
			config,
			bandwidth: moq_net::bandwidth::Allocator::unlimited(),
			lower: None,
		}
	}
}

/// A rendition's place in a simulcast ladder, from [`Producer::rung`], which the next
/// rendition up takes as its [`Options::lower`].
///
/// Watches the rendition without keeping it publishing: once it finishes or drops, the rung
/// above has nothing to fall back to and never disables.
#[derive(Clone)]
pub struct Rung {
	ceiling: kio::Consumer<Rate>,
	priority: i8,
}

impl std::fmt::Debug for Rung {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Rung")
			.field("ceiling", &*self.ceiling.read())
			.field("priority", &self.priority)
			.finish()
	}
}

impl Rung {
	/// The rendition's current ceiling, or `None` once it ended. Arms `waiter` for either change.
	fn ceiling(&self, waiter: &kio::Waiter) -> Option<Rate> {
		match self.ceiling.poll(waiter, |_| Poll::<()>::Pending) {
			Poll::Pending => Some(*self.ceiling.read()),
			// The closure never completes, so this is the channel closing.
			Poll::Ready(_) => None,
		}
	}
}

/// What a [`Producer`]'s encoder is doing.
enum Encoding {
	/// Encoding every frame written.
	Open(Sink),
	/// Stopped by a [`discontinuity`](Producer::discontinuity) or a capture between sources: the
	/// next frame written reopens it.
	Idle,
	/// Stopped while the grant cannot fund this rung: frames written are dropped until it resumes.
	Disabled,
}

/// Encode raw [`Frame`]s and publish them as a moq video track (avc3 / hev1 by codec).
///
/// The counterpart of `moq_audio::encode::Producer`: it owns its encoder, registers the catalog
/// rendition at construction, probed from the encoder so a subscriber can discover the track
/// before a frame is written, and follows the [`bandwidth`](Options::bandwidth) grant between
/// frames. [`Publisher`] is the layer below, for frames you encoded yourself.
///
/// While nobody watches, a rendition with no [`lower`](Options::lower) one encodes at its
/// ceiling and ignores the grant: its groups wait in the cache rather than being paced to a
/// subscriber, and the next viewer's grant applies afresh. A rung keeps following the grant,
/// which reports the slice it would get back, so it resumes after its viewers left.
///
/// The `async` methods are not cancel-safe, as [`Sink`]'s are not: a cancelled one leaves the
/// encoder refusing every later call, so drop the producer instead.
pub struct Producer {
	publisher: Publisher,
	encoding: Encoding,
	/// The encoder settings, whose bitrate is the ceiling, to reopen with.
	config: Config,
	/// The ceiling, which caps every grant. A channel, so a rung above this rendition wakes when
	/// it moves and when this rendition ends.
	ceiling: kio::Producer<Rate>,
	/// The track priority, which a higher rung takes one below.
	priority: i8,
	lower: Option<Rung>,
	/// Reserves against the track a decoder identity change moves the rendition to.
	allocator: moq_net::bandwidth::Allocator,
	/// This rendition's claim on the track named by `demand`.
	reservation: Arc<moq_net::bandwidth::Reservation>,
	/// The track `reservation` claims on, which stays the published one until a frame moves it.
	demand: moq_net::track::Demand,
	/// `None` once retired: the grant ended for good, so the encoder holds its rate.
	grant: Option<rate::Grant>,
	/// What the encoder produces, or reopens at.
	target: rate::Target,
	/// Whether someone watched at the last write, so a rendition that never disables rests at
	/// its ceiling once nobody does.
	watched: bool,
	/// Whether nothing has been published since the last break marker, which a second break
	/// would only repeat.
	marked: bool,
	/// Whether a resume's encoder refused the funded rate, so this rung now resumes only at its
	/// full ceiling, which needs no retune, rather than overshoot a partial grant.
	ceiling_only: bool,
	/// The soonest a resume at the ceiling that failed is tried again, a grant's retry step later.
	resume_retry: Option<kio::clock::Instant>,
	/// Whether the grant is being followed with nothing written, so a resume re-enables the rung
	/// without opening an encoder nobody would feed.
	unwritten: bool,
}

impl Producer {
	/// Open the encoder and publish a track for it in `broadcast`, registering its rendition in
	/// `catalog` immediately.
	///
	/// The rendition comes from [`Config::probe`], so it says what the first keyframe will. An
	/// unsupported codec, resolution, or backend fails here, before the track exists.
	pub async fn new<E: CatalogExt>(
		broadcast: &moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::Producer<E>,
		options: Options,
	) -> Result<Self, Error> {
		let Options {
			track,
			config,
			bandwidth,
			lower,
		} = options;
		let ceiling = config.resolved_bitrate();
		if let Some(below) = lower.as_ref().and_then(|lower| lower.ceiling(&kio::Waiter::noop()))
			&& below >= ceiling
		{
			return Err(Error::UnsupportedConfig(format!(
				"the lower rendition's {} bps is not below this one's {} bps",
				below.as_bps(),
				ceiling.as_bps()
			)));
		}
		// Both before the track exists: a config this machine can't encode fails without leaving
		// a track advertised that will never carry frames. The probe closes its encoder before this
		// one opens, so only one codec session is live.
		let rendition = config.probe().await?;
		let sink = Sink::open(&config).await?;

		let priority = lower
			.as_ref()
			.map_or(hang::catalog::PRIORITY.video, |lower| lower.priority.saturating_sub(1));
		let info = catalog.track_info(priority);
		let track = match track {
			Some(name) => broadcast.create_track(name, info).ok_or(moq_net::Error::Duplicate)?,
			None => broadcast.unique_track(suffix(config.codec), info),
		};
		let mut producer = Self::with_track(track, &catalog, config, rendition, bandwidth, lower)?;
		producer.encoding = Encoding::Open(sink);
		Ok(producer)
	}

	/// Publish `rendition` on `track` with no encoder open yet: the next frame written, or
	/// [`open`](Self::open), opens one.
	pub(crate) fn with_track<E: CatalogExt>(
		track: moq_net::track::Producer,
		catalog: &moq_mux::catalog::Producer<E>,
		mut config: Config,
		rendition: hang::catalog::VideoConfig,
		allocator: moq_net::bandwidth::Allocator,
		lower: Option<Rung>,
	) -> Result<Self, Error> {
		let priority = track.info().priority;
		let publisher = Publisher::new(
			track,
			catalog.reserve().with_interval(config.effective_join()),
			rendition,
		)?;
		let ceiling = config.resolved_bitrate();
		config.bitrate = Some(ceiling);
		let demand = publisher.demand();
		let reservation = Arc::new(allocator.reserve(&demand, ceiling));
		let grant = rate::Grant::new(reservation.consume(), policy(ceiling, lower.as_ref()));
		publisher.report_bitrate(ceiling);
		Ok(Self {
			publisher,
			encoding: Encoding::Idle,
			config,
			ceiling: kio::Producer::new(ceiling),
			priority,
			lower,
			allocator,
			reservation,
			demand,
			grant: Some(grant),
			target: rate::Target::Enabled(ceiling),
			watched: false,
			marked: false,
			ceiling_only: false,
			resume_retry: None,
			unwritten: false,
		})
	}

	/// A watch-only handle to the published track's subscriber demand. A frame that changes the
	/// decoder identity, such as a resume at a ceiling past the declared level, moves the
	/// rendition to a newly named track, so read this again after [`write`](Self::write).
	pub fn demand(&self) -> moq_net::track::Demand {
		self.publisher.demand()
	}

	/// This rendition's place in a simulcast ladder, for the next rendition up to take as its
	/// [`Options::lower`].
	pub fn rung(&self) -> Rung {
		Rung {
			ceiling: self.ceiling.consume(),
			priority: self.priority,
		}
	}

	/// This track's claim on the [`bandwidth`](Options::bandwidth) allocator, whose
	/// [`rate`](moq_net::bandwidth::Reservation::rate) is the grant the encoder follows. A frame
	/// that moves the rendition to a new track claims on that one instead, so read this again
	/// after [`write`](Self::write).
	pub fn reservation(&self) -> &Arc<moq_net::bandwidth::Reservation> {
		&self.reservation
	}

	/// The bitrate the encoder produces at, or `None` while a rung is disabled.
	pub fn bitrate(&self) -> Option<Rate> {
		match self.target {
			rate::Target::Enabled(bitrate) => Some(bitrate),
			rate::Target::Disabled => None,
		}
	}

	/// Encode and publish one frame at its own timestamp, at the configured size.
	///
	/// Follows any grant change first, so the encoder retunes between frames rather than mid-
	/// encode. A backend that pipelines publishes an earlier frame's output here, so a call that
	/// publishes nothing is normal. While a rung is disabled, the frame is dropped unencoded.
	///
	/// Takes the frame as anything that can become an [`Arc`], like [`Sink::encode`], so a
	/// source fanning one frame out to a ladder hands each rung a clone of the handle.
	pub async fn write(&mut self, frame: impl Into<Arc<Frame>>) -> Result<(), Error> {
		self.follow().await;
		if let Encoding::Idle = self.encoding {
			let wanted = self.bitrate().unwrap_or(*self.ceiling.read());
			if let Some(sink) = self.reopen(wanted).await? {
				self.encoding = Encoding::Open(sink);
			}
		}
		let Encoding::Open(sink) = &mut self.encoding else {
			return Ok(());
		};
		sink.encode(frame).await?;
		self.publish_ready()
	}

	/// Cut a new group at the next written frame.
	///
	/// Optional: the encoder already keyframes every [`join`](Config::join), and each of those cuts
	/// a group. Reach for it to place a boundary the encoder cannot see, such as a scene change.
	/// Fails when the encoder cannot force a keyframe, leaving groups to that interval. A stopped
	/// encoder opens with a keyframe anyway, so this does nothing then.
	pub async fn cut(&mut self) -> Result<(), Error> {
		match &mut self.encoding {
			Encoding::Open(sink) => sink.cut().await,
			Encoding::Idle | Encoding::Disabled => Ok(()),
		}
	}

	/// Set the ceiling to `bitrate`, retuning the encoder to it from roughly the next frame. No
	/// keyframe is forced. The reservation moves with it, so a later grant never retunes above it.
	///
	/// Some backends cannot raise past the bitrate they opened at (openh264), so configure the
	/// highest you will ask for and adapt downwards. A refused rate, such as
	/// [`Error::BitrateUnsupported`] or openh264's raise past its opening rate, changes nothing:
	/// the encoder keeps encoding at its current rate. Only [`Error::CodecGone`] means it stopped.
	pub async fn set_bitrate(&mut self, bitrate: Rate) -> Result<(), Error> {
		if let Encoding::Open(sink) = &mut self.encoding {
			sink.set_bitrate(bitrate).await?;
		}
		if !matches!(self.encoding, Encoding::Disabled) {
			self.target = rate::Target::Enabled(bitrate);
			self.publisher.report_bitrate(bitrate);
		}
		self.config.bitrate = Some(bitrate);
		if let Ok(mut ceiling) = self.ceiling.write() {
			*ceiling = bitrate;
		}
		self.reservation.update(bitrate);
		// The grant follows from the rate the encoder applied itself, even when the ceiling is
		// unchanged and the policy with it, so a grant below it retunes back down.
		if let Some(grant) = self.grant.as_mut() {
			grant.set_policy(policy(bitrate, self.lower.as_ref()));
			if !matches!(self.encoding, Encoding::Disabled) {
				grant.assume(rate::Target::Enabled(bitrate));
			}
		}
		Ok(())
	}

	/// Mark a break in the published timeline: what is written next does not continue what came
	/// before.
	///
	/// Call this when the source stops rather than merely pausing between frames, such as a
	/// paused game or a source switch. The encoder drains into the track and closes, so what
	/// resumes opens a group with a keyframe from a fresh one. Errors when the drain fails, after
	/// the break is marked anyway: only those last frames are lost.
	pub async fn discontinuity(&mut self) -> Result<(), Error> {
		let drained = self.close().await;
		self.mark()?;
		drained
	}

	/// Drain the encoder into the track and finish it.
	///
	/// Consumes the producer, releasing the encoder and the bandwidth claim. The returned
	/// [`Finished`] keeps the track and its catalog rendition published, and can still
	/// [`abort`](Finished::abort) them. A drain that fails ends the track with an error instead,
	/// since a clean end promises a subscriber everything the publisher produced.
	pub async fn finish(self) -> Result<Finished, Error> {
		let Self {
			mut publisher,
			encoding,
			ceiling,
			reservation,
			..
		} = self;
		// The claim goes before the drain, so siblings can take the room while the last frames go
		// out, and the ceiling closes, so a rung above stops counting on this one now.
		drop((ceiling, reservation));
		let drained = match encoding {
			Encoding::Open(mut sink) => {
				async {
					sink.flush().await?;
					while let Some(encoded) = sink.receive()? {
						publisher.publish(&encoded)?;
					}
					Ok::<_, Error>(())
				}
				.await
			}
			Encoding::Idle | Encoding::Disabled => Ok(()),
		};
		match drained {
			Ok(()) => publisher.finish(),
			Err(err) => {
				publisher.abort(moq_net::Error::Transport(err.to_string()));
				Err(err)
			}
		}
	}

	/// Abort the track with `err` instead of finishing it, so subscribers see the real cause rather
	/// than [`moq_net::Error::Dropped`].
	pub fn abort(self, err: moq_net::Error) {
		self.publisher.abort(err);
	}

	/// [`abort`](Self::abort) the track only while nobody wants it, handing the producer back
	/// unchanged when a reader does.
	///
	/// This is how an on-demand publisher ends a track once its [`demand`](Self::demand) reports
	/// nobody wants it: dropping or aborting it instead fails a reader that joined in between.
	/// See [`moq_net::track::Producer::abort_unwanted`].
	#[expect(
		clippy::result_large_err,
		reason = "hand the producer back to keep serving without allocating on an idle check"
	)]
	pub fn abort_unwanted(self, err: moq_net::Error) -> Result<(), Self> {
		self.publisher
			.abort_unwanted(err)
			.map_err(|publisher| Self { publisher, ..self })
	}
}

/// What the capture driver needs beyond the public API: it sizes the encoder to each camera it
/// opens, and releases the encoder with its camera rather than on a drain.
#[cfg(feature = "capture")]
impl Producer {
	/// Encode at `config` from the next frame written, for a capture's source reopened at
	/// whatever mode it negotiated. Call it once the encoder stopped: the next write opens one.
	///
	/// The ceiling and its claim follow the mode, and rate control starts over from the ceiling,
	/// enabled, since what the link did while the source was closed says little about now.
	pub(crate) fn configure(&mut self, mut config: Config) -> Result<(), Error> {
		let ceiling = config.resolved_bitrate();
		config.bitrate = Some(ceiling);
		self.config = config;
		if let Ok(mut current) = self.ceiling.write() {
			*current = ceiling;
		}
		self.reservation.update(ceiling);
		if self.grant.is_some() {
			self.grant = Some(rate::Grant::new(
				self.reservation.consume(),
				policy(ceiling, self.lower.as_ref()),
			));
		}
		if let Encoding::Disabled = self.encoding {
			self.publisher.set_enabled(true)?;
		}
		self.encoding = Encoding::Idle;
		self.target = rate::Target::Enabled(ceiling);
		self.watched = false;
		// Another mode can land on another backend, which may retune where this one refused.
		self.ceiling_only = false;
		self.resume_retry = None;
		self.publisher.report_bitrate(ceiling);
		Ok(())
	}

	/// [`discontinuity`](Self::discontinuity), for a capture whose source stopped or whose viewers
	/// left this rendition. A failed drain only logs: it costs the last frames, and the next frame
	/// written opens a fresh encoder, so it never takes down the renditions beside this one.
	/// Errors only when the break cannot be marked, as the track has closed.
	pub(crate) async fn stop(&mut self) -> Result<(), Error> {
		if let Err(err) = self.close().await {
			tracing::warn!(track = %self.demand.name(), error = %err, "failed to drain a stopped rendition's encoder");
		}
		self.mark()
	}

	/// Close the encoder without draining it and mark the break, for a capture that cancelled an
	/// encode mid-call: the encoder refuses every call after that, so it cannot drain.
	pub(crate) fn release(&mut self) -> Result<(), Error> {
		if let Encoding::Open(_) = self.encoding {
			self.encoding = Encoding::Idle;
		}
		self.mark()
	}

	/// The open encoder's backend name.
	pub(crate) fn name(&self) -> Option<&str> {
		match &self.encoding {
			Encoding::Open(sink) => Some(sink.name()),
			Encoding::Idle | Encoding::Disabled => None,
		}
	}

	/// What the open encoder's backend applied.
	pub(crate) fn applied(&self) -> Option<super::Applied> {
		match &self.encoding {
			Encoding::Open(sink) => Some(sink.applied()),
			Encoding::Idle | Encoding::Disabled => None,
		}
	}

	/// The group a subscriber wants past the largest published; see [`Publisher::wanted`].
	pub(crate) fn wanted(&self) -> Option<u64> {
		self.publisher.wanted()
	}

	/// Follow the grant with nothing written, for a rung nobody watches.
	///
	/// A rung the grant disabled re-enables once it is funded again, so a viewer that switched
	/// down when it disabled can switch back; its encoder opens on the next frame written. Does
	/// nothing for a rendition with no [`lower`](Options::lower) one, which ignores the grant
	/// while unwatched.
	pub(crate) async fn follow_unwritten(&mut self) {
		if self.lower.is_none() {
			return;
		}
		self.unwritten = true;
		self.follow().await;
		self.unwritten = false;
	}

	/// Report source frames the encoder never saw, in the catalog's stats.
	pub(crate) fn report_skipped(&self, frames: u64) {
		self.publisher.report_skipped(frames);
	}
}

impl Producer {
	/// Publish every access unit the encoder has ready, then move the claim to the track the
	/// rendition is on now, even when publishing failed, so it never stays on an ended track.
	fn publish_ready(&mut self) -> Result<(), Error> {
		let published = match &mut self.encoding {
			Encoding::Open(sink) => Self::publish_from(sink, &mut self.publisher, &mut self.marked),
			Encoding::Idle | Encoding::Disabled => Ok(()),
		};
		self.follow_track();
		published
	}

	fn publish_from(sink: &mut Sink, publisher: &mut Publisher, marked: &mut bool) -> Result<(), Error> {
		while let Some(encoded) = sink.receive()? {
			publisher.publish(&encoded)?;
			*marked = false;
		}
		Ok(())
	}

	/// Drain the encoder and close it, so the next frame written opens a fresh one. A disabled
	/// rung stays disabled, still dropping what is written until the grant resumes it.
	async fn close(&mut self) -> Result<(), Error> {
		match std::mem::replace(&mut self.encoding, Encoding::Idle) {
			Encoding::Open(sink) => self.drain(sink).await,
			Encoding::Idle => Ok(()),
			Encoding::Disabled => {
				self.encoding = Encoding::Disabled;
				Ok(())
			}
		}
	}

	/// Flush `sink` and publish its tail, closing it.
	async fn drain(&mut self, mut sink: Sink) -> Result<(), Error> {
		sink.flush().await?;
		let published = Self::publish_from(&mut sink, &mut self.publisher, &mut self.marked);
		self.follow_track();
		published
	}

	/// Publish the break marker, unless nothing was published since the last one.
	fn mark(&mut self) -> Result<(), Error> {
		if !self.marked {
			self.publisher.discontinuity()?;
			self.marked = true;
		}
		Ok(())
	}

	/// A frame whose decoder identity differs from the declared one publishes under a new name
	/// and ends the old track. The claim moves there, following on from the current target.
	fn follow_track(&mut self) {
		let demand = self.publisher.demand();
		if demand.name() == self.demand.name() {
			return;
		}
		tracing::info!(track = %demand.name(), "video moved to a new track");
		self.reservation = Arc::new(self.allocator.reserve(&demand, *self.ceiling.read()));
		self.demand = demand;
		if let Some(grant) = self.grant.as_mut() {
			let mut moved = rate::Grant::new(self.reservation.consume(), grant.policy().clone());
			moved.assume(self.target);
			*grant = moved;
		}
		// The new track's stats start empty.
		if let rate::Target::Enabled(bitrate) = self.target {
			self.publisher.report_bitrate(bitrate);
		}
	}

	/// Apply every target the grant has due, or rest at the ceiling while nobody watches a
	/// rendition that never disables.
	///
	/// A target that fails to apply is taken back and asked for again a ramp step later, so an
	/// encoder that cannot retune at all costs one attempt per step while the grant moves.
	async fn follow(&mut self) {
		if self.lower.is_none() {
			let watched = self.demand.wanted();
			let rested = self.watched && !watched;
			self.watched = watched;
			if rested {
				self.rest().await;
			}
			if !watched {
				return;
			}
		}

		self.resume_at_ceiling().await;

		let noop = kio::Waiter::noop();
		// Each target is one the grant would not hand out again without a new grant, a deadline,
		// or a failure, all of which leave it pending until the next write.
		loop {
			let policy = policy(*self.ceiling.read(), self.lower.as_ref());
			let Some(grant) = self.grant.as_mut() else { return };
			grant.set_policy(policy);
			let target = match grant.poll_next(&noop) {
				Poll::Pending => return,
				Poll::Ready(Ok(target)) => target,
				Poll::Ready(Err(_)) => {
					tracing::debug!("bandwidth grant ended; holding the current encoder bitrate");
					self.grant = None;
					return;
				}
			};
			if let Err(err) = self.apply(target).await {
				tracing::warn!(error = %err, ?target, "failed to follow the bandwidth grant");
				if let Some(grant) = self.grant.as_mut() {
					grant.failed();
				}
				return;
			}
			// A deferred resume told the grant the rung is still off, so it would only offer the
			// same resume again: wait for the next write.
			if matches!(target, rate::Target::Enabled(_)) && matches!(self.encoding, Encoding::Disabled) {
				return;
			}
			tracing::debug!(?target, "followed the bandwidth grant");
		}
	}

	/// Resume a rung that only resumes at its ceiling, once its slice of the grant covers it.
	async fn resume_at_ceiling(&mut self) {
		if !self.ceiling_only || self.grant.is_none() || !matches!(self.encoding, Encoding::Disabled) {
			return;
		}
		let ceiling = *self.ceiling.read();
		if !self.reservation.rate().is_some_and(|rate| rate >= ceiling) {
			return;
		}
		let now = kio::clock::now();
		if self.resume_retry.is_some_and(|retry| now < retry) {
			return;
		}
		let target = rate::Target::Enabled(ceiling);
		match self.apply(target).await {
			Ok(()) => {
				self.resume_retry = None;
				if let Some(grant) = self.grant.as_mut() {
					grant.assume(target);
				}
			}
			// Retried a step later, as a failed grant target is, rather than on every frame.
			Err(err) => {
				tracing::warn!(error = %err, bitrate = ceiling.as_bps(), "failed to resume at the ceiling");
				self.resume_retry = now.checked_add(rate::RETRY);
			}
		}
	}

	/// Leave a rung disabled and tell the grant so, which then offers its resume again from the
	/// current estimate.
	fn defer_resume(&mut self) {
		if let Some(grant) = self.grant.as_mut() {
			grant.assume(rate::Target::Disabled);
		}
	}

	/// Return an encoder nobody watches to its ceiling, and start its rate control over from
	/// there for the next viewer.
	async fn rest(&mut self) {
		// Retired: the grant is gone, and the encoder holds its rate.
		if self.grant.is_none() {
			return;
		}
		let ceiling = *self.ceiling.read();
		if let Err(err) = self.apply(rate::Target::Enabled(ceiling)).await {
			// The grant still matches the rate the encoder kept, so the next viewer follows on from it.
			tracing::warn!(error = %err, bitrate = ceiling.as_bps(), "failed to rest the encoder bitrate");
			return;
		}
		self.grant = Some(rate::Grant::new(self.reservation.consume(), policy(ceiling, None)));
	}

	/// Apply a rate-control target, leaving the rendition as it was when that fails.
	///
	/// A disable drains the encoder into the track and closes it; a resume reopens one, whose
	/// first frame is a keyframe on every backend.
	async fn apply(&mut self, target: rate::Target) -> Result<(), Error> {
		match (target, &mut self.encoding) {
			(rate::Target::Disabled, Encoding::Disabled) => Ok(()),
			(rate::Target::Disabled, _) => {
				// The catalog first, the only step that leaves the rendition as it was when it fails.
				// After it the encoder goes whatever the drain reports, so the catalog never
				// advertises a rendition whose frames are dropped.
				self.publisher.set_enabled(false)?;
				self.target = rate::Target::Disabled;
				let drained = match std::mem::replace(&mut self.encoding, Encoding::Disabled) {
					Encoding::Open(sink) => self.drain(sink).await,
					Encoding::Idle | Encoding::Disabled => Ok(()),
				};
				// The encoder is gone either way, so a lost tail only costs those frames.
				if let Err(err) = drained {
					tracing::warn!(error = %err, "failed to drain a disabled rendition's encoder");
				}
				self.mark()
			}
			(rate::Target::Enabled(bitrate), Encoding::Open(sink)) => {
				sink.set_bitrate(bitrate).await?;
				self.target = target;
				self.publisher.report_bitrate(bitrate);
				Ok(())
			}
			// Applied when the next frame reopens the encoder.
			(rate::Target::Enabled(bitrate), Encoding::Idle) => {
				self.target = target;
				self.publisher.report_bitrate(bitrate);
				Ok(())
			}
			// A resume. One deferred leaves the grant agreeing the rung is off, and
			// `resume_at_ceiling` resumes it once its slice covers the ceiling.
			(rate::Target::Enabled(bitrate), Encoding::Disabled) => {
				let ceiling = *self.ceiling.read();
				let bitrate = bitrate.min(ceiling);
				if self.ceiling_only && bitrate < ceiling {
					self.defer_resume();
					return Ok(());
				}
				// Nothing is written, so the next frame opens the encoder, which disables the rung
				// again rather than overshoot if the backend refuses this rate.
				if self.unwritten {
					self.publisher.set_enabled(true)?;
					self.target = rate::Target::Enabled(bitrate);
					self.publisher.report_bitrate(bitrate);
					self.encoding = Encoding::Idle;
					return Ok(());
				}
				let mut sink = Sink::open(&self.config).await?;
				if bitrate < ceiling
					&& let Err(err) = sink.set_bitrate(bitrate).await
				{
					// Opening at the ceiling would overshoot the grant, and retrying would reopen
					// an encoder every ramp step on a backend that can never retune.
					tracing::warn!(error = %err, bitrate = bitrate.as_bps(), "resumed encoder refused the funded bitrate; resuming only at the ceiling");
					self.ceiling_only = true;
					self.defer_resume();
					return Ok(());
				}
				self.publisher.set_enabled(true)?;
				self.target = rate::Target::Enabled(bitrate);
				self.publisher.report_bitrate(bitrate);
				self.encoding = Encoding::Open(sink);
				Ok(())
			}
		}
	}

	/// Open an encoder at the ceiling and retune it to `wanted`, which becomes the target once it
	/// opens. A failed open changes nothing.
	///
	/// One that cannot retune still opens, at the ceiling, which the grant then follows on from.
	/// A rung whose lower rendition still serves disables instead, as a refused resume does, since
	/// the ceiling would overshoot its grant, and from then on resumes only at its full ceiling:
	/// `None` then.
	async fn reopen(&mut self, wanted: Rate) -> Result<Option<Sink>, Error> {
		let mut sink = Sink::open(&self.config).await?;
		let ceiling = *self.ceiling.read();
		let wanted = wanted.min(ceiling);
		let applied = match wanted < ceiling {
			true => match sink.set_bitrate(wanted).await {
				Ok(()) => wanted,
				// Only while a lower rendition still serves below this one, as the grant disables.
				Err(err)
					if self
						.lower
						.as_ref()
						.and_then(|lower| lower.ceiling(&kio::Waiter::noop()))
						.is_some_and(|lower| lower < ceiling) =>
				{
					tracing::warn!(error = %err, bitrate = wanted.as_bps(), "reopened encoder refused the funded bitrate; resuming only at the ceiling");
					self.ceiling_only = true;
					self.publisher.set_enabled(false)?;
					self.target = rate::Target::Disabled;
					self.encoding = Encoding::Disabled;
					self.mark()?;
					self.defer_resume();
					return Ok(None);
				}
				Err(err) => {
					tracing::warn!(error = %err, bitrate = wanted.as_bps(), "reopened encoder kept its ceiling");
					ceiling
				}
			},
			false => ceiling,
		};
		self.target = rate::Target::Enabled(applied);
		if applied != wanted
			&& let Some(grant) = self.grant.as_mut()
		{
			grant.assume(self.target);
		}
		self.publisher.report_bitrate(applied);
		Ok(Some(sink))
	}
}

/// The track name suffix for `codec`: avc3 or hev1, which carry their parameter sets in band.
pub(crate) fn suffix(codec: Codec) -> &'static str {
	match codec {
		Codec::H264 => ".avc3",
		Codec::H265 => ".hev1",
	}
}

/// The policy a rendition follows: its `ceiling`, above the `lower` rendition's current ceiling.
fn policy(ceiling: Rate, lower: Option<&Rung>) -> rate::Policy {
	let mut policy = rate::Policy::new(ceiling);
	policy.lower = lower.and_then(|lower| lower.ceiling(&kio::Waiter::noop()));
	policy
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use moq_net::Timestamp;

	use super::*;
	use crate::encode::backend::probe;
	use crate::encode::encoder;

	fn config(kind: encoder::Kind, ceiling: Rate) -> Config {
		let mut config = Config::new(320, 240, crate::FrameRate::new(30, 1).unwrap());
		config.kind = kind;
		config.bitrate = Some(ceiling);
		config
	}

	/// The rendition the probe backend would declare, if it emitted H.264: it emits the frame's
	/// timestamp instead, which publishes nothing, so the declared shape never gets corrected.
	fn rendition() -> hang::catalog::VideoConfig {
		hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::H264(hang::catalog::H264 {
			inline: true,
			profile: 0x64,
			constraints: 0,
			level: 0x1f,
		}))
	}

	/// A producer on the probe backend, publishing `name` with no encoder open yet.
	fn probed(
		broadcast: &moq_net::broadcast::Producer,
		catalog: &moq_mux::catalog::Producer,
		name: &str,
		ceiling: Rate,
		bandwidth: moq_net::bandwidth::Allocator,
		lower: Option<Rung>,
	) -> Producer {
		let track = broadcast
			.create_track(name, catalog.track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let config = config(encoder::Kind::Named(probe::NAME.into()), ceiling);
		Producer::with_track(track, catalog, config, rendition(), bandwidth, lower).unwrap()
	}

	/// A mid-gray frame at `micros`, advancing it by one frame at 30 fps.
	fn frame(micros: &mut u64) -> Frame {
		let size = crate::Size::new(320, 240);
		let i420 = crate::I420::new(size, vec![0x80; crate::I420::len(size).unwrap()]).unwrap();
		let frame = Frame::new(crate::Surface::I420(i420), Timestamp::from_micros(*micros).unwrap());
		*micros += 33_333;
		frame
	}

	fn enabled(catalog: &moq_mux::catalog::Producer, name: &str) -> bool {
		catalog.snapshot().video.renditions[name].enabled
	}

	fn events(name: &str) -> usize {
		probe::take().iter().filter(|(event, _)| *event == name).count()
	}

	/// A watched producer retunes to a falling grant between frames, and climbs back to its
	/// ceiling after congestion clears although the grant never changes again, retrying a step
	/// the encoder refused rather than waiting for the next change.
	#[tokio::test(start_paused = true)]
	#[expect(
		clippy::await_holding_lock,
		reason = "the guard only keeps other tests out of the process-wide probe log, and this runtime has one thread, so holding it across an await blocks nothing"
	)]
	async fn follows_a_falling_and_recovering_grant() {
		let _probe = probe::exclusive();
		let ceiling = Rate::from_mbps(4);
		let estimate = moq_net::bandwidth::Producer::new();
		estimate.set(Some(ceiling)).unwrap();
		let bandwidth = moq_net::bandwidth::Allocator::new(estimate.consume());

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut producer = probed(&broadcast, &catalog, "video", ceiling, bandwidth, None);
		let _viewer = broadcast
			.consume()
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();
		let mut micros = 0;

		producer.write(frame(&mut micros)).await.unwrap();
		assert_eq!(producer.bitrate(), Some(ceiling));

		estimate.set(Some(Rate::from_mbps(1))).unwrap();
		for _ in 0..3 {
			tokio::time::advance(Duration::from_millis(33)).await;
			producer.write(frame(&mut micros)).await.unwrap();
		}
		let fallen = probe::bitrates();
		assert!(!fallen.is_empty(), "the encoder never followed the grant down");
		assert!(fallen.iter().all(|&bps| bps <= 1_000_000), "{fallen:?}");
		assert!(producer.bitrate().unwrap() <= Rate::from_mbps(1));

		// The last grant change: the link recovers a second later.
		tokio::time::advance(Duration::from_secs(1)).await;
		estimate.set(Some(ceiling)).unwrap();
		probe::refuse_once(ceiling.as_bps());
		let _ = probe::take();

		// A minute of frames is far past anything the ramp still needs.
		for _ in 0..600 {
			tokio::time::advance(Duration::from_millis(100)).await;
			producer.write(frame(&mut micros)).await.unwrap();
		}
		let recovered = probe::bitrates();
		assert_eq!(
			recovered.last(),
			Some(&ceiling.as_bps()),
			"the ramp stalled: {recovered:?}"
		);
		assert_eq!(producer.bitrate(), Some(ceiling));
		assert!(
			events("set_bitrate") > recovered.len(),
			"the refused step was never retried: {recovered:?}"
		);

		let _finished = producer.finish().await.unwrap();
	}

	/// A rung the grant cannot fund stops encoding, drops what is written, and advertises itself
	/// disabled; resumed, it opens a fresh encoder and advertises itself again.
	#[tokio::test]
	#[expect(
		clippy::await_holding_lock,
		reason = "the guard only keeps other tests out of the process-wide probe log, and this runtime has one thread, so holding it across an await blocks nothing"
	)]
	async fn disables_and_reenables_its_rendition() {
		let _probe = probe::exclusive();
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let unlimited = moq_net::bandwidth::Allocator::unlimited();
		let low = probed(
			&broadcast,
			&catalog,
			"low",
			Rate::from_kbps(500),
			unlimited.clone(),
			None,
		);
		let ceiling = Rate::from_mbps(2);
		let mut high = probed(&broadcast, &catalog, "high", ceiling, unlimited, Some(low.rung()));
		let mut micros = 0;

		high.write(frame(&mut micros)).await.unwrap();
		assert!(enabled(&catalog, "high"));
		let _ = probe::take();

		high.apply(rate::Target::Disabled).await.unwrap();
		assert!(!enabled(&catalog, "high"));
		assert_eq!(high.bitrate(), None);
		let disabled = probe::take();
		assert!(disabled.iter().any(|(event, _)| *event == "flush"), "{disabled:?}");
		assert!(disabled.iter().any(|(event, _)| *event == "drop"), "{disabled:?}");

		high.write(frame(&mut micros)).await.unwrap();
		assert_eq!(events("encode"), 0, "a disabled rung encodes nothing");

		let resumed = Rate::from_mbps(1);
		high.apply(rate::Target::Enabled(resumed)).await.unwrap();
		assert!(enabled(&catalog, "high"));
		assert_eq!(high.bitrate(), Some(resumed));
		assert_eq!(probe::bitrates(), [resumed.as_bps()]);
		assert_eq!(events("open"), 1);

		high.write(frame(&mut micros)).await.unwrap();
		assert_eq!(events("encode"), 1);

		let _high = high.finish().await.unwrap();
		let _low = low.finish().await.unwrap();
	}

	/// A rung whose resumed encoder refuses the funded rate stays disabled rather than open at a
	/// ceiling the grant cannot fund, and from then on resumes only at its full ceiling.
	#[tokio::test]
	#[expect(
		clippy::await_holding_lock,
		reason = "the guard only keeps other tests out of the process-wide probe log, and this runtime has one thread, so holding it across an await blocks nothing"
	)]
	async fn a_refused_resume_waits_for_the_ceiling() {
		let _probe = probe::exclusive();
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let unlimited = moq_net::bandwidth::Allocator::unlimited();
		let low = probed(
			&broadcast,
			&catalog,
			"low",
			Rate::from_kbps(500),
			unlimited.clone(),
			None,
		);
		let ceiling = Rate::from_mbps(2);
		let mut high = probed(&broadcast, &catalog, "high", ceiling, unlimited, Some(low.rung()));
		let mut micros = 0;

		high.write(frame(&mut micros)).await.unwrap();
		high.apply(rate::Target::Disabled).await.unwrap();
		let _ = probe::take();

		let funded = Rate::from_mbps(1);
		probe::refuse_once(funded.as_bps());
		high.apply(rate::Target::Enabled(funded)).await.unwrap();
		assert!(!enabled(&catalog, "high"), "a refused resume stays disabled");
		assert_eq!(high.bitrate(), None);

		// A grant that would now retune fine still funds less than the ceiling.
		let _ = probe::take();
		high.apply(rate::Target::Enabled(funded)).await.unwrap();
		assert!(!enabled(&catalog, "high"));
		assert_eq!(events("open"), 0, "a partial grant no longer reopens the encoder");

		high.apply(rate::Target::Enabled(ceiling)).await.unwrap();
		assert!(enabled(&catalog, "high"));
		assert_eq!(high.bitrate(), Some(ceiling));
		let resumed = probe::take();
		assert!(resumed.iter().all(|(event, _)| *event != "set_bitrate"), "{resumed:?}");

		let _high = high.finish().await.unwrap();
		let _low = low.finish().await.unwrap();
	}

	/// A rung disabled under congestion whose viewers left re-enables once the grant funds it
	/// again, with nothing written and no encoder opened, so a viewer can switch back; the next
	/// frame written opens its encoder.
	#[cfg(feature = "capture")]
	#[tokio::test(start_paused = true)]
	#[expect(
		clippy::await_holding_lock,
		reason = "the guard only keeps other tests out of the process-wide probe log, and this runtime has one thread, so holding it across an await blocks nothing"
	)]
	async fn an_unwatched_rung_re_enables_once_funded() {
		let _probe = probe::exclusive();
		let estimate = moq_net::bandwidth::Producer::new();
		estimate.set(Some(Rate::from_mbps(4))).unwrap();
		let bandwidth = moq_net::bandwidth::Allocator::new(estimate.consume());

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let low = probed(
			&broadcast,
			&catalog,
			"low",
			Rate::from_kbps(500),
			moq_net::bandwidth::Allocator::unlimited(),
			None,
		);
		let ceiling = Rate::from_mbps(2);
		let mut high = probed(&broadcast, &catalog, "high", ceiling, bandwidth, Some(low.rung()));
		let mut micros = 0;
		high.write(frame(&mut micros)).await.unwrap();

		// Disabled while the link was congested, as the grant would have.
		high.apply(rate::Target::Disabled).await.unwrap();
		high.defer_resume();
		assert!(!enabled(&catalog, "high"));
		let _ = probe::take();

		// Nobody watches it now, and the link has recovered: following the grant re-enables it.
		tokio::time::advance(Duration::from_secs(1)).await;
		high.follow_unwritten().await;
		assert!(enabled(&catalog, "high"));
		assert_eq!(events("open"), 0, "an encoder opened for a rendition nobody writes to");

		high.write(frame(&mut micros)).await.unwrap();
		assert_eq!(events("open"), 1);
		assert!(high.bitrate().is_some());

		let _high = high.finish().await.unwrap();
		let _low = low.finish().await.unwrap();
	}

	/// A rung whose lower rendition ended has nothing to fall back to, so an encoder reopened at a
	/// rate it refuses keeps the ceiling rather than disabling the last rendition.
	#[tokio::test(start_paused = true)]
	#[expect(
		clippy::await_holding_lock,
		reason = "the guard only keeps other tests out of the process-wide probe log, and this runtime has one thread, so holding it across an await blocks nothing"
	)]
	async fn a_rung_without_a_lower_rendition_keeps_encoding() {
		let _probe = probe::exclusive();
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let unlimited = moq_net::bandwidth::Allocator::unlimited();
		let low = probed(
			&broadcast,
			&catalog,
			"low",
			Rate::from_kbps(500),
			unlimited.clone(),
			None,
		);
		let ceiling = Rate::from_mbps(2);
		let mut high = probed(&broadcast, &catalog, "high", ceiling, unlimited, Some(low.rung()));
		let mut micros = 0;
		high.write(frame(&mut micros)).await.unwrap();
		let funded = Rate::from_kbps(1_200);
		high.apply(rate::Target::Enabled(funded)).await.unwrap();
		high.discontinuity().await.unwrap();

		let _low = low.finish().await.unwrap();
		probe::refuse_once(funded.as_bps());
		high.write(frame(&mut micros)).await.unwrap();
		assert!(enabled(&catalog, "high"));
		assert_eq!(high.bitrate(), Some(ceiling));

		let _high = high.finish().await.unwrap();
	}

	/// A rung re-enabled with nothing written whose encoder then refuses the funded rate disables
	/// again rather than send at its ceiling, and from then on resumes only once its slice covers
	/// the ceiling.
	#[cfg(feature = "capture")]
	#[tokio::test(start_paused = true)]
	#[expect(
		clippy::await_holding_lock,
		reason = "the guard only keeps other tests out of the process-wide probe log, and this runtime has one thread, so holding it across an await blocks nothing"
	)]
	async fn a_refused_lazy_resume_stays_under_the_grant() {
		let _probe = probe::exclusive();
		let ceiling = Rate::from_mbps(2);
		let funded = Rate::from_kbps(1_200);
		let estimate = moq_net::bandwidth::Producer::new();
		estimate.set(Some(funded)).unwrap();
		let bandwidth = moq_net::bandwidth::Allocator::new(estimate.consume());

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let low = probed(
			&broadcast,
			&catalog,
			"low",
			Rate::from_kbps(500),
			moq_net::bandwidth::Allocator::unlimited(),
			None,
		);
		let mut high = probed(&broadcast, &catalog, "high", ceiling, bandwidth, Some(low.rung()));
		let mut micros = 0;
		high.write(frame(&mut micros)).await.unwrap();
		high.apply(rate::Target::Disabled).await.unwrap();
		high.defer_resume();

		// Funded in part, it re-enables unwritten; the frame that reopens it finds the rate refused.
		tokio::time::advance(Duration::from_secs(1)).await;
		high.follow_unwritten().await;
		assert!(enabled(&catalog, "high"));
		assert_eq!(high.bitrate(), Some(funded));
		probe::refuse_once(funded.as_bps());
		high.write(frame(&mut micros)).await.unwrap();
		assert!(
			!enabled(&catalog, "high"),
			"the rung sent past its grant at the ceiling"
		);
		assert_eq!(high.bitrate(), None);

		// The same partial grant no longer resumes it; one covering the ceiling does.
		for _ in 0..5 {
			tokio::time::advance(Duration::from_secs(1)).await;
			high.follow_unwritten().await;
		}
		assert!(!enabled(&catalog, "high"));
		estimate.set(Some(Rate::from_mbps(4))).unwrap();
		tokio::time::advance(Duration::from_secs(1)).await;
		high.follow_unwritten().await;
		assert!(enabled(&catalog, "high"));
		assert_eq!(high.bitrate(), Some(ceiling));

		let _high = high.finish().await.unwrap();
		let _low = low.finish().await.unwrap();
	}

	/// A rung waiting for its ceiling resumes from the grant: not while its slice covers only
	/// part of the ceiling, and at once when the slice covers it, including after `set_bitrate`
	/// lowers the ceiling to what the slice already funds.
	#[tokio::test(start_paused = true)]
	#[expect(
		clippy::await_holding_lock,
		reason = "the guard only keeps other tests out of the process-wide probe log, and this runtime has one thread, so holding it across an await blocks nothing"
	)]
	async fn a_deferred_resume_follows_the_grant_to_the_ceiling() {
		let _probe = probe::exclusive();
		let estimate = moq_net::bandwidth::Producer::new();
		estimate.set(Some(Rate::from_kbps(1_500))).unwrap();
		let bandwidth = moq_net::bandwidth::Allocator::new(estimate.consume());

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let low = probed(
			&broadcast,
			&catalog,
			"low",
			Rate::from_kbps(500),
			moq_net::bandwidth::Allocator::unlimited(),
			None,
		);
		let ceiling = Rate::from_mbps(2);
		let mut high = probed(&broadcast, &catalog, "high", ceiling, bandwidth, Some(low.rung()));
		let _viewer = broadcast
			.consume()
			.track("high")
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();
		let mut micros = 0;

		high.write(frame(&mut micros)).await.unwrap();
		high.apply(rate::Target::Disabled).await.unwrap();
		let funded = Rate::from_mbps(1);
		probe::refuse_once(funded.as_bps());
		high.apply(rate::Target::Enabled(funded)).await.unwrap();
		assert_eq!(high.bitrate(), None);
		let _ = probe::take();

		// The slice funds part of the ceiling: the grant offers resumes, and none opens.
		for _ in 0..30 {
			tokio::time::advance(Duration::from_millis(100)).await;
			high.write(frame(&mut micros)).await.unwrap();
		}
		assert!(!enabled(&catalog, "high"));
		assert_eq!(events("open"), 0);

		// A ceiling the slice covers resumes it on the next frame, at that ceiling.
		let lowered = Rate::from_mbps(1);
		high.set_bitrate(lowered).await.unwrap();
		high.write(frame(&mut micros)).await.unwrap();
		assert!(enabled(&catalog, "high"));
		assert_eq!(high.bitrate(), Some(lowered));
		let resumed = probe::take();
		assert!(resumed.iter().any(|(event, _)| *event == "open"), "{resumed:?}");
		assert!(resumed.iter().all(|(event, _)| *event != "set_bitrate"), "{resumed:?}");

		let _high = high.finish().await.unwrap();
		let _low = low.finish().await.unwrap();
	}

	/// A resume at the ceiling whose encoder fails to open is tried again a grant's retry step
	/// later, not on every frame.
	#[tokio::test(start_paused = true)]
	#[expect(
		clippy::await_holding_lock,
		reason = "the guard only keeps other tests out of the process-wide probe log, and this runtime has one thread, so holding it across an await blocks nothing"
	)]
	async fn a_failed_resume_waits_a_retry_step() {
		let _probe = probe::exclusive();
		let estimate = moq_net::bandwidth::Producer::new();
		estimate.set(Some(Rate::from_mbps(10))).unwrap();
		let bandwidth = moq_net::bandwidth::Allocator::new(estimate.consume());

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let low = probed(
			&broadcast,
			&catalog,
			"low",
			Rate::from_kbps(500),
			moq_net::bandwidth::Allocator::unlimited(),
			None,
		);
		let ceiling = Rate::from_mbps(2);
		let mut high = probed(&broadcast, &catalog, "high", ceiling, bandwidth, Some(low.rung()));
		let _viewer = broadcast
			.consume()
			.track("high")
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();
		let mut micros = 0;

		// Disabled, and resuming only at its ceiling, which the slice covers.
		high.write(frame(&mut micros)).await.unwrap();
		high.apply(rate::Target::Disabled).await.unwrap();
		probe::refuse_once(Rate::from_mbps(1).as_bps());
		high.apply(rate::Target::Enabled(Rate::from_mbps(1))).await.unwrap();
		let _ = probe::take();

		// An encoder that keeps failing to open, written to well inside one retry step: the
		// ceiling resume and the grant's own resume each try once, not once per frame.
		probe::fail_opens(u32::MAX);
		for _ in 0..8 {
			high.write(frame(&mut micros)).await.unwrap();
			tokio::time::advance(moq_mux::rate::RETRY / 10).await;
		}
		let opens = events("open");
		assert!((1..=2).contains(&opens), "{opens} opens in one retry step");
		assert!(!enabled(&catalog, "high"));

		// Once the encoder opens again, the next retry resumes the rung at its ceiling.
		probe::fail_opens(0);
		tokio::time::advance(moq_mux::rate::RETRY).await;
		high.write(frame(&mut micros)).await.unwrap();
		assert!(enabled(&catalog, "high"));
		assert_eq!(high.bitrate(), Some(ceiling));

		let _high = high.finish().await.unwrap();
		let _low = low.finish().await.unwrap();
	}

	/// A rung must sit above the rendition it falls back to: one that does not fails before its
	/// track exists.
	#[tokio::test]
	async fn a_rung_at_or_below_its_lower_rendition_is_refused() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let unlimited = moq_net::bandwidth::Allocator::unlimited();
		let lower = probed(&broadcast, &catalog, "lower", Rate::from_mbps(2), unlimited, None);

		let mut options = Options::new(config(encoder::Kind::Named(probe::NAME.into()), Rate::from_mbps(2)));
		options.lower = Some(lower.rung());
		assert!(Producer::new(&broadcast, catalog.clone(), options).await.is_err());
		assert_eq!(catalog.snapshot().video.renditions.len(), 1);
	}

	/// A rung takes the priority below the rendition it falls back to, so the allocator funds
	/// that one first.
	#[tokio::test]
	#[cfg(feature = "openh264")]
	async fn a_rung_takes_the_priority_below_its_lower_rendition() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let unlimited = moq_net::bandwidth::Allocator::unlimited();
		let lower = probed(&broadcast, &catalog, "lower", Rate::from_kbps(500), unlimited, None);

		let mut options = Options::new(config(encoder::Kind::Software, Rate::from_mbps(1)));
		options.lower = Some(lower.rung());
		let higher = Producer::new(&broadcast, catalog, options).await.unwrap();
		// Read back from the track it created.
		assert_eq!(higher.priority, lower.priority - 1);
	}

	/// Subscribe to `video`, skipping no group however far behind the reader falls.
	#[cfg(feature = "openh264")]
	async fn subscribe(broadcast: &moq_net::broadcast::Producer) -> moq_net::track::Subscriber {
		let history = moq_net::track::Delay::default().with_max(None);
		broadcast
			.consume()
			.track("video")
			.unwrap()
			.subscribe(moq_net::track::Subscription::default().with_delay(history))
			.await
			.unwrap()
	}

	/// The frames in each group `consumer` reads, until the track ends.
	#[cfg(feature = "openh264")]
	async fn read(mut consumer: moq_net::track::Subscriber) -> Vec<usize> {
		let mut groups = Vec::new();
		while let Some(mut group) = consumer.recv_group().await.unwrap() {
			let mut frames = 0;
			while group.next_frame().await.unwrap().is_some() {
				frames += 1;
			}
			groups.push(frames);
		}
		groups
	}

	/// A discontinuity drains the encoder and marks the break once, however often it is called,
	/// and the next frame opens a fresh encoder on a keyframe.
	#[tokio::test]
	#[cfg(feature = "openh264")]
	async fn a_discontinuity_marks_the_break_once() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut options = Options::new(config(encoder::Kind::Software, Rate::from_mbps(1)));
		options.track = Some("video".into());
		let mut producer = Producer::new(&broadcast, catalog, options).await.unwrap();
		let consumer = subscribe(&broadcast).await;
		let mut micros = 0;

		producer.write(frame(&mut micros)).await.unwrap();
		producer.discontinuity().await.unwrap();
		producer.discontinuity().await.unwrap();
		producer.write(frame(&mut micros)).await.unwrap();
		let _finished = producer.finish().await.unwrap();

		assert_eq!(read(consumer).await, [1, 1, 1]);
	}

	/// A disabled rung's last frames reach the track before its break marker, and it resumes on
	/// a keyframe, so a subscriber reads the gap as a break rather than a stall.
	#[tokio::test]
	#[cfg(feature = "openh264")]
	async fn a_disabled_rung_resumes_on_a_new_group() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut options = Options::new(config(encoder::Kind::Software, Rate::from_mbps(1)));
		options.track = Some("video".into());
		let mut producer = Producer::new(&broadcast, catalog, options).await.unwrap();
		let consumer = subscribe(&broadcast).await;
		let mut micros = 0;

		producer.write(frame(&mut micros)).await.unwrap();
		producer.apply(rate::Target::Disabled).await.unwrap();
		producer.write(frame(&mut micros)).await.unwrap();
		producer
			.apply(rate::Target::Enabled(Rate::from_kbps(500)))
			.await
			.unwrap();
		producer.write(frame(&mut micros)).await.unwrap();
		let _finished = producer.finish().await.unwrap();

		assert_eq!(read(consumer).await, [1, 1, 1]);
	}

	/// A rung resumed at a ceiling past its declared level publishes a new track, and its
	/// bandwidth claim moves there with it rather than watching the track that ended.
	#[tokio::test]
	#[cfg(feature = "openh264")]
	async fn a_resume_past_the_declared_level_moves_the_claim() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut options = Options::new(config(encoder::Kind::Software, Rate::from_mbps(2)));
		options.track = Some("video".into());
		let mut producer = Producer::new(&broadcast, catalog, options).await.unwrap();
		let mut micros = 0;

		producer.write(frame(&mut micros)).await.unwrap();
		let before = producer.reservation().clone();
		producer.apply(rate::Target::Disabled).await.unwrap();
		// 3 Mbps takes H.264 level 2.1, past the 2.0 the 2 Mbps rendition declared.
		producer.set_bitrate(Rate::from_mbps(3)).await.unwrap();
		producer.apply(rate::Target::Enabled(Rate::from_mbps(3))).await.unwrap();
		producer.write(frame(&mut micros)).await.unwrap();

		assert_eq!(producer.demand().name(), "video/1");
		assert!(
			!Arc::ptr_eq(&before, producer.reservation()),
			"the claim still watches the ended track"
		);
		let _finished = producer.finish().await.unwrap();
		assert_eq!(Arc::strong_count(&before), 1, "a finished producer still holds a claim");
	}

	/// A disable that fails partway leaves the catalog and the encoder agreeing, so viewers
	/// never wait on a rendition the catalog calls enabled while its frames are dropped.
	#[tokio::test]
	#[cfg(feature = "openh264")]
	async fn a_failed_disable_leaves_the_catalog_and_encoder_agreeing() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let track = broadcast
			.create_track("video", catalog.track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let config = config(encoder::Kind::Software, Rate::from_mbps(1));
		let rendition = config.probe().await.unwrap();
		let unlimited = moq_net::bandwidth::Allocator::unlimited();
		let mut producer = Producer::with_track(track.clone(), &catalog, config, rendition, unlimited, None).unwrap();
		let mut micros = 0;

		producer.write(frame(&mut micros)).await.unwrap();
		// The track breaks, so the break marker the disable writes fails.
		track.abort(moq_net::Error::Cancel).unwrap();
		let disabled = producer.apply(rate::Target::Disabled).await;

		assert!(disabled.is_err(), "the break marker should fail on a broken track");
		let encoding = matches!(producer.encoding, Encoding::Open(_));
		assert_eq!(
			encoding,
			enabled(&catalog, "video"),
			"the catalog and the encoder disagree"
		);
	}

	/// A lost tail must reach the subscriber as an abort: finishing the track anyway would
	/// report a truncated stream as a complete one, and only the publisher would ever know.
	#[tokio::test]
	#[expect(
		clippy::await_holding_lock,
		reason = "the guard only keeps other tests out of the process-wide probe log, and this runtime has one thread, so holding it across an await blocks nothing"
	)]
	async fn a_lost_tail_aborts_the_track() {
		let _probe = probe::exclusive();
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let unlimited = moq_net::bandwidth::Allocator::unlimited();
		let mut producer = probed(&broadcast, &catalog, "video", Rate::from_mbps(1), unlimited, None);
		let mut track = broadcast
			.consume()
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();
		let mut micros = 0;

		// The probe holds this frame back, so only the flush can publish it.
		producer.write(frame(&mut micros)).await.unwrap();
		probe::lose_tail();
		assert!(producer.finish().await.is_err());

		let Err(err) = track.recv_group().await else {
			panic!("expected an abort, not a clean end");
		};
		assert!(
			err.to_string().contains("the probe lost its tail"),
			"the abort should carry the drain failure: {err}"
		);
	}

	/// `set_bitrate` lowers the ceiling while the grant is climbing back toward the old one:
	/// nothing retunes past the new ceiling, and the encoder recovers to it.
	#[tokio::test(start_paused = true)]
	#[expect(
		clippy::await_holding_lock,
		reason = "the guard only keeps other tests out of the process-wide probe log, and this runtime has one thread, so holding it across an await blocks nothing"
	)]
	async fn a_lowered_ceiling_caps_a_recovering_grant() {
		let _probe = probe::exclusive();
		let old = Rate::from_mbps(4);
		let lowered = Rate::from_mbps(2);
		let estimate = moq_net::bandwidth::Producer::new();
		estimate.set(Some(Rate::from_mbps(1))).unwrap();
		let bandwidth = moq_net::bandwidth::Allocator::new(estimate.consume());

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut producer = probed(&broadcast, &catalog, "video", old, bandwidth, None);
		let _viewer = broadcast
			.consume()
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();
		let mut micros = 0;

		producer.write(frame(&mut micros)).await.unwrap();
		assert!(producer.bitrate().unwrap() <= Rate::from_mbps(1));
		// The link recovers, and the encoder starts climbing back toward the old ceiling.
		tokio::time::advance(Duration::from_secs(1)).await;
		estimate.set(Some(old)).unwrap();
		for _ in 0..3 {
			tokio::time::advance(Duration::from_millis(100)).await;
			producer.write(frame(&mut micros)).await.unwrap();
		}

		producer.set_bitrate(lowered).await.unwrap();
		let _ = probe::bitrates();
		for _ in 0..600 {
			tokio::time::advance(Duration::from_millis(100)).await;
			producer.write(frame(&mut micros)).await.unwrap();
		}
		let applied = probe::bitrates();
		assert!(applied.iter().all(|&bps| bps <= lowered.as_bps()), "{applied:?}");
		assert_eq!(producer.bitrate(), Some(lowered));
		assert_eq!(producer.reservation().rate(), Some(lowered));
	}

	/// A manual retune to the unchanged ceiling hands the encoder back to the grant rather than
	/// leaving it at the ceiling the grant cannot fund.
	#[tokio::test(start_paused = true)]
	#[expect(
		clippy::await_holding_lock,
		reason = "the guard only keeps other tests out of the process-wide probe log, and this runtime has one thread, so holding it across an await blocks nothing"
	)]
	async fn a_manual_retune_returns_to_the_grant() {
		let _probe = probe::exclusive();
		let ceiling = Rate::from_mbps(4);
		let estimate = moq_net::bandwidth::Producer::new();
		estimate.set(Some(Rate::from_mbps(1))).unwrap();
		let bandwidth = moq_net::bandwidth::Allocator::new(estimate.consume());

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut producer = probed(&broadcast, &catalog, "video", ceiling, bandwidth, None);
		let _viewer = broadcast
			.consume()
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();
		let mut micros = 0;

		producer.write(frame(&mut micros)).await.unwrap();
		assert!(producer.bitrate().unwrap() <= Rate::from_mbps(1));

		producer.set_bitrate(ceiling).await.unwrap();
		assert_eq!(producer.bitrate(), Some(ceiling));
		for _ in 0..10 {
			tokio::time::advance(Duration::from_millis(100)).await;
			producer.write(frame(&mut micros)).await.unwrap();
		}
		assert!(
			producer.bitrate().unwrap() <= Rate::from_mbps(1),
			"{:?}",
			producer.bitrate()
		);
	}
}
