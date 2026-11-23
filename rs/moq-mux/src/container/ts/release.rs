//! Writes a [`Passthrough`](super::Passthrough) track back out as the multiplex it carries.

use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use kio::clock::{Instant, Sleep};

use super::error::unsupported;
use super::export;

use crate::catalog::hang::Catalog;
use crate::catalog::{CatalogFormat, Stream};
use crate::container::consumer::Event;
use crate::container::{ExportSource, Frame, Resume};
use crate::jitter::{self, Arrival, Push};

/// The packets a release writes at once: a UDP datagram's worth, as SRT and RTP senders take them.
const RUN: usize = 7 * 188;

/// The longest PCR interval a release spreads an object across, ISO 13818-1's limit.
const SPREAD: Duration = Duration::from_millis(100);

/// Writes the multiplex a broadcast's [passthrough track](super::Mpegts::passthrough) carries
/// back out byte-identical, each packet a fixed delay after its time on the source's PCR, like
/// an SRT receiver.
///
/// The [importer](super::Passthrough) stamps each object, the packets from one PCR to the
/// next, with the PCR time of its first byte on the catalog clock. The release holds every
/// object until that time plus the delay on the jitter buffer's clock, which follows the
/// source's, then spreads its packets evenly up to the next object's time, seven at a time
/// (a UDP datagram's worth), so the output keeps the source's rate whatever the arrival jitter.
/// An object whose successor is not yet buffered when it comes due, or is more than ISO
/// 13818-1's 100 ms PCR interval ahead, goes out whole, as does every object under no delay.
/// Two releases that each receive every object before its deadline write the same bytes; where
/// each held an object's successor when it came due, at the same times
/// too, so a delay longer than the arrival jitter by a PCR interval keeps them in step. An
/// object that arrives past its deadline is dropped and counted; the output shows a continuity
/// error there, since nothing is rewritten, and carries on with the next object. A PCR clock
/// break, which the importer declares with a discontinuity marker, opens a new clock, so the
/// output does not wait out a jump; the bytes carry the break as the source sent it.
///
/// A [`Follower`](super::Follower) carries it across its broadcast's returns and, when
/// stitching, onto another publisher instance, whose track it reads from the first group its
/// live subscription delivers, after what was already buffered has gone out. Its packets follow
/// verbatim, so the output shows the seam as each PID's continuity break, as the source would
/// show a splice, and they go out on a clock of their own, as after an encoder changeover.
pub struct Release {
	source: crate::Source,
	/// The format of the catalog being read, kept for following another instance.
	format: CatalogFormat,
	/// The instance of the broadcast being read, `None` for one not resolved through an origin.
	instance: Option<moq_net::Instance>,
	catalog: Option<crate::catalog::Consumer<super::Ext>>,
	/// The catalog last subscribed has delivered a snapshot.
	cataloged: bool,
	/// The passthrough track, once a catalog names it.
	track: Option<(String, ExportSource)>,
	/// When the track was subscribed. Nothing it delivers reached the release before then,
	/// however long the cache held it.
	subscribed: Instant,
	finished: bool,
	/// Where the track read before its instance went stopped, for that instance's return.
	resume: Option<Resume>,
	/// The publisher left with this error; it is reported once what the jitter buffer holds
	/// has gone out on its clock.
	failed: Option<crate::Error>,
	/// An object has gone out.
	released: bool,
	/// Timeline restarts and gaps counted by tracks read before the current one, whose own
	/// counts start over.
	restarts: u64,
	gaps: u64,
	delay: Duration,
	jitter: jitter::Buffer<(), Frame>,
	/// The object going out, a run at a time.
	spread: Option<Spread>,
	timer: Option<Pin<Box<Sleep>>>,
	replay: bool,
}

/// An object's runs, each due at its first byte's time on the way from the object's time to the next's.
pub(super) struct Spread {
	frame: Frame,
	start: Instant,
	span: Duration,
	/// The next object's timestamp, from which each run's own is interpolated.
	end: Option<moq_net::Timestamp>,
	runs: usize,
	sent: usize,
}

impl Spread {
	/// Spread `frame` from `start` to `next`, the successor's deadline and timestamp, if its
	/// interval on the source's clock is a PCR interval and the jitter clock, which runs within
	/// [`MAX_DRIFT`](jitter::MAX_DRIFT) of the source's, has not stretched it further.
	pub(super) fn new(frame: Frame, start: Instant, next: Option<(Instant, moq_net::Timestamp)>) -> Self {
		let next = next.filter(|(at, end)| {
			*at >= start
				&& at.duration_since(start) <= SPREAD.mul_f64(1.0 + jitter::MAX_DRIFT)
				&& end.scale() == frame.timestamp.scale()
				&& end
					.checked_sub(frame.timestamp)
					.and_then(|interval| interval.as_micros())
					.is_ok_and(|micros| micros <= SPREAD.as_micros())
		});
		let runs = match next {
			Some(_) => frame.payload.len().div_ceil(RUN).max(1),
			None => 1,
		};
		Self {
			span: next.map_or(Duration::ZERO, |(at, _)| at.duration_since(start)),
			end: next.map(|(_, end)| end),
			frame,
			start,
			runs,
			sent: 0,
		}
	}

	/// How far into the object run `i` starts, as a fraction of its bytes.
	fn share(&self, i: usize, of: u128) -> u128 {
		of * (i * RUN) as u128 / self.frame.payload.len().max(1) as u128
	}

	/// When run `i` is due: the time of its first byte, at the object's rate to the next.
	fn due(&self, i: usize) -> Instant {
		let offset = self.share(i, self.span.as_nanos());
		self.start.saturating_add(Duration::from_nanos(offset as u64))
	}

	/// Every run due by `now`, as one frame stamped with the first one's time.
	pub(super) fn take(&mut self, now: Instant) -> Option<Frame> {
		let due = (self.sent..self.runs).take_while(|&i| self.due(i) <= now).count();
		if due == 0 {
			return None;
		}
		if self.runs == 1 {
			self.sent = 1;
			return Some(self.frame.clone());
		}
		let (from, to) = (self.sent * RUN, ((self.sent + due) * RUN).min(self.frame.payload.len()));
		let mut timestamp = self.frame.timestamp;
		if let (Some(end), Some(scale)) = (self.end, timestamp.scale()) {
			let (start, end) = (timestamp.value(), end.value());
			let value = start + self.share(self.sent, (end - start) as u128) as u64;
			timestamp = moq_net::Timestamp::new(value, scale).unwrap_or(timestamp);
		}
		self.sent += due;
		Some(Frame {
			timestamp,
			duration: None,
			payload: self.frame.payload.slice(from..to),
			keyframe: self.frame.keyframe && from == 0,
		})
	}

	fn done(&self) -> bool {
		self.sent == self.runs
	}
}

impl Release {
	/// Subscribe to the catalog of `source`, in either format, waiting for the `mpegts` section to
	/// name its passthrough track.
	///
	/// Each object is written `delay` after its PCR time; late objects are dropped.
	#[cfg(feature = "async")]
	pub async fn new(source: crate::Source, catalog_format: CatalogFormat, delay: Duration) -> crate::Result<Self> {
		let mut subscribing = crate::catalog::Subscribing::new(&source.broadcast(), catalog_format);
		let catalog = kio::wait(|waiter| subscribing.poll_ok(waiter)).await?;
		let mut jitter = jitter::Buffer::new(delay);
		jitter.expect([()]);
		let release = Self {
			instance: source.broadcast().info().instance.clone(),
			source,
			format: catalog_format,
			catalog: Some(catalog),
			cataloged: false,
			track: None,
			subscribed: kio::clock::now(),
			finished: false,
			resume: None,
			failed: None,
			released: false,
			restarts: 0,
			gaps: 0,
			delay,
			jitter,
			spread: None,
			timer: None,
			replay: false,
		};
		Ok(release)
	}

	/// Read the track from the oldest group the delay still reaches instead of the live edge,
	/// and anchor the clock on the first object, for a test that writes a broadcast whole first.
	#[cfg(test)]
	pub(super) fn with_replay(mut self) -> Self {
		self.replay = true;
		self.jitter = self.jitter();
		self
	}

	#[cfg(test)]
	fn jitter(&self) -> jitter::Buffer<(), Frame> {
		let mut jitter = jitter::Buffer::new(self.delay);
		#[cfg(test)]
		if self.replay {
			jitter = jitter.replay();
		}
		jitter.expect([()]);
		jitter
	}

	/// How many objects were dropped for missing their deadline.
	pub fn dropped(&self) -> u64 {
		self.jitter.dropped()
	}

	/// The source's clock rate off ours, as last measured, as a fraction: positive runs fast.
	pub fn drift(&self) -> Option<f64> {
		self.jitter.drift()
	}

	/// How many times the source's clock was measured further off ours than the release follows.
	pub fn out_of_tolerance(&self) -> u64 {
		self.jitter.out_of_tolerance()
	}

	/// Snapshot how the release clock is keeping up with the source.
	pub fn stats(&self) -> super::stats::Release {
		let mut stats = super::stats::Release::of(&self.jitter);
		stats.gaps = self.gaps;
		if let Some((_, track)) = &self.track {
			stats.gaps += track.skips() - track.markers();
		}
		stats
	}

	/// How many objects are waiting in the jitter buffer.
	#[cfg(test)]
	pub(super) fn buffered(&self) -> usize {
		self.jitter.len()
	}

	/// The next run of packets, at its time: whole TS packets, verbatim, stamped with the PCR
	/// time of its first byte. Returns `None` once the track, or a catalog that never named one,
	/// ends.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Frame>>> {
		while let Some(catalog) = self.catalog.as_mut() {
			match catalog.poll_next(waiter) {
				Poll::Ready(Ok(Some(snapshot))) => {
					self.cataloged = true;
					self.update_catalog(&snapshot)?
				}
				Poll::Ready(Ok(None)) => self.catalog = None,
				Poll::Ready(Err(err)) => self.leave(err)?,
				Poll::Pending => break,
			}
		}

		if !self.finished
			&& let Some((name, track)) = self.track.as_mut()
		{
			loop {
				if self.jitter.full(&(), waiter) {
					let _ = track.poll_received(waiter);
					break;
				}
				let (frame, arrived) = match track.poll_event(waiter) {
					Poll::Ready(Ok(Some(Event::Frame { frame, arrived }))) => (frame, arrived),
					Poll::Ready(Ok(Some(Event::GroupEnd | Event::FrameEnd { .. } | Event::Cut { .. }))) => continue,
					Poll::Ready(Ok(None)) => {
						self.finished = true;
						break;
					}
					Poll::Ready(Err(err)) => {
						self.leave(err)?;
						break;
					}
					Poll::Pending => break,
				};
				// Judged on when the object landed, so a sink that leaves arrived objects unread
				// does not make them late, and a stall of the whole path does not hide that they are.
				let arrival = Arrival {
					arrived: arrived.max(self.subscribed),
					read: kio::clock::now(),
					decode: frame.timestamp.try_into()?,
					restart: self.restarts + track.markers(),
					// The output opens on a group, at a random access point; from then on every
					// object goes out on its own, so one dropped late is a gap of its packets only.
					sync: frame.keyframe || self.released,
					leading: false,
					bytes: frame.payload.len(),
					item: frame,
				};
				if self.jitter.push((), arrival)? == Push::Late {
					tracing::warn!(track = %name, dropped = self.jitter.dropped(), "object missed its deadline; dropped");
				}
			}
		}

		if self.spread.is_none()
			&& let Poll::Ready(ready) = self.jitter.poll_next(waiter)
		{
			self.released = true;
			// No delay releases an object as it arrives, ahead of its deadline.
			let (start, next) = match self.delay.is_zero() {
				true => (kio::clock::now(), None),
				false => (
					ready.deadline,
					self.jitter
						.front_deadline()
						.zip(self.jitter.peek(&()).map(|frame| frame.timestamp)),
				),
			};
			self.spread = Some(Spread::new(ready.item, start, next));
		}
		if let Some(spread) = self.spread.as_mut() {
			loop {
				if let Some(run) = spread.take(kio::clock::now()) {
					if spread.done() {
						self.spread = None;
						self.timer = None;
					}
					return Poll::Ready(Ok(Some(run)));
				}
				let wake = spread.due(spread.sent);
				let timer = self
					.timer
					.get_or_insert_with(|| Box::pin(kio::clock::sleep_until(wake)));
				if timer.deadline() != wake {
					timer.as_mut().reset(wake);
				}
				if waiter.poll_future(timer.as_mut()).is_pending() {
					return Poll::Pending;
				}
			}
		}
		let ended = match &self.track {
			Some(_) => self.finished,
			None => self.catalog.is_none(),
		};
		if !(ended && self.jitter.is_empty()) {
			return Poll::Pending;
		}
		match self.failed.take() {
			Some(err) => Poll::Ready(Err(err)),
			None => Poll::Ready(Ok(None)),
		}
	}

	/// The catalog or track failed with `err`. Only a publisher that left is survivable: reading
	/// stops at once, what the jitter buffer holds goes out on its clock, and then the release
	/// reports `err`, so a [`Follower`](super::Follower) waits for a return only once the output
	/// before the gap has gone out, rather than leaving it buffered to go out late behind the gap.
	fn leave(&mut self, err: crate::Error) -> crate::Result<()> {
		if !super::follower::left(&err) {
			return Err(err);
		}
		self.catalog = None;
		self.finished = true;
		self.failed = Some(err);
		Ok(())
	}

	/// Subscribe to the passthrough track the catalog names, the first time it names one.
	fn update_catalog(&mut self, catalog: &Catalog<super::Ext>) -> crate::Result<()> {
		let Some(passthrough) = &catalog.ext.mpegts.passthrough else {
			if self.track.is_none() && !(catalog.video.renditions.is_empty() && catalog.audio.renditions.is_empty()) {
				return Err(
					unsupported!("the broadcast is demultiplexed, with no passthrough track to release").into(),
				);
			}
			return Ok(());
		};
		match &self.track {
			None => {
				let source =
					ExportSource::for_stream(&self.source, &passthrough.track, export::subscribed(self.delay))?;
				let source = export::skipping(source, self.delay);
				// The instance returning carries on after the group it was reading, which it
				// would otherwise replay, rather than at its live edge, which would skip the
				// groups after it that the delay may still reach.
				let source = match (self.resume.take(), self.replay) {
					(Some(resume), _) => source.resuming(&resume),
					(None, true) => source,
					(None, false) => source.live(),
				};
				self.track = Some((passthrough.track.clone(), source));
				self.subscribed = kio::clock::now();
			}
			Some((name, _)) if *name != passthrough.track => {
				return Err(
					unsupported!("the passthrough track moved from `{name}` to `{}`", passthrough.track).into(),
				);
			}
			Some(_) => {}
		}
		Ok(())
	}
}

impl super::follower::sealed::Followed for Release {
	type Ext = super::Ext;

	fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Frame>>> {
		Release::poll_next(self, waiter)
	}

	fn cataloged(&self) -> bool {
		self.cataloged
	}

	fn source(&self) -> &crate::Source {
		&self.source
	}

	fn instance(&self) -> Option<&moq_net::Instance> {
		self.instance.as_ref()
	}

	/// Drop the track being read and read the passthrough track `catalog` names next. What the
	/// jitter buffer holds still goes out on its clock. The same instance returning carries on
	/// past the group it was reading; another opens a new timeline, whose clock owes the old
	/// one nothing.
	fn followed(
		&mut self,
		broadcast: &moq_net::broadcast::Consumer,
		catalog: crate::catalog::Consumer<super::Ext>,
		_follow: Duration,
	) -> crate::Result<()> {
		let same = self.instance.is_some() && broadcast.info().instance == self.instance;
		let track = self.track.take();
		if let Some((_, track)) = &track {
			self.gaps += track.skips() - track.markers();
			self.restarts += track.markers();
		}
		if same {
			self.source.pin(broadcast);
			// A track never read has no cursor, but an earlier one still might.
			if let Some((_, track)) = &track {
				self.resume = Some(track.resume());
			}
		} else {
			self.source = self.source.switched(broadcast);
			self.instance = broadcast.info().instance.clone();
			self.resume = None;
			self.restarts += 1;
		}
		self.catalog = Some(catalog);
		self.cataloged = false;
		self.finished = false;
		self.failed = None;
		Ok(())
	}

	fn catalog_format(&self) -> CatalogFormat {
		self.format
	}
}
