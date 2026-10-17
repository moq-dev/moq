//! Writes a [`Passthrough`](super::Passthrough) track back out as the multiplex it carries.

use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

use web_async::time::Instant;

use crate::catalog::{CatalogFormat, Stream};
use crate::container::{ExportSource, Frame};
use crate::jitter::{self, Arrival, Push};

/// Availability observed even while a sink leaves its source unpolled.
struct Readable {
	state: Mutex<Availability>,
}

struct Availability {
	empty: Instant,
	first: Option<Instant>,
	polling: bool,
	during: Option<Instant>,
	// Weak registrations keep observing the source without keeping its caller alive.
	waiters: kio::WaiterList,
}

impl Readable {
	fn new() -> Self {
		Self {
			state: Mutex::new(Availability {
				empty: Instant::now(),
				first: None,
				polling: false,
				during: None,
				waiters: kio::WaiterList::new(),
			}),
		}
	}

	fn begin(&self, waiter: &kio::Waiter) {
		let mut state = self.state.lock().unwrap();
		debug_assert!(!state.polling);
		state.polling = true;
		state.during = None;
		waiter.register(&mut state.waiters);
	}

	fn end(&self, pending: bool) {
		let mut state = self.state.lock().unwrap();
		state.polling = false;
		if pending {
			// A notification racing the empty read belongs to the next batch.
			state.first = state.during.take();
			state.empty = Instant::now();
		}
		state.during = None;
	}

	fn arrived(&self) -> Instant {
		let state = self.state.lock().unwrap();
		state.first.unwrap_or(state.empty)
	}
}

impl Wake for Readable {
	fn wake(self: Arc<Self>) {
		self.wake_by_ref();
	}

	fn wake_by_ref(self: &Arc<Self>) {
		let mut waiters = {
			let mut state = self.state.lock().unwrap();
			let now = Instant::now();
			state.first.get_or_insert(now);
			if state.polling {
				state.during.get_or_insert(now);
			}
			state.waiters.take()
		};
		waiters.wake();
	}
}

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
/// importer marks by skipping a group sequence, opens a new clock once it leaps further than
/// the delay, so the output does not wait it out; the bytes carry the jump.
pub struct Release {
	source: crate::Source,
	catalog: Option<crate::catalog::Consumer<()>>,
	/// The `m2ts` track, once a catalog names it.
	track: Option<(String, ExportSource)>,
	readable: Arc<Readable>,
	/// Wakes `readable`, which forwards to the caller.
	notify: Waker,
	/// Keeps the source's registrations alive while a sink leaves it unpolled.
	input: kio::Park,
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
		let readable = Arc::new(Readable::new());
		let mut release = Self {
			source,
			catalog: Some(catalog),
			track: None,
			notify: Waker::from(readable.clone()),
			readable,
			input: kio::Park::default(),
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
				if self.jitter.full(&()) {
					break;
				}
				self.readable.begin(waiter);
				// The park retires a waiter whose registrations overflowed its records.
				let input = self.input.hold(&Context::from_waker(&self.notify));
				let polled = track.poll_read(input);
				self.readable.end(polled.is_pending());
				let frame = match polled {
					Poll::Ready(Ok(Some(frame))) => frame,
					Poll::Ready(Ok(None)) => {
						self.finished = true;
						break;
					}
					Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
					Poll::Pending => break,
				};
				// A sink can leave arrived objects unread. The first notification after
				// an empty read distinguishes that backlog from a late network burst.
				let read = Instant::now();
				let arrival = Arrival {
					arrived: self.readable.arrived().min(read),
					read,
					decode: frame.timestamp,
					restart: track.markers(),
					skip: track.skips(),
					cut: track.cuts(),
					// The output opens on a group, at a random access point; from then on every
					// object goes out on its own, so one dropped late is a gap of its packets only.
					sync: frame.keyframe || self.released,
					leading: false,
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
			}
			Some((name, _)) if *name != m2ts.track => {
				return Err(anyhow::anyhow!("the `m2ts` track moved from `{name}` to `{}`", m2ts.track).into());
			}
			Some(_) => {}
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test(start_paused = true)]
	async fn a_notification_during_an_empty_read_is_kept() {
		let readable = Arc::new(Readable::new());
		let input = kio::Waiter::new(Waker::from(readable.clone()));
		let caller = kio::Waiter::noop();
		let start = Instant::now();
		readable.begin(&caller);
		tokio::time::advance(Duration::from_millis(20)).await;
		input.waker().wake_by_ref();
		tokio::time::advance(Duration::from_millis(20)).await;
		readable.end(true);
		assert_eq!(readable.arrived(), start + Duration::from_millis(20));
		readable.begin(&caller);
		readable.end(true);
		assert_eq!(readable.arrived(), start + Duration::from_millis(40));
	}

	#[tokio::test(start_paused = true)]
	async fn observing_a_stalled_sink_keeps_no_caller_alive() {
		use std::sync::atomic::{AtomicUsize, Ordering};
		struct Caller(AtomicUsize);
		impl Wake for Caller {
			fn wake(self: Arc<Self>) {
				self.0.fetch_add(1, Ordering::Relaxed);
			}
		}
		let readable = Arc::new(Readable::new());
		let input = kio::Waiter::new(Waker::from(readable.clone()));
		let wakes = Arc::new(Caller(AtomicUsize::new(0)));
		let caller = kio::Waiter::new(Waker::from(wakes.clone()));
		readable.begin(&caller);
		readable.end(true);
		drop(caller);
		assert_eq!(Arc::strong_count(&wakes), 1);
		tokio::time::advance(Duration::from_millis(20)).await;
		let arrived = Instant::now();
		input.waker().wake_by_ref();
		tokio::time::advance(Duration::from_secs(1)).await;
		assert_eq!(readable.arrived(), arrived);
		assert_eq!(wakes.0.load(Ordering::Relaxed), 0);
	}
}
