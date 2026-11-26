//! [`Follower`]: an [`Export`] or a [`Release`](super::Release) driven by the publisher identities
//! serving its path, as a player is.

use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use kio::clock::Instant;

use super::error::malformed;
use super::{Export, catalog};
use crate::catalog::CatalogFormat;
use crate::container::Frame;

/// What a [`Follower`] carries across its path's identities: an [`Export`], or a
/// [`Release`](super::Release) of a passthrough track.
pub trait Followed: sealed::Followed {}

impl<T: sealed::Followed> Followed for T {}

pub(super) mod sealed {
	use super::*;

	/// What a [`Follower`](super::Follower) asks of the export it carries on.
	pub trait Followed {
		/// The catalog extension the export reads.
		type Ext: catalog::Catalog + 'static;

		/// The next frame, or `None` once the broadcast ends.
		fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Frame>>>;

		/// Whether the catalog last subscribed has delivered a snapshot.
		fn cataloged(&self) -> bool;

		/// The source the export reads.
		fn source(&self) -> &crate::Source;

		/// The identity of the broadcast being exported, `None` for one not resolved through
		/// an origin.
		fn identity(&self) -> Option<&moq_net::Identity>;

		/// Why the publisher left, while the export still plays out what it buffered before
		/// reporting it.
		fn departed(&self) -> Option<&crate::Error>;

		/// The identity carried on into while playing out went before serving: stop reading it,
		/// and report `err` once what was buffered has gone out.
		fn depart(&mut self, err: crate::Error) -> crate::Result<()>;

		/// Carry on into `broadcast`, whose `catalog` is subscribed, waiting up to `follow` for
		/// another identity's program to become usable.
		fn followed(
			&mut self,
			broadcast: &moq_net::broadcast::Consumer,
			catalog: crate::catalog::Consumer<Self::Ext>,
			follow: Duration,
		) -> crate::Result<()>;

		/// The format of the catalog being read.
		fn catalog_format(&self) -> CatalogFormat;
	}

	impl<E: catalog::Catalog + 'static> Followed for Export<crate::catalog::Consumer<E>> {
		type Ext = E;

		fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Frame>>> {
			Export::poll_next(self, waiter)
		}

		fn cataloged(&self) -> bool {
			Export::cataloged(self)
		}

		fn source(&self) -> &crate::Source {
			Export::source(self)
		}

		fn identity(&self) -> Option<&moq_net::Identity> {
			Export::identity(self)
		}

		fn departed(&self) -> Option<&crate::Error> {
			Export::departed(self)
		}

		fn depart(&mut self, err: crate::Error) -> crate::Result<()> {
			Export::depart(self, err)
		}

		fn followed(
			&mut self,
			broadcast: &moq_net::broadcast::Consumer,
			catalog: crate::catalog::Consumer<E>,
			follow: Duration,
		) -> crate::Result<()> {
			Export::followed(self, broadcast, catalog, follow)
		}

		fn catalog_format(&self) -> CatalogFormat {
			Export::catalog_format(self)
		}
	}
}

/// What the identities say serves the exported path.
enum Serving {
	/// The identity being exported, with no change since the export carried on into it.
	Ours,
	/// Nothing: the identity handed out last went.
	Gone,
	/// The identity being exported, handed out again since: it came back after a gap.
	Return(moq_net::broadcast::Consumer),
	/// Another stream: a broadcast of another identity.
	Other(moq_net::broadcast::Consumer),
	/// A return or another stream that refused its catalog: nothing is followed until the
	/// next change.
	Refused,
}

/// Subscribing to an identity's catalog, to carry the export on into it.
struct Subscription<E: catalog::Catalog> {
	target: moq_net::broadcast::Consumer,
	catalog: crate::catalog::Subscribing<E>,
}

/// Why the export awaits the first catalog snapshot of the identity it carried on into.
enum Missing {
	/// Carried on after the export ended with `end`: an identity that ends before it serves
	/// leaves the export waiting for another until `deadline`.
	Ended { end: crate::Result<()>, deadline: Instant },
	/// A replacement switched to mid-stream, which ending before it serves is a failure.
	Switch,
	/// The same identity returned while the export played out what it buffered after the
	/// publisher left with this error, which an end before it serves reports after all.
	Return(crate::Error),
}

enum State<E: catalog::Catalog> {
	/// Exporting, while switching to a replacement when stitching one, and awaiting the
	/// catalog's first snapshot after carrying on into an identity.
	Running {
		awaiting: Option<Missing>,
		switching: Option<Subscription<E>>,
	},
	/// The export ended with `end`, waiting until `deadline` for an identity to carry on into.
	Settling { end: crate::Result<()>, deadline: Instant },
	/// The export ended with `end`, and an identity serving the path is being subscribed.
	Resuming {
		subscription: Subscription<E>,
		end: crate::Result<()>,
		deadline: Instant,
	},
	/// Nothing follows.
	Done,
}

/// Carries an [`Export`], or a [`Release`](super::Release) of a passthrough track, across its
/// broadcast's returns, following the publisher identities serving its path through
/// [`Following`](moq_net::origin::Following).
///
/// The same stream (the same identity) returning within the [follow](Self::with_follow)
/// continues the export. Another identity taking the path fails with
/// [`Error::Replaced`](crate::Error::Replaced), unless [stitching](Self::with_stitch), which
/// switches the program to it at once. Subscriptions are sticky, so without stitching a
/// replaced publisher that stays up keeps the export until it ends.
///
/// A track failure is followed only when its cause says the publisher left: the source failed
/// to deliver, as [`moq_net::Error::retry`] answers (a lost route, an ended session, a cancel,
/// or a going away), or a moq-transport peer ended it with a code it does not register, since
/// PUBLISH_DONE has none for a lost route. Any other failure is the export's own and fails
/// here at once.
///
/// An identity that comes back is awaited until its catalog serves a first snapshot, with no
/// deadline but its own end. A return that refuses its catalog waits for the next change, and
/// the follow running out reports that refusal. A refusal to resolve the path at all fails at
/// once, as it does for every [`Following`](moq_net::origin::Following) reader.
pub struct Follower<T: Followed = Export<crate::catalog::Consumer<()>>> {
	export: T,
	state: State<T::Ext>,
	identities: moq_net::origin::Following,
	path: moq_net::Path,
	serving: Serving,
	/// Taking the identities on hand when built, which replay the route the export resolved.
	priming: bool,
	/// The origin closed, so nothing more is announced.
	closed: bool,
	/// Why the last return refused its catalog, reported instead of the end if the follow runs out.
	failed: Option<crate::Error>,
	follow: Duration,
	stitch: bool,
	timer: Option<Pin<Box<kio::clock::Sleep>>>,
}

impl<T: Followed> Follower<T> {
	/// Follow the path `export` reads, from the broadcast it resolved.
	///
	/// Fails when the export's origin can never cover its path.
	pub fn new(export: T) -> crate::Result<Self> {
		let source = export.source();
		let identities = source.origin().follow(source.path(), None)?;
		let mut follower = Self {
			path: source.path().clone(),
			export,
			state: State::Running {
				awaiting: None,
				switching: None,
			},
			identities,
			serving: Serving::Ours,
			priming: true,
			closed: false,
			failed: None,
			follow: Duration::ZERO,
			stitch: false,
			timer: None,
		};
		// Take the identity on hand now, before a gap can fold into it: one handed out at once is
		// the route the export resolved, so only a later one is a return.
		follower.poll_identities(&kio::Waiter::noop())?;
		follower.priming = false;
		Ok(follower)
	}

	/// Wait up to `follow` after the broadcast ends for the same identity, or with
	/// [stitching](Self::with_stitch) a replacement, to come back. Defaults to
	/// [`Duration::ZERO`], which carries on only into an identity already serving the path.
	pub fn with_follow(mut self, follow: Duration) -> Self {
		self.follow = follow;
		self
	}

	/// Follow another publisher identity taking the path, as a full program switch (see
	/// [`Export::follow`]). Defaults to `false`.
	pub fn with_stitch(mut self, stitch: bool) -> Self {
		self.stitch = stitch;
		self
	}

	/// The export being carried on.
	pub fn export(&self) -> &T {
		&self.export
	}

	/// Get the next muxed frame, or `None` once the broadcast ends with nothing to follow.
	#[cfg_attr(feature = "async", kio::wrapper)]
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
					self.poll_identities(waiter)?;
					if let Some(missing) = self.poll_switch(waiter, &mut switching)? {
						awaiting = Some(missing);
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
							// An identity that goes with its catalog still open never served.
							if let Some(missing) = awaiting.take_if(|_| matches!(self.serving, Serving::Gone)) {
								self.state = self.withdrawn(missing)?;
								continue;
							}
							// The publisher left during this poll, so a return already on hand is
							// carried on into now: nothing else wakes the follower for it.
							if switching.is_none()
								&& matches!(self.serving, Serving::Return(_))
								&& self.export.departed().is_some()
							{
								self.state = State::Running { awaiting, switching };
								continue;
							}
							self.state = State::Running { awaiting, switching };
							return Poll::Pending;
						}
					};
					// Ending abandons the catalog switch; a return of this identity reuses its source.
					if switching.is_some() {
						self.export.source().priority(0)?;
					}
					// Nor did one whose catalog ended before its first snapshot.
					if let Some(missing) = awaiting {
						self.state = match missing {
							// What was buffered has gone out too, so the export ends as the publisher left.
							Missing::Return(err) => {
								tracing::warn!(path = %self.path, "broadcast ended before serving its catalog");
								State::Settling {
									end: Err(err),
									deadline: kio::clock::now().saturating_add(self.follow),
								}
							}
							missing => self.withdrawn(missing)?,
						};
						continue;
					}
					self.following(&end);
					self.state = State::Settling {
						end,
						deadline: kio::clock::now().saturating_add(self.follow),
					};
				}
				State::Settling { end, deadline } => {
					self.poll_identities(waiter)?;
					let target = match &self.serving {
						Serving::Return(target) => Some(target.clone()),
						Serving::Other(target) if self.stitch => Some(target.clone()),
						Serving::Other(_) => return Poll::Ready(Err(crate::Error::Replaced(self.path.to_string()))),
						Serving::Ours | Serving::Gone | Serving::Refused => None,
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
					// Whatever the subscription waits on, the identities say what serves the path
					// now: rather than sit out one that went, decide afresh from there.
					if self.poll_identities(waiter)? {
						tracing::debug!(path = %self.path, "identity changed while subscribing, deciding again");
						self.state = State::Settling { end, deadline };
						continue;
					}
					match subscription.catalog.poll_ok(waiter) {
						Poll::Ready(Ok(catalog)) => {
							self.carry_on(&subscription.target, catalog)?;
							self.state = State::Running {
								awaiting: Some(Missing::Ended { end, deadline }),
								switching: None,
							};
						}
						// Refused, or gone before the identities say so: nothing is followed until
						// the next change. Only the follow running out reports a refusal.
						Poll::Ready(Err(err)) => {
							tracing::warn!(path = %self.path, %err, "catalog subscription failed, still waiting");
							if !left(&err) {
								self.failed = Some(err);
							}
							self.serving = Serving::Refused;
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

	/// Drive a switch to the replacement serving the path when stitching, or a carry on into the
	/// same identity returning while the export plays out what it buffered after the publisher
	/// left, returning why the export awaits the first snapshot once it carried on.
	///
	/// When stitching, the old identity keeps playing until the replacement's catalog is
	/// subscribed, however long that takes, and a replacement that goes first is no switch. The
	/// old program's track subscriptions yield priority while acquiring the replacement.
	///
	/// A return is carried on into as soon as its catalog is subscribed, rather than once the
	/// buffered output has gone out, so each track reads on from where it stopped before that
	/// media is due ([`Export::follow`]).
	fn poll_switch(
		&mut self,
		waiter: &kio::Waiter,
		switching: &mut Option<Subscription<T::Ext>>,
	) -> crate::Result<Option<Missing>> {
		let (target, missing) = match &self.serving {
			Serving::Other(target) if self.stitch => (target, None),
			Serving::Return(target) => match self.export.departed() {
				Some(err) => (target, Some(err.clone())),
				None => return self.abandon(switching),
			},
			_ => return self.abandon(switching),
		};
		if !switching
			.as_ref()
			.is_some_and(|switching| switching.target.is_clone(target))
		{
			let target = target.clone();
			match missing {
				// A replacement being acquired may have lowered the priority; the return reads on.
				Some(_) => {
					tracing::info!(path = %self.path, "broadcast returned, carrying on while the old output plays out");
					self.export.source().priority(0)?;
				}
				None => {
					tracing::info!(path = %self.path, "broadcast replaced, switching the program to the new identity");
					self.export.source().priority(-1)?;
				}
			}
			*switching = Some(self.subscribe(target));
		}
		let subscription = switching.as_mut().expect("just set");
		match subscription.catalog.poll_ok(waiter) {
			Poll::Ready(Ok(catalog)) => {
				let subscription = switching.take().expect("just polled");
				self.carry_on(&subscription.target, catalog)?;
				Ok(Some(missing.map_or(Missing::Switch, Missing::Return)))
			}
			// Gone before the identities say so: no switch until the next change.
			Poll::Ready(Err(err)) if left(&err) => {
				tracing::warn!(path = %self.path, %err, "identity went before its catalog subscribed");
				self.serving = Serving::Refused;
				self.abandon(switching)
			}
			// A return that refuses its catalog waits for the next change, as one after the
			// output ended does. Only the follow running out reports the refusal.
			Poll::Ready(Err(err)) if missing.is_some() => {
				tracing::warn!(path = %self.path, %err, "catalog subscription failed, still waiting");
				self.failed = Some(err);
				self.serving = Serving::Refused;
				self.abandon(switching)
			}
			// The replacement would not serve, so the switch fails loud.
			Poll::Ready(Err(err)) => Err(err),
			Poll::Pending => Ok(None),
		}
	}

	/// Drop the catalog subscription [`Self::poll_switch`] holds, if any, giving the old program's
	/// tracks back their priority.
	fn abandon(&mut self, switching: &mut Option<Subscription<T::Ext>>) -> crate::Result<Option<Missing>> {
		if switching.take().is_some() {
			self.export.source().priority(0)?;
		}
		Ok(None)
	}

	/// Carry the export on into `target`, whose `catalog` is subscribed.
	fn carry_on(
		&mut self,
		target: &moq_net::broadcast::Consumer,
		catalog: crate::catalog::Consumer<T::Ext>,
	) -> crate::Result<()> {
		self.export.followed(target, catalog, self.follow)?;
		self.serving = Serving::Ours;
		self.failed = None;
		tracing::info!(
			path = %self.path,
			identity = ?self.export.identity(),
			"exporting broadcast"
		);
		Ok(())
	}

	/// Apply every change on hand, returning whether any came through.
	fn poll_identities(&mut self, waiter: &kio::Waiter) -> crate::Result<bool> {
		let mut any = false;
		while !self.closed {
			match self.identities.poll_available(waiter) {
				Poll::Ready(Ok(Some(broadcast))) => {
					self.serving = match self.export.identity() {
						// A broadcast not resolved through an origin has no identity to match,
						// so only the broadcast being exported is its own.
						None if broadcast.is_clone(&self.export.source().broadcast()) => Serving::Ours,
						// Even the same broadcast handed out again is a return: the origin hands
						// one out only for an announcement that brings it.
						Some(identity) if broadcast.info().identity.as_ref() == Some(identity) => match self.priming {
							true => Serving::Ours,
							false => Serving::Return(broadcast),
						},
						_ => Serving::Other(broadcast),
					};
				}
				Poll::Ready(Ok(None)) => self.serving = Serving::Gone,
				Poll::Ready(Err(moq_net::Error::Closed)) => self.closed = true,
				Poll::Ready(Err(err)) => return Err(err.into()),
				Poll::Pending => break,
			}
			any = true;
		}
		Ok(any)
	}

	/// Begin subscribing to `target`'s catalog.
	fn subscribe(&self, target: moq_net::broadcast::Consumer) -> Subscription<T::Ext> {
		Subscription {
			catalog: crate::catalog::Subscribing::new(&target, self.export.catalog_format()),
			target,
		}
	}

	/// Where the export goes once the identity it carried on into ends before its first snapshot.
	fn withdrawn(&mut self, missing: Missing) -> crate::Result<State<T::Ext>> {
		tracing::warn!(path = %self.path, "broadcast ended before serving its catalog");
		Ok(match missing {
			Missing::Ended { end, deadline } => State::Settling { end, deadline },
			Missing::Switch => State::Settling {
				end: Err(malformed!("replacement broadcast `{}` ended before serving its catalog", self.path).into()),
				deadline: kio::clock::now().saturating_add(self.follow),
			},
			// What was buffered still goes out, and then the export ends as the publisher left.
			Missing::Return(err) => {
				self.export.depart(err)?;
				State::Running {
					awaiting: None,
					switching: None,
				}
			}
		})
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
		if kio::clock::now() >= at {
			return true;
		}
		let timer = self.timer.get_or_insert_with(|| Box::pin(kio::clock::sleep_until(at)));
		if timer.deadline() != at {
			timer.as_mut().reset(at);
		}
		waiter.poll_future(timer.as_mut()).is_ready()
	}
}

/// Whether a failure says the publisher left rather than the export failing: the source failed
/// to deliver (see [`moq_net::Error::retry`]), or a moq-transport peer named no cause.
///
/// moq-transport's PUBLISH_DONE has no code for a lost route, so a relay's lost upstream and a
/// publisher's own failure both end with an unregistered one, which `retry` cannot answer for.
pub(super) fn left(err: &crate::Error) -> bool {
	let (crate::Error::Moq(err) | crate::Error::Hang(hang::Error::Moq(err))) = err else {
		return false;
	};
	err.retry() != moq_net::Retry::Never || matches!(err, moq_net::Error::Remote(_))
}
