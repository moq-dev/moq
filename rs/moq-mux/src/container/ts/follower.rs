//! [`Follower`]: an [`Export`] driven by its path's announcements, as a player is.

use std::future::Future;
use std::pin::Pin;
use std::task::{Poll, ready};
use std::time::Duration;

use web_async::time::Instant;

use super::{Export, catalog};
use crate::container::Frame;

/// How long an export failure waits for its broadcast to go before it counts as the export's own.
///
/// A killed publisher's tracks can error just before its route is withdrawn, so the two need not
/// land together.
const CLOSE_GRACE: Duration = Duration::from_secs(1);

/// The subscriptions carrying an export on into another broadcast.
type Resolved<E> = crate::Result<(moq_net::broadcast::Consumer, crate::catalog::Consumer<E>)>;

#[cfg(not(target_family = "wasm"))]
type Resolving<E> = Pin<Box<dyn Future<Output = Resolved<E>> + Send>>;
#[cfg(target_family = "wasm")]
type Resolving<E> = Pin<Box<dyn Future<Output = Resolved<E>>>>;

/// What the announcements say serves the exported path.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Serving {
	/// Nothing: every route went.
	Gone,
	/// The publisher instance being exported.
	Ours,
	/// Another instance: a newer epoch, or any return of an epochless route.
	Other(Option<moq_net::Epoch>),
}

enum State<E: catalog::Catalog> {
	/// Exporting.
	Running,
	/// The export ended with `end`, waiting for the announcements to say what follows.
	Settling {
		end: crate::Result<()>,
		/// Until when a failure with the broadcast still announced waits for it to go.
		grace: Option<Instant>,
		/// When the linger runs out.
		deadline: Instant,
	},
	/// Resolving the instance serving the path now, to carry the export on into it.
	Following {
		resolving: Resolving<E>,
		/// How the export ended and when the linger runs out, or `None` for a stitch mid-stream.
		settling: Option<(crate::Result<()>, Instant)>,
	},
	/// Nothing follows.
	Done,
}

/// Carries an [`Export`] across its broadcast's returns, following the path's announcements.
///
/// The same publisher instance (the same epoch) returning within the [linger](Self::with_linger)
/// continues the stream. Another instance taking the path fails with
/// [`Error::Replaced`](crate::Error::Replaced), unless [stitching](Self::with_stitch), which
/// switches the program to it at once. Subscriptions are sticky, so without stitching a
/// replaced publisher that stays up keeps the export until it ends. An export that fails
/// while its broadcast stays announced fails here too, without lingering.
///
/// An epochless route has no instance to match, so any return of one is a replacement.
pub struct Follower<E: catalog::Catalog = ()> {
	export: Export<E>,
	state: State<E>,
	announced: moq_net::announce::Follow,
	path: moq_net::PathOwned,
	/// The epoch of the instance being exported.
	epoch: Option<moq_net::Epoch>,
	serving: Serving,
	/// Every route went since the export resolved its broadcast.
	ended: bool,
	/// An announcement arrived since the follower was built.
	started: bool,
	/// The origin closed, so nothing more is announced.
	closed: bool,
	linger: Duration,
	stitch: bool,
	timer: Option<Pin<Box<web_async::time::Sleep>>>,
}

impl<E: catalog::Catalog + 'static> Follower<E> {
	/// Follow the path `export` reads, from the broadcast it resolved.
	///
	/// Fails when the export's origin can never cover its path.
	pub fn new(export: Export<E>) -> crate::Result<Self> {
		let source = export.source();
		let announced = source.origin().follow(source.path())?;
		let path = source.path().clone();
		let epoch = export.instance().cloned();
		let mut follower = Self {
			export,
			state: State::Running,
			announced,
			path,
			epoch,
			serving: Serving::Ours,
			ended: false,
			started: false,
			closed: false,
			linger: Duration::ZERO,
			stitch: false,
			timer: None,
		};
		// Take the start naming the route the export resolved now, before anything else can
		// fold into it.
		follower.poll_announced(&kio::Waiter::noop());
		Ok(follower)
	}

	/// Wait up to `linger` for the same instance to return once the broadcast ends. Defaults to
	/// [`Duration::ZERO`].
	///
	/// It bounds the whole return, catalog subscription included.
	pub fn with_linger(mut self, linger: Duration) -> Self {
		self.linger = linger;
		self
	}

	/// Follow another publisher instance taking the path, as a full program switch (see
	/// [`Export::follow`]). Defaults to `false`.
	pub fn with_stitch(mut self, stitch: bool) -> Self {
		self.stitch = stitch;
		self
	}

	/// The export being carried on.
	pub fn export(&self) -> &Export<E> {
		&self.export
	}

	/// Get the next muxed frame, or `None` once the broadcast ends with nothing to follow.
	pub async fn next(&mut self) -> crate::Result<Option<Frame>> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next muxed frame, registering `waiter` when there is none yet.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Frame>>> {
		let next = self.poll_state(waiter);
		if matches!(next, Poll::Ready(Ok(None) | Err(_))) {
			self.state = State::Done;
		}
		next
	}

	fn poll_state(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Frame>>> {
		loop {
			match std::mem::replace(&mut self.state, State::Done) {
				State::Done => return Poll::Ready(Ok(None)),
				State::Running => {
					self.poll_announced(waiter);
					// Subscriptions are sticky, so a replacement leaves the export on the old
					// instance until it ends, unless asked to switch at once.
					if self.stitch && matches!(self.serving, Serving::Other(_)) {
						tracing::info!(path = %self.path, "broadcast replaced, switching the program to the new instance");
						self.state = State::Following {
							resolving: self.resolve(),
							settling: None,
						};
						continue;
					}
					self.state = State::Running;
					let end = match ready!(self.export.poll_next(waiter)) {
						Ok(Some(frame)) => return Poll::Ready(Ok(Some(frame))),
						Ok(None) => Ok(()),
						Err(err) => Err(err),
					};
					let now = Instant::now();
					// A failure ends the broadcast only if the broadcast goes too.
					let grace = (end.is_err() && !self.ended && self.serving == Serving::Ours)
						.then(|| now + CLOSE_GRACE.min(self.linger));
					if grace.is_none() {
						self.lingering(&end);
					}
					self.state = State::Settling {
						end,
						grace,
						deadline: now + self.linger,
					};
				}
				State::Settling {
					end,
					mut grace,
					deadline,
				} => {
					self.poll_announced(waiter);
					if grace.is_some() && self.serving != Serving::Ours {
						grace = None;
						self.lingering(&end);
					}
					if let Some(at) = grace {
						// One still announced cannot return, so the failure is the export's own.
						if self.closed || self.elapsed(waiter, at) {
							let err = end.expect_err("only a failure waits out the grace");
							tracing::warn!(path = %self.path, %err, "export failed with the broadcast still up, so not lingering");
							return Poll::Ready(Err(err));
						}
						self.state = State::Settling { end, grace, deadline };
						return Poll::Pending;
					}

					let follow = match &self.serving {
						Serving::Ours => self.ended,
						Serving::Other(_) if self.stitch => true,
						Serving::Other(_) => return Poll::Ready(Err(crate::Error::Replaced(self.path.to_string()))),
						Serving::Gone => false,
					};
					if follow {
						self.state = State::Following {
							resolving: self.resolve(),
							settling: Some((end, deadline)),
						};
						continue;
					}
					if self.closed || self.elapsed(waiter, deadline) {
						tracing::info!(path = %self.path, linger = ?self.linger, "broadcast did not return");
						return Poll::Ready(end.map(|()| None));
					}
					self.state = State::Settling { end, grace, deadline };
					return Poll::Pending;
				}
				State::Following {
					mut resolving,
					settling,
				} => {
					// Announcements wait until the export is on the instance they describe.
					if let Poll::Ready(resolved) = waiter.poll_future(resolving.as_mut()) {
						let (broadcast, catalog) = resolved?;
						self.export.followed(&broadcast, catalog)?;
						self.epoch = self.export.instance().cloned();
						self.serving = Serving::Ours;
						self.ended = false;
						tracing::info!(
							path = %self.path,
							epoch = self.epoch.as_ref().map(tracing::field::display),
							"exporting broadcast"
						);
						self.state = State::Running;
						continue;
					}
					if let Some((end, deadline)) = &settling
						&& self.elapsed(waiter, *deadline)
					{
						tracing::info!(path = %self.path, linger = ?self.linger, "broadcast did not return");
						return Poll::Ready(end.clone().map(|()| None));
					}
					self.state = State::Following { resolving, settling };
					return Poll::Pending;
				}
			}
		}
	}

	/// Apply every announcement on hand.
	fn poll_announced(&mut self, waiter: &kio::Waiter) {
		while !self.closed {
			match self.announced.poll_next(waiter) {
				Poll::Ready(Some(event)) => self.apply(event),
				Poll::Ready(None) => self.closed = true,
				Poll::Pending => return,
			}
		}
	}

	fn apply(&mut self, event: moq_net::announce::Event) {
		let first = !std::mem::replace(&mut self.started, true);
		self.serving = match event {
			moq_net::announce::Event::Start(announce) => {
				let epoch = announce.route.epoch;
				// The first start names the route the export resolved, as near as an epochless
				// route can tell.
				match epoch == self.epoch && (epoch.is_some() || first) {
					true => Serving::Ours,
					false => Serving::Other(epoch),
				}
			}
			moq_net::announce::Event::Restart(announce) => Serving::Other(announce.route.epoch),
			moq_net::announce::Event::End(_) => {
				self.ended = true;
				Serving::Gone
			}
			// The same instance over another route, which subscriptions ride out. A gap that
			// ended the export's request comes as an end and a start instead.
			moq_net::announce::Event::Update(_) => return,
		};
	}

	/// Begin resolving the instance serving the path now.
	fn resolve(&self) -> Resolving<E> {
		let epoch = match &self.serving {
			Serving::Other(epoch) => epoch.clone(),
			Serving::Ours | Serving::Gone => self.epoch.clone(),
		};
		let request = self.export.source().origin().request_broadcast(&self.path, epoch);
		let format = self.export.catalog_format();
		Box::pin(async move {
			let broadcast = request.await?;
			let catalog = crate::catalog::Consumer::new(&broadcast, format).await?;
			Ok((broadcast, catalog))
		})
	}

	fn lingering(&self, end: &crate::Result<()>) {
		if self.linger.is_zero() {
			return;
		}
		match end {
			Ok(()) => {
				tracing::info!(path = %self.path, linger = ?self.linger, "broadcast finished, waiting for it to return")
			}
			Err(err) => {
				tracing::warn!(path = %self.path, %err, linger = ?self.linger, "broadcast ended, waiting for it to return")
			}
		}
	}

	/// Whether `at` has come, registering `waiter` to wake then if not.
	fn elapsed(&mut self, waiter: &kio::Waiter, at: Instant) -> bool {
		if Instant::now() >= at {
			return true;
		}
		let timer = self
			.timer
			.get_or_insert_with(|| Box::pin(web_async::time::sleep_until(at)));
		if timer.deadline() != at {
			timer.as_mut().reset(at);
		}
		waiter.poll_future(timer.as_mut()).is_ready()
	}
}
