//! Writes a [`Passthrough`](super::Passthrough) track back out as the multiplex it carries.

use std::task::Poll;
use std::time::Duration;

use kio::clock::Instant;

use crate::catalog::{CatalogFormat, Stream};
use crate::container::consumer::Event;
use crate::container::{ExportSource, Frame};
use crate::jitter::{self, Arrival, Push};

/// Writes the multiplex a broadcast's [`m2ts`](hang::catalog::M2ts) track carries back out
/// byte-identical, each object a fixed delay after its PCR time, like an SRT receiver.
///
/// The [importer](super::Passthrough) stamps each object, the packets from one PCR to the
/// next, with the PCR time of its first byte on the catalog clock. The release holds every
/// object until that time plus the delay on the jitter buffer's clock, which follows the
/// source's, so the output keeps the source's PCR pacing whatever the arrival jitter, and two
/// releases fed the same objects write the same bytes. An object that arrives past its
/// deadline is dropped and counted; the output shows a continuity error there, since nothing
/// is rewritten, and carries on with the next object. A flagged forward jump, which the
/// importer declares with a discontinuity marker, opens a new clock, so the output does not
/// wait it out; the bytes carry the jump.
pub struct Release {
	source: crate::Source,
	catalog: Option<crate::catalog::Consumer<()>>,
	/// The `m2ts` track, once a catalog names it.
	track: Option<(String, ExportSource)>,
	/// When the track was subscribed. Nothing it delivers reached the release before then,
	/// however long the cache held it.
	subscribed: Instant,
	finished: bool,
	/// An object has gone out.
	released: bool,
	delay: Duration,
	jitter: jitter::Buffer<(), Frame>,
	replay: bool,
}

impl Release {
	/// Subscribe to the catalog of `source`, waiting for the `m2ts` section that names its track.
	///
	/// Fails for [`CatalogFormat::Msf`], which has no `m2ts` section.
	pub async fn new(source: crate::Source, catalog_format: CatalogFormat) -> crate::Result<Self> {
		if catalog_format == CatalogFormat::Msf {
			return Err(
				anyhow::anyhow!("the MSF catalog has no `m2ts` passthrough track; read the hang catalog").into(),
			);
		}
		let catalog = source.catalog(catalog_format).await?;
		let mut release = Self {
			source,
			catalog: Some(catalog),
			track: None,
			subscribed: kio::clock::now(),
			finished: false,
			released: false,
			delay: Duration::ZERO,
			jitter: jitter::Buffer::new(Duration::ZERO),
			replay: false,
		};
		release.jitter = release.jitter();
		Ok(release)
	}

	/// Write each object this long after its PCR time. An object that arrives later is dropped.
	/// Defaults to [`Duration::ZERO`], which holds nothing.
	pub fn with_delay(mut self, delay: Duration) -> Self {
		self.delay = delay;
		self.jitter = self.jitter();
		self
	}

	/// Read the track from the oldest group the delay still reaches instead of the live edge,
	/// and anchor the clock on the first object, for a test that writes a broadcast whole first.
	#[cfg(test)]
	pub(super) fn with_replay(mut self) -> Self {
		self.replay = true;
		self.jitter = self.jitter();
		self
	}

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
		if let Some((_, track)) = &self.track {
			stats.gaps = track.skips() - track.markers();
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
	pub async fn next(&mut self) -> crate::Result<Option<Frame>> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next original MPEG-TS frame at its release time, or `None` at end.
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
					decode: frame.timestamp.into(),
					restart: track.markers(),
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

	/// Subscribe to the track the catalog's `m2ts` section names, the first time it names one.
	fn update_catalog(&mut self, catalog: &crate::catalog::hang::Catalog<()>) -> crate::Result<()> {
		let Some(m2ts) = &catalog.m2ts else {
			if self.track.is_none() && !(catalog.video.renditions.is_empty() && catalog.audio.renditions.is_empty()) {
				return Err(anyhow::anyhow!(
					"the broadcast is demultiplexed, with no `m2ts` passthrough track to release"
				)
				.into());
			}
			return Ok(());
		};
		match &self.track {
			None => {
				let source = ExportSource::for_stream(&self.source, &m2ts.track, self.delay / 2)?;
				let source = match self.replay {
					true => source,
					false => source.live(),
				};
				self.track = Some((m2ts.track.clone(), source));
				self.subscribed = kio::clock::now();
			}
			Some((name, _)) if *name != m2ts.track => {
				return Err(anyhow::anyhow!("the `m2ts` track moved from `{name}` to `{}`", m2ts.track).into());
			}
			Some(_) => {}
		}
		Ok(())
	}
}
