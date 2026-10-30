//! [`Follower`]: an [`Export`] driven by the publisher instances serving its path, as a player is.

use std::future::Future;
use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use moq_clock::Instant;

use super::{Export, catalog};
use crate::container::Frame;
use crate::follow::{Event, Instances};

#[cfg(not(target_family = "wasm"))]
type Subscribing<E> = Pin<Box<dyn Future<Output = crate::Result<crate::catalog::Consumer<E>>> + Send>>;
#[cfg(target_family = "wasm")]
type Subscribing<E> = Pin<Box<dyn Future<Output = crate::Result<crate::catalog::Consumer<E>>>>>;

/// What the instances say serves the exported path.
enum Serving {
	/// The instance being exported, with no change since the export carried on into it.
	Ours,
	/// Nothing: the instance handed out last went.
	Gone,
	/// The instance being exported, handed out again since: it came back after a gap.
	Return(moq_net::broadcast::Consumer),
	/// Another instance: another epoch, or any return of an epochless route.
	Other(moq_net::broadcast::Consumer),
}

/// Subscribing to an instance's catalog, to carry the export on into it.
struct Subscription<E: catalog::Catalog> {
	target: moq_net::broadcast::Consumer,
	catalog: Subscribing<E>,
}

/// Why the export awaits the first catalog snapshot of the instance it carried on into.
enum Missing {
	/// Carried on after the export ended with `end`: an instance that ends before it serves
	/// leaves the export waiting for another until `deadline`.
	Ended { end: crate::Result<()>, deadline: Instant },
	/// A replacement switched to mid-stream, which ending before it serves is a failure.
	Switch,
}

enum State<E: catalog::Catalog> {
	/// Exporting, while switching to a replacement when stitching one, and awaiting the
	/// catalog's first snapshot after carrying on into an instance.
	Running {
		awaiting: Option<Missing>,
		switching: Option<Subscription<E>>,
	},
	/// The export ended with `end`, waiting until `deadline` for an instance to carry on into.
	Settling { end: crate::Result<()>, deadline: Instant },
	/// The export ended with `end`, and an instance serving the path is being subscribed.
	Resuming {
		subscription: Subscription<E>,
		end: crate::Result<()>,
		deadline: Instant,
	},
	/// Nothing follows.
	Done,
}

/// Carries an [`Export`] across its broadcast's returns, following the publisher instances
/// serving its path through [`Instances`].
///
/// The same publisher instance (the same epoch) returning within the [follow](Self::with_follow)
/// continues the stream. Another instance taking the path fails with
/// [`Error::Replaced`](crate::Error::Replaced), unless [stitching](Self::with_stitch), which
/// switches the program to it at once. Subscriptions are sticky, so without stitching a
/// replaced publisher that stays up keeps the export until it ends.
///
/// A track failure is followed only when its cause says the publisher left: the route serving
/// it was lost (`Unroutable`, which a moq-lite relay resets a copy with once its upstream
/// session ends), the session carrying it ended, or a moq-transport peer ended it with a code
/// it does not register, since PUBLISH_DONE has none for a lost route. Any other failure is
/// the export's own and fails here at once.
///
/// An instance that comes back is awaited until its catalog serves a first snapshot, with no
/// deadline but its own end. A return that refuses its catalog waits for the next change, and
/// the follow running out reports that refusal. A refusal to resolve the path at all fails at
/// once, as it does for every [`Instances`] reader.
///
/// An epochless route has no instance to match, so any return of one is a replacement.
pub struct Follower<E: catalog::Catalog = ()> {
	export: Export<crate::catalog::Consumer<E>>,
	state: State<E>,
	instances: Instances,
	path: moq_net::Path,
	serving: Serving,
	/// Taking the instances on hand when built, which replay the route the export resolved.
	priming: bool,
	/// The origin closed, so nothing more is announced.
	closed: bool,
	/// Why the last return refused its catalog, reported instead of the end if the follow runs out.
	failed: Option<crate::Error>,
	/// The instance serving now refused its catalog: nothing is followed until the next change.
	refused: bool,
	follow: Duration,
	stitch: bool,
	timer: Option<Pin<Box<moq_clock::Sleep>>>,
}

impl<E: catalog::Catalog + 'static> Follower<E> {
	/// Follow the path `export` reads, from the broadcast it resolved.
	///
	/// Fails when the export's origin can never cover its path.
	pub fn new(export: Export<crate::catalog::Consumer<E>>) -> crate::Result<Self> {
		let source = export.source();
		let instances = Instances::new(source.origin(), source.path(), None)?;
		let mut follower = Self {
			path: source.path().clone(),
			export,
			state: State::Running {
				awaiting: None,
				switching: None,
			},
			instances,
			serving: Serving::Ours,
			priming: true,
			closed: false,
			failed: None,
			refused: false,
			follow: Duration::ZERO,
			stitch: false,
			timer: None,
		};
		// Take the instance on hand now, before a gap can fold into it: one handed out at once is
		// the route the export resolved, so only a later one is a return.
		follower.poll_instances(&kio::Waiter::noop())?;
		follower.priming = false;
		Ok(follower)
	}

	/// Wait up to `follow` after the broadcast ends for the same instance, or with
	/// [stitching](Self::with_stitch) a replacement, to come back. Defaults to
	/// [`Duration::ZERO`], which carries on only into an instance already serving the path.
	pub fn with_follow(mut self, follow: Duration) -> Self {
		self.follow = follow;
		self
	}

	/// Follow another publisher instance taking the path, as a full program switch (see
	/// [`Export::follow`]). Defaults to `false`.
	pub fn with_stitch(mut self, stitch: bool) -> Self {
		self.stitch = stitch;
		self
	}

	/// The export being carried on.
	pub fn export(&self) -> &Export<crate::catalog::Consumer<E>> {
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
				State::Running {
					mut awaiting,
					mut switching,
				} => {
					self.poll_instances(waiter)?;
					if self.poll_switch(waiter, &mut switching)? {
						awaiting = Some(Missing::Switch);
					}
					let next = self.export.poll_next(waiter);
					awaiting = awaiting.filter(|_| !self.export.cataloged());
					let end = match next {
						Poll::Ready(Ok(Some(frame))) => {
							self.state = State::Running { awaiting, switching };
							return Poll::Ready(Ok(Some(frame)));
						}
						Poll::Ready(Err(err)) if !left(&err) => {
							tracing::warn!(path = %self.path, %err, "export failed, so not following");
							return Poll::Ready(Err(err));
						}
						Poll::Ready(end) => end.map(|_| ()),
						Poll::Pending => {
							// An instance that goes with its catalog still open never served.
							if let Some(missing) = awaiting.take_if(|_| matches!(self.serving, Serving::Gone)) {
								self.state = self.withdrawn(missing);
								continue;
							}
							self.state = State::Running { awaiting, switching };
							return Poll::Pending;
						}
					};
					// Ending abandons the catalog switch; a return of this instance reuses its source.
					if switching.is_some() {
						self.export.source().priority(0)?;
					}
					// Nor did one whose catalog ended before its first snapshot.
					if let Some(missing) = awaiting {
						self.state = self.withdrawn(missing);
						continue;
					}
					self.following(&end);
					self.state = State::Settling {
						end,
						deadline: moq_clock::now() + self.follow,
					};
				}
				State::Settling { end, deadline } => {
					self.poll_instances(waiter)?;
					let target = match &self.serving {
						_ if self.refused => None,
						Serving::Return(target) => Some(target.clone()),
						Serving::Other(target) if self.stitch => Some(target.clone()),
						Serving::Other(_) => return Poll::Ready(Err(crate::Error::Replaced(self.path.to_string()))),
						Serving::Ours | Serving::Gone => None,
					};
					if let Some(target) = target {
						self.state = State::Resuming {
							subscription: self.subscribe(target),
							end,
							deadline,
						};
						continue;
					}
					if self.closed || self.elapsed(waiter, deadline) {
						return Poll::Ready(self.gone(end));
					}
					self.state = State::Settling { end, deadline };
					return Poll::Pending;
				}
				State::Resuming {
					mut subscription,
					end,
					deadline,
				} => {
					// Whatever the subscription waits on, the instances say what serves the path
					// now: rather than sit out one that went, decide afresh from there.
					if self.poll_instances(waiter)? {
						tracing::debug!(path = %self.path, "instance changed while subscribing, deciding again");
						self.state = State::Settling { end, deadline };
						continue;
					}
					match waiter.poll_future(subscription.catalog.as_mut()) {
						Poll::Ready(Ok(catalog)) => {
							self.carry_on(&subscription.target, catalog)?;
							self.state = State::Running {
								awaiting: Some(Missing::Ended { end, deadline }),
								switching: None,
							};
						}
						// Refused, or gone before the instances say so: nothing is followed until
						// the next change. Only the follow running out reports a refusal.
						Poll::Ready(Err(err)) => {
							tracing::warn!(path = %self.path, %err, "catalog subscription failed, still waiting");
							if !left(&err) {
								self.failed = Some(err);
							}
							self.refused = true;
							self.state = State::Settling { end, deadline };
						}
						Poll::Pending => {
							self.state = State::Resuming {
								subscription,
								end,
								deadline,
							};
							return Poll::Pending;
						}
					}
				}
			}
		}
	}

	/// Drive a switch to the replacement serving the path when stitching, returning whether the
	/// export carried on into it.
	///
	/// The old instance keeps playing until the replacement's catalog is subscribed, however long
	/// that takes, and a replacement that goes first is no switch. The old program's track
	/// subscriptions yield priority while acquiring the replacement.
	fn poll_switch(&mut self, waiter: &kio::Waiter, switching: &mut Option<Subscription<E>>) -> crate::Result<bool> {
		let target = match &self.serving {
			Serving::Other(target) if self.stitch && !self.refused => target,
			_ => {
				if switching.take().is_some() {
					self.export.source().priority(0)?;
				}
				return Ok(false);
			}
		};
		if !switching
			.as_ref()
			.is_some_and(|switching| switching.target.is_clone(target))
		{
			tracing::info!(path = %self.path, "broadcast replaced, switching the program to the new instance");
			self.export.source().priority(-1)?;
			*switching = Some(self.subscribe(target.clone()));
		}
		let subscription = switching.as_mut().expect("just set");
		match waiter.poll_future(subscription.catalog.as_mut()) {
			Poll::Ready(Ok(catalog)) => {
				let subscription = switching.take().expect("just polled");
				self.carry_on(&subscription.target, catalog)?;
				Ok(true)
			}
			// Gone before the instances say so: no switch until the next change.
			Poll::Ready(Err(err)) if left(&err) => {
				tracing::warn!(path = %self.path, %err, "replacement went before its catalog subscribed");
				*switching = None;
				self.refused = true;
				self.export.source().priority(0)?;
				Ok(false)
			}
			// The replacement would not serve, so the switch fails loud.
			Poll::Ready(Err(err)) => Err(err),
			Poll::Pending => Ok(false),
		}
	}

	/// Carry the export on into `target`, whose `catalog` is subscribed.
	fn carry_on(
		&mut self,
		target: &moq_net::broadcast::Consumer,
		catalog: crate::catalog::Consumer<E>,
	) -> crate::Result<()> {
		self.export.followed(target, catalog, self.follow)?;
		self.serving = Serving::Ours;
		self.failed = None;
		tracing::info!(
			path = %self.path,
			epoch = self.export.instance().map(tracing::field::display),
			"exporting broadcast"
		);
		Ok(())
	}

	/// Apply every change on hand, returning whether any came through.
	fn poll_instances(&mut self, waiter: &kio::Waiter) -> crate::Result<bool> {
		let mut any = false;
		while !self.closed {
			match self.instances.poll_next(waiter) {
				Poll::Ready(Ok(Event::Start(broadcast))) => {
					self.serving = match self.export.instance() {
						// An epochless route has no instance to match, so only the broadcast
						// being exported is its own.
						None if broadcast.is_clone(&self.export.source().broadcast()) => Serving::Ours,
						// Even the same broadcast handed out again is a return: the origin hands
						// one out only for an announcement that brings it.
						Some(epoch) if broadcast.info().epoch.as_ref() == Some(epoch) => match self.priming {
							true => Serving::Ours,
							false => Serving::Return(broadcast),
						},
						_ => Serving::Other(broadcast),
					};
				}
				Poll::Ready(Ok(Event::End)) => self.serving = Serving::Gone,
				Poll::Ready(Err(crate::Error::Moq(moq_net::Error::Closed))) => self.closed = true,
				Poll::Ready(Err(err)) => return Err(err),
				Poll::Pending => break,
			}
			any = true;
		}
		self.refused &= !any;
		Ok(any)
	}

	/// Begin subscribing to `target`'s catalog.
	fn subscribe(&self, target: moq_net::broadcast::Consumer) -> Subscription<E> {
		let (broadcast, format) = (target.clone(), self.export.catalog_format());
		Subscription {
			target,
			catalog: Box::pin(async move { crate::catalog::Consumer::new(&broadcast, format).await }),
		}
	}

	/// Where the export goes once the instance it carried on into ends before its first snapshot.
	fn withdrawn(&mut self, missing: Missing) -> State<E> {
		tracing::warn!(path = %self.path, "broadcast ended before serving its catalog");
		match missing {
			Missing::Ended { end, deadline } => State::Settling { end, deadline },
			Missing::Switch => State::Settling {
				end: Err(
					anyhow::anyhow!("replacement broadcast `{}` ended before serving its catalog", self.path).into(),
				),
				deadline: moq_clock::now() + self.follow,
			},
		}
	}

	fn following(&self, end: &crate::Result<()>) {
		if self.follow.is_zero() {
			return;
		}
		match end {
			Ok(()) => {
				tracing::info!(path = %self.path, follow = ?self.follow, "broadcast finished, waiting for it to return")
			}
			Err(err) => {
				tracing::warn!(path = %self.path, %err, follow = ?self.follow, "broadcast ended, waiting for it to return")
			}
		}
	}

	/// How the export ends once the follow runs out with nothing back: with why the last
	/// return refused its catalog, if one did, else as the broadcast ended.
	fn gone(&mut self, end: crate::Result<()>) -> crate::Result<Option<Frame>> {
		tracing::info!(path = %self.path, follow = ?self.follow, "broadcast did not return");
		match self.failed.take() {
			Some(err) => Err(err),
			None => end.map(|()| None),
		}
	}

	/// Whether `at` has come, registering `waiter` to wake then if not.
	fn elapsed(&mut self, waiter: &kio::Waiter, at: Instant) -> bool {
		if moq_clock::now() >= at {
			return true;
		}
		let timer = self.timer.get_or_insert_with(|| Box::pin(moq_clock::sleep_until(at)));
		if timer.deadline() != at {
			timer.as_mut().reset(at);
		}
		waiter.poll_future(timer.as_mut()).is_ready()
	}
}

/// Whether a failure says the publisher left rather than the export failing: the route serving
/// the broadcast was lost, the session carrying it ended, or a moq-transport peer named no cause.
pub(super) fn left(err: &crate::Error) -> bool {
	let (crate::Error::Moq(err) | crate::Error::Hang(hang::Error::Moq(err))) = err else {
		return false;
	};
	use moq_net::{Error, StreamError};
	matches!(
		err,
		Error::Unroutable
			| Error::Stream(StreamError::Unroutable | StreamError::Session(_))
			| Error::Session(_)
			| Error::Transport(_)
			| Error::IdleTimeout
			| Error::Remote(_)
	)
}
