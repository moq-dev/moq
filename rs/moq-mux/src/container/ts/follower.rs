//! [`Follower`]: an [`Export`] that follows its path across publisher instances.

use std::time::Duration;

use moq_net::AsPath;
use moq_net::announce::Event;
use web_async::time::{Instant, timeout_at};

use super::{Export, catalog};
use crate::catalog::CatalogFormat;
use crate::container::Frame;

/// How long an export failure waits for its broadcast to go before it counts as the export's own.
///
/// A killed publisher's tracks can error just before its route is withdrawn, so the two need not
/// land together.
const CLOSE_GRACE: Duration = Duration::from_secs(1);

/// An MPEG-TS export of the broadcast at one path, driven by the path's announcements the way a
/// player is.
///
/// The same publisher instance (an equal epoch) coming back within the
/// [linger](Self::with_linger) continues the stream. Another instance taking the path ends it with
/// [`Error::Replaced`](crate::Error::Replaced), or with [stitch](Self::with_stitch) switches the
/// program to it at once (see [`Export::follow`]). Subscriptions are sticky, so without stitch a
/// replaced publisher that stays up keeps the export until it ends.
///
/// Like [`Export::with_ts`], it carries the `mpegts` streams the catalog lists verbatim.
pub struct Follower {
	/// Taken only while [`Export::follow`] carries it on, and gone once the export ends.
	export: Option<Export<catalog::Ext>>,
	watch: Watch,
	linger: Duration,
	stitch: bool,
}

impl Follower {
	/// Wait for a broadcast to serve `path` on `origin`, and export it with its `format` catalog.
	///
	/// Fails with [`moq_net::Error::Unauthorized`] for a path outside the origin's scope, and
	/// with [`moq_net::Error::Closed`] when the origin closes first.
	pub async fn new(
		origin: moq_net::origin::Consumer,
		path: impl AsPath,
		format: CatalogFormat,
	) -> Result<Self, crate::Error> {
		let path = path.as_path().to_owned();
		let mut watch = Watch {
			follow: origin.follow(&path)?,
			origin,
			path,
			epoch: None,
			serving: Serving::Gone,
			ended: false,
			closed: false,
		};
		while watch.serving == Serving::Gone {
			if !watch.changed().await {
				return Err(moq_net::Error::Closed.into());
			}
		}
		let broadcast = watch.resolve().await?;
		let source = crate::Source::new(watch.origin.clone(), &watch.path);
		let export = Export::build_on(source, &broadcast, format).await?;
		Ok(Self {
			export: Some(export),
			watch,
			linger: Duration::ZERO,
			stitch: false,
		})
	}

	/// See [`Export::with_delay`].
	pub fn with_delay(mut self, delay: Duration) -> Self {
		self.export = self.export.map(|export| export.with_delay(delay));
		self
	}

	/// See [`Export::with_mux_rate`].
	pub fn with_mux_rate(mut self, mux_rate: u64) -> Self {
		self.export = self.export.map(|export| export.with_mux_rate(mux_rate));
		self
	}

	/// Wait up to `linger` for the same publisher instance to come back once the broadcast ends,
	/// carrying on with the same stream. Defaults to zero, which ends at the broadcast's end.
	pub fn with_linger(mut self, linger: Duration) -> Self {
		self.linger = linger;
		self
	}

	/// Follow another publisher instance taking the path as a full program switch, instead of
	/// ending with [`Error::Replaced`](crate::Error::Replaced). Off by default.
	pub fn with_stitch(mut self, stitch: bool) -> Self {
		self.stitch = stitch;
		self
	}

	/// The next muxed frame (see [`Export::next`]), or `None` once the broadcast ends with
	/// nothing to follow.
	///
	/// An export failure while the broadcast is still announced is the export's own, so it is
	/// returned without lingering.
	pub async fn next(&mut self) -> crate::Result<Option<Frame>> {
		loop {
			let Some(export) = self.export.as_mut() else {
				return Ok(None);
			};
			let end = tokio::select! {
				next = export.next() => match next {
					Ok(Some(frame)) => return Ok(Some(frame)),
					Ok(None) => Ok(()),
					Err(err) => Err(err),
				},
				true = self.watch.changed(), if !self.watch.closed => {
					if self.stitch && matches!(self.watch.serving, Serving::Other(_)) {
						tracing::info!(path = %self.watch.path, "broadcast replaced, switching the program to the new instance");
						let export = self.export.take().expect("an export between follows");
						self.export = Some(self.watch.carry(export).await?);
					}
					continue;
				}
			};
			let export = self.export.take().expect("an export between follows");
			self.export = self.settle(export, end).await?;
		}
	}

	/// See [`Export::stats`].
	pub fn stats(&self) -> super::stats::Export {
		self.export.as_ref().map(Export::stats).unwrap_or_default()
	}

	/// See [`Export::discontinuity`].
	pub fn discontinuity(&self) -> u64 {
		self.export.as_ref().map_or(0, Export::discontinuity)
	}

	/// Decide what follows the export's `end`: the export carried on into a return, or `None`
	/// once a clean end has waited out the linger with nothing to follow.
	///
	/// The linger bounds the whole return, catalog subscription included.
	async fn settle(
		&mut self,
		export: Export<catalog::Ext>,
		end: crate::Result<()>,
	) -> crate::Result<Option<Export<catalog::Ext>>> {
		let watch = &mut self.watch;
		let deadline = Instant::now() + self.linger;
		// A failure ends the broadcast only if the broadcast goes too. One still announced
		// cannot return, so the failure is the export's own.
		if let Err(err) = &end
			&& !watch.ended
			&& watch.serving == Serving::Ours
		{
			let grace = Instant::now() + CLOSE_GRACE.min(self.linger);
			while watch.serving == Serving::Ours && timeout_at(grace, watch.changed()).await == Ok(true) {}
			if watch.serving == Serving::Ours {
				tracing::warn!(path = %watch.path, %err, "export failed with the broadcast still up, so not lingering");
				return Err(end.unwrap_err());
			}
		}
		if !self.linger.is_zero() {
			match &end {
				Ok(()) => {
					tracing::info!(path = %watch.path, linger = ?self.linger, "broadcast finished, waiting for it to return")
				}
				Err(err) => {
					tracing::warn!(path = %watch.path, %err, linger = ?self.linger, "broadcast ended, waiting for it to return")
				}
			}
		}

		loop {
			let follow = match &watch.serving {
				Serving::Ours => watch.ended,
				Serving::Other(_) if self.stitch => true,
				Serving::Other(_) => return Err(crate::Error::Replaced(watch.path.to_string())),
				Serving::Gone => false,
			};
			if follow {
				return match timeout_at(deadline, watch.carry(export)).await {
					Ok(next) => {
						tracing::info!(path = %watch.path, "broadcast returned, carrying on");
						next.map(Some)
					}
					Err(_) => {
						tracing::info!(path = %watch.path, linger = ?self.linger, "broadcast did not return");
						end.map(|()| None)
					}
				};
			}
			if !matches!(timeout_at(deadline, watch.changed()).await, Ok(true)) {
				tracing::info!(path = %watch.path, linger = ?self.linger, "broadcast did not return");
				return end.map(|()| None);
			}
		}
	}
}

/// What the announcements say serves the exported path.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Serving {
	/// Nothing: every route went.
	Gone,
	/// The publisher instance being exported.
	Ours,
	/// Another instance: another epoch, or any return of an epochless route.
	Other(Option<moq_net::Epoch>),
}

/// The path's announcements, against the instance the export reads.
struct Watch {
	origin: moq_net::origin::Consumer,
	path: moq_net::PathOwned,
	follow: moq_net::announce::Follow,
	/// The epoch of the instance being exported.
	epoch: Option<moq_net::Epoch>,
	serving: Serving,
	/// Nothing served the path at some point since the export resolved its broadcast.
	ended: bool,
	/// The origin closed, so nothing more is announced.
	closed: bool,
}

impl Watch {
	/// Apply the next change to the route serving the path, or `false` once the origin closes.
	async fn changed(&mut self) -> bool {
		let Some(event) = self.follow.next().await else {
			self.closed = true;
			return false;
		};
		match event {
			Event::Start(announce) => {
				let epoch = announce.route.epoch;
				self.serving = match epoch.is_some() && epoch == self.epoch {
					true => Serving::Ours,
					false => Serving::Other(epoch),
				};
			}
			Event::Restart(announce) => self.serving = Serving::Other(announce.route.epoch),
			// The same instance re-priced or failed over, which subscriptions ride out.
			Event::Update(_) => {}
			Event::End(_) => {
				self.serving = Serving::Gone;
				self.ended = true;
			}
		}
		true
	}

	/// Resolve the instance serving the path now, and export it from here on.
	async fn resolve(&mut self) -> crate::Result<moq_net::broadcast::Consumer> {
		let epoch = match &self.serving {
			Serving::Other(epoch) => epoch.clone(),
			Serving::Ours | Serving::Gone => self.epoch.clone(),
		};
		let broadcast = self.origin.request_broadcast(&self.path, epoch).await?;
		self.epoch = broadcast.info().epoch.clone();
		self.serving = Serving::Ours;
		self.ended = false;
		tracing::info!(path = %self.path, epoch = self.epoch.as_ref().map(tracing::field::display), "exporting broadcast");
		Ok(broadcast)
	}

	/// Carry `export` on into the instance serving the path now.
	async fn carry(&mut self, export: Export<catalog::Ext>) -> crate::Result<Export<catalog::Ext>> {
		let broadcast = self.resolve().await?;
		export.follow(broadcast).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn route(epoch: &moq_net::Epoch) -> moq_net::origin::Route {
		moq_net::origin::Route::default().with_epoch(epoch.clone())
	}

	fn origin() -> moq_net::origin::Producer {
		let (origin, driver) = moq_net::origin::Producer::new(Default::default());
		tokio::spawn(moq_net::time::run(driver));
		origin
	}

	/// A broadcast at `path` with a catalog, so an export of it resolves.
	fn publish(
		origin: &moq_net::origin::Producer,
		path: &str,
		route: moq_net::origin::Route,
	) -> (moq_net::broadcast::Producer, crate::catalog::Producer) {
		let mut broadcast = origin.publish(path, route).unwrap();
		let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		(broadcast, catalog)
	}

	async fn follower(origin: &moq_net::origin::Producer, path: &str) -> Follower {
		Follower::new(origin.consume(), path, CatalogFormat::Hang)
			.await
			.unwrap()
	}

	/// A failure while the broadcast stays announced is the export's own: it ends after the
	/// grace, without waiting out the linger.
	#[tokio::test(start_paused = true)]
	async fn a_failure_with_the_broadcast_up_ends_after_the_grace() {
		let origin = origin();
		let _live = publish(&origin, "live", route(&moq_net::Epoch::mint()));
		let mut follower = follower(&origin, "live").await.with_linger(Duration::from_secs(10));

		let start = Instant::now();
		let export = follower.export.take().unwrap();
		let failed = crate::Error::from(anyhow::anyhow!("boom"));
		let end = follower.settle(export, Err(failed)).await;
		assert!(end.is_err(), "the export's own failure ends it");
		assert_eq!(start.elapsed(), CLOSE_GRACE);
	}

	/// A replacement followed under stitch that never serves its catalog gives up at the
	/// linger, rather than waiting on the catalog past it.
	#[tokio::test(start_paused = true)]
	async fn a_return_without_a_catalog_expires_with_the_linger() {
		let origin = origin();
		let first = publish(&origin, "live", route(&moq_net::Epoch::mint()));
		let linger = Duration::from_secs(10);
		let mut follower = follower(&origin, "live").await.with_linger(linger).with_stitch(true);
		drop(first);

		// Replaced, but the replacement's catalog request is never answered.
		let second = origin.publish("live", route(&moq_net::Epoch::mint())).unwrap();
		let _unanswered = second.dynamic();

		let start = Instant::now();
		let export = follower.export.take().unwrap();
		let end = follower.settle(export, Ok(())).await.unwrap();
		assert!(end.is_none(), "a return that never resumes is no return");
		assert_eq!(start.elapsed(), linger);
	}

	/// Without stitch, another instance taking the path after the export ends fails it,
	/// however long the linger.
	#[tokio::test(start_paused = true)]
	async fn a_replacement_fails_without_stitch() {
		let origin = origin();
		let first = publish(&origin, "live", route(&moq_net::Epoch::mint()));
		let mut follower = follower(&origin, "live").await.with_linger(Duration::from_secs(10));
		drop(first);
		let _second = publish(&origin, "live", route(&moq_net::Epoch::mint()));

		let start = Instant::now();
		let export = follower.export.take().unwrap();
		let err = follower
			.settle(export, Ok(()))
			.await
			.err()
			.expect("a replacement fails");
		assert!(matches!(err, crate::Error::Replaced(_)), "{err}");
		assert!(start.elapsed() < Duration::from_secs(1), "no wait for a return");
	}

	/// An epochless route has no instance to match, so even its own return is a replacement.
	#[tokio::test(start_paused = true)]
	async fn an_epochless_return_is_a_replacement() {
		let origin = origin();
		let first = publish(&origin, "live", Default::default());
		let mut follower = follower(&origin, "live").await.with_linger(Duration::from_secs(10));
		drop(first);
		let _second = publish(&origin, "live", Default::default());

		let export = follower.export.take().unwrap();
		let err = follower
			.settle(export, Ok(()))
			.await
			.err()
			.expect("a replacement fails");
		assert!(matches!(err, crate::Error::Replaced(_)), "{err}");
	}

	/// The same instance coming back within the linger carries the export on.
	#[tokio::test(start_paused = true)]
	async fn the_same_instance_returning_continues() {
		let origin = origin();
		let epoch = moq_net::Epoch::mint();
		let first = publish(&origin, "live", route(&epoch));
		let mut follower = follower(&origin, "live").await.with_linger(Duration::from_secs(10));
		drop(first);

		let returned = async {
			tokio::time::sleep(Duration::from_secs(2)).await;
			let _second = publish(&origin, "live", route(&epoch));
			std::future::pending::<()>().await;
		};
		let start = Instant::now();
		let export = follower.export.take().unwrap();
		let next = tokio::select! {
			next = follower.settle(export, Ok(())) => next.unwrap(),
			_ = returned => unreachable!(),
		};
		assert!(next.is_some(), "the export carries on");
		assert_eq!(start.elapsed(), Duration::from_secs(2), "as soon as it is back");
	}

	/// The exact route going and a covering prefix of the same epoch taking over after the gap
	/// reaches the follower as an update rather than an end and a start
	/// (`quest/m0/broadcast-epoch/follow-gap.md`), so the export sees its broadcast end while
	/// its own instance still serves the path. It must never take that for a replacement: the
	/// export ends rather than splicing, and once the follower reports the gap it carries on.
	#[tokio::test(start_paused = true)]
	async fn a_same_epoch_handoff_after_a_gap_is_no_replacement() {
		let origin = origin();
		let epoch = moq_net::Epoch::mint();
		let exact = publish(&origin, "pool/job", route(&epoch));
		let linger = Duration::from_secs(10);
		let mut follower = follower(&origin, "pool/job").await.with_linger(linger);

		// Left unpolled across the handoff, and long enough for the exact route's retraction to
		// end the export's request before the prefix arrives.
		drop(exact);
		tokio::time::sleep(Duration::from_secs(1)).await;
		let _pool = origin.dynamic("pool", route(&epoch)).unwrap();

		let end = tokio::time::timeout(linger * 2, follower.next())
			.await
			.expect("the export settles within the linger");
		assert!(!matches!(end, Err(crate::Error::Replaced(_))), "{end:?}");
		assert!(!matches!(follower.watch.serving, Serving::Other(_)));
	}
}
