//! Writes a [`Passthrough`](super::Passthrough) track back out as the multiplex it carries.

use std::task::Poll;
use std::time::Duration;

use kio::clock::Instant;

use super::error::unsupported;
use super::export;

#[cfg(feature = "async")]
use crate::catalog::CatalogFormat;
use crate::catalog::Stream;
use crate::catalog::hang::Catalog;
use crate::container::consumer::Event;
use crate::container::{ExportSource, Frame, Resume};
use crate::jitter::{self, Arrival, Push};

/// Writes the multiplex a broadcast's [passthrough track](super::Mpegts::passthrough) carries
/// back out byte-identical, each object a fixed delay after its PCR time, like an SRT receiver.
///
/// The [importer](super::Passthrough) stamps each object, the packets from one PCR to the
/// next, with the PCR time of its first byte on the catalog clock. The release holds every
/// object until that time plus the delay on the jitter buffer's clock, which follows the
/// source's, so the output keeps the source's PCR pacing whatever the arrival jitter, and two
/// releases fed the same objects write the same bytes. An object that arrives past its
/// deadline is dropped and counted; the output shows a continuity error there, since nothing
/// is rewritten, and carries on with the next object. A PCR clock break, which the importer
/// declares with a discontinuity marker, opens a new clock, so the output does not wait out a
/// jump; the bytes carry the break as the source sent it.
///
/// A [`Follower`](super::Follower) carries a release on into the instance that takes its path:
/// that instance's objects open a new clock, after whatever of the last one is still queued.
pub struct Release {
	source: crate::Source,
	/// The instance of the broadcast being released, `None` for one not resolved through an
	/// origin.
	instance: Option<moq_net::Instance>,
	catalog: Option<crate::catalog::Consumer<super::Ext>>,
	catalog_format: crate::catalog::CatalogFormat,
	/// The passthrough track, once a catalog names it.
	track: Option<(String, ExportSource)>,
	/// Where the track stopped, for the same instance's return to read on from.
	resume: Option<Resume>,
	/// When the track was subscribed. Nothing it delivers reached the release before then,
	/// however long the cache held it.
	subscribed: Instant,
	finished: bool,
	/// An object has gone out.
	released: bool,
	delay: Duration,
	jitter: jitter::Buffer<(), Frame>,
	replay: bool,
	/// The timeline restarts of the instances released before this one, each counting one more
	/// for the switch away from it.
	restarts: u64,
	/// The groups the instances released before this one skipped.
	gaps: u64,
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
			catalog: Some(catalog),
			catalog_format,
			track: None,
			resume: None,
			subscribed: kio::clock::now(),
			finished: false,
			released: false,
			delay,
			jitter,
			replay: false,
			restarts: 0,
			gaps: 0,
		};
		Ok(release)
	}

	/// The source being released.
	pub(super) fn source(&self) -> &crate::Source {
		&self.source
	}

	/// The instance of the broadcast being released.
	pub(super) fn instance(&self) -> Option<&moq_net::Instance> {
		self.instance.as_ref()
	}

	/// Whether the catalog last carried on into has named the passthrough track.
	pub(super) fn cataloged(&self) -> bool {
		self.track.is_some()
	}

	/// The format the catalog is subscribed in.
	pub(super) fn catalog_format(&self) -> crate::catalog::CatalogFormat {
		self.catalog_format
	}

	/// Carry on into `broadcast`, whose `catalog` is subscribed. The same instance's return reads
	/// its track on after the group last delivered, on the same clock, giving up the rest of the
	/// group it was reading as a gap, however often it returns before its catalog names the
	/// track. Another instance's track replaces the one being read, and its objects go out on a
	/// new clock once what is queued has.
	pub(super) fn followed(
		&mut self,
		broadcast: &moq_net::broadcast::Consumer,
		catalog: crate::catalog::Consumer<super::Ext>,
	) {
		let same = self.instance.is_some() && broadcast.info().instance == self.instance;
		if same {
			self.source.pin(broadcast);
		} else {
			self.source = self.source.switched(broadcast);
			self.instance = broadcast.info().instance.clone();
			self.resume = None;
		}
		self.catalog = Some(catalog);
		if let Some((_, track)) = self.track.take() {
			self.restarts += track.markers() + u64::from(!same);
			self.gaps += track.skips() - track.markers();
			if same {
				self.gaps += u64::from(track.mid_group());
				self.resume = Some(track.resume());
			}
		}
		self.finished = false;
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

	/// The next object, at its time: whole TS packets, verbatim, stamped with the PCR time of
	/// its first byte. Returns `None` once the track, or a catalog that never named one, ends.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Frame>>> {
		while let Some(catalog) = self.catalog.as_mut() {
			match catalog.poll_next(waiter)? {
				Poll::Ready(Some(snapshot)) => self.update_catalog(&snapshot)?,
				Poll::Ready(None) => self.catalog = None,
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
					Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
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

		if let Poll::Ready(ready) = self.jitter.poll_next(waiter) {
			self.released = true;
			return Poll::Ready(Ok(Some(ready.item)));
		}
		let ended = match &self.track {
			Some(_) => self.finished,
			None => self.catalog.is_none(),
		};
		match ended && self.jitter.is_empty() {
			true => Poll::Ready(Ok(None)),
			false => Poll::Pending,
		}
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
