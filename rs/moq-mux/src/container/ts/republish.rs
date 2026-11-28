//! One program of a multiplex, republished on its path whenever the source changes.

use bytes::{Bytes, BytesMut};

use super::error::{Error, Result};
use super::{Ext, Import, Step, stats};
use crate::catalog;

/// Imports one program of a multiplex as a broadcast on `origin`, republishing the path under a
/// new identity each time the source changes.
///
/// The single-program sibling of [`Programs`](super::Programs). Each identity is created hidden
/// and announced once its catalog first goes out: a relay queries a broadcast's tracks the moment
/// it is announced, so one announced any sooner would be asked for tracks it cannot serve yet.
///
/// A change of source is a [`Step`]: the source's clock moving onto another timeline, or its PAT
/// naming another program, as [`Import::decode`] judges it. [`decode`](Self::decode) then
/// finishes the identity before the step, withdrawing it, and opens the next under a new
/// identity, returning the step as [`Republished`]. Nothing crosses it but the measured mux rate:
/// the new catalog records it from its first media rather than holding for a fresh measurement
/// window, since the multiplex is the same. The new identity waits for the source's own PAT, PMT
/// and next keyframe.
pub struct Republish {
	target: Target,
	identity: Identity,
	/// The input after the last step, which the current identity has not read yet.
	held: Bytes,
}

/// The source changed, so [`Republish`] finished the identity before the change and opened the
/// next one, hidden until its catalog goes out.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Republished {
	/// How the source changed.
	pub step: Step,
	/// What the finished identity's elementary streams delivered; see [`Import::stats`].
	pub stats: stats::Snapshot,
}

/// Where and how each identity publishes.
struct Target {
	origin: moq_net::origin::Producer,
	name: moq_net::Path,
	config: catalog::Config,
	program: Option<u16>,
}

impl Target {
	/// Open an identity with `options`, its catalog recording `mux_rate` when the multiplex was
	/// measured already.
	fn open(&self, options: moq_net::origin::RouteOptions, mux_rate: Option<u64>) -> Result<Identity> {
		let route = self.origin.route(self.name.as_str(), options)?;
		// TS carries undecoded elementary streams verbatim, so the catalog carries the `mpegts`
		// extension.
		let mut initial = catalog::hang::Catalog::<Ext>::default();
		initial.ext.mpegts.mux_rate = mux_rate;
		let config = self.config.clone().with_catalog(initial);
		let catalog = catalog::Producer::new(&mut route.broadcast().clone(), config)?;
		let mut import = Import::new(route.broadcast().clone(), catalog.reserve());
		if let Some(program) = self.program {
			import = import.with_program(program);
		}
		let pending = Some(route.broadcast().consume().track(hang::Catalog::DEFAULT_NAME)?);
		Ok(Identity {
			route,
			import,
			catalog,
			pending,
		})
	}
}

/// One identity of the broadcast.
struct Identity {
	/// Created hidden: dropping it retracts the broadcast the importer fills.
	route: moq_net::origin::Route,
	import: Import<Ext>,
	catalog: catalog::Producer<Ext>,
	/// The catalog track until the catalog first goes out, when the route announces.
	pending: Option<moq_net::track::Consumer>,
}

impl Identity {
	/// Announce the broadcast once its catalog has gone out, returning whether it has.
	fn announce_if_listed(&mut self) -> Result<bool> {
		listed(&mut self.pending, &self.route)
	}

	/// Finish the media tracks, then the catalog while it still lists them, aborting both with the
	/// cause of a failure.
	///
	/// The `last` identity announces if its drain released the first catalog, and fails when the
	/// input declared no track. One a step ended is withdrawn as it drops, so it does neither: a
	/// source can step before its first catalog goes out, such as within the mux rate's window.
	fn finish(mut self, last: bool) -> Result<Finished> {
		let import = match self.import.finish() {
			Ok(import) => import,
			Err(err) => {
				self.catalog.abort(moq_net::Error::Transport(format!("{err:#}")));
				return Err(err);
			}
		};
		// The drain at end of input can release the first catalog. Without one the input declared
		// no track, and announcing would only advertise an empty broadcast.
		let declared = match last {
			true => listed(&mut self.pending, &self.route).and_then(|declared| match declared {
				true => Ok(()),
				false => Err(Error::Empty("it declared a track")),
			}),
			false => Ok(()),
		};
		let declared = declared.and_then(|()| Ok(self.catalog.finish()?));
		if let Err(err) = declared {
			let reason = moq_net::Error::Transport(format!("{err:#}"));
			import.abort(reason.clone());
			self.catalog.abort(reason);
			return Err(err);
		}
		Ok(Finished {
			import,
			_catalog: self.catalog,
			route: self.route,
		})
	}
}

impl Republish {
	/// Import the multiplex onto `name`, its catalogs built from `config`, publishing the first
	/// identity as `identity` and each after it under a fresh one. The first is created now, so
	/// its [`catalog`](Self::catalog) can be filled before any input.
	///
	/// Pass [`Identity::random`](moq_net::Identity::random) unless the identity is shared, such as
	/// across a run's broadcasts; see [`RouteOptions::with_identity`](moq_net::origin::RouteOptions::with_identity).
	pub fn new(
		origin: moq_net::origin::Producer,
		name: impl Into<moq_net::Path>,
		identity: moq_net::Identity,
		config: catalog::Config,
	) -> Result<Self> {
		let target = Target {
			origin,
			name: name.into(),
			config,
			program: None,
		};
		let options = moq_net::origin::RouteOptions::hidden().with_identity(identity, 0);
		let identity = target.open(options, None)?;
		Ok(Self {
			target,
			identity,
			held: Bytes::new(),
		})
	}

	/// Import only the program with this PAT program number; see [`Import::with_program`].
	pub fn with_program(mut self, program: u16) -> Self {
		self.target.program = Some(program);
		self.identity.import = self.identity.import.with_program(program);
		self
	}

	/// The current identity's broadcast, such as for a track of the caller's own.
	pub fn broadcast(&self) -> &moq_net::broadcast::Producer {
		self.identity.route.broadcast()
	}

	/// The current identity's catalog. Sections set before the importer declares a track ride
	/// the first catalog; set them again on each new identity after [`Republished`].
	pub fn catalog(&self) -> &catalog::Producer<Ext> {
		&self.identity.catalog
	}

	/// The current identity's publisher identity.
	pub fn identity(&self) -> moq_net::Identity {
		self.identity.route.identity()
	}

	/// Demux a chunk of the multiplex into the current identity, announcing it once its catalog
	/// goes out. A trailing partial packet is retained for the next call.
	///
	/// A change of source finishes the identity there and opens the next, returning
	/// [`Republished`] before the next reads anything: the rest of `data` is held for it, read at
	/// the start of the next call. Call again, with no data if there is none, until it returns
	/// `None`. A failure to finish the identity before the step returns that failure, with the
	/// next identity current.
	pub fn decode(&mut self, data: &[u8]) -> Result<Option<Republished>> {
		if !self.held.is_empty() {
			let held = std::mem::take(&mut self.held);
			if let Some(republished) = self.feed(&held)? {
				if !data.is_empty() {
					let mut rest = BytesMut::from(std::mem::take(&mut self.held));
					rest.extend_from_slice(data);
					self.held = rest.freeze();
				}
				return Ok(Some(republished));
			}
		}
		self.feed(data)
	}

	/// Demux `data` into the current identity, or republish at the step it reaches.
	fn feed(&mut self, data: &[u8]) -> Result<Option<Republished>> {
		let Some(restart) = self.identity.import.decode_or_restart(data)? else {
			self.identity.announce_if_listed()?;
			return Ok(None);
		};
		self.republish(restart).map(Some)
	}

	/// No input follows: read what the current identity held while judging a step, and
	/// republish at a step it reaches.
	fn settle(&mut self) -> Result<Option<Republished>> {
		match self.identity.import.settle()? {
			Some(restart) => self.republish(restart).map(Some),
			None => Ok(None),
		}
	}

	/// Finish the current identity before `restart`'s step and open the next, holding the input
	/// from the step on for it.
	fn republish(&mut self, restart: super::Restart) -> Result<Republished> {
		let mux_rate = self.identity.catalog.snapshot().ext.mpegts.mux_rate;
		let next = self.target.open(moq_net::origin::RouteOptions::hidden(), mux_rate)?;
		self.held = restart.remainder;
		// The finished identity ends as it drops, withdrawing its announcement.
		let finished = std::mem::replace(&mut self.identity, next).finish(false)?;
		Ok(Republished {
			step: restart.step,
			stats: finished.stats(),
		})
	}

	/// What every elementary stream of the current identity has delivered; see [`Import::stats`].
	pub fn stats(&self) -> stats::Snapshot {
		self.identity.import.stats()
	}

	/// Finish the current identity's media tracks, then its catalog while it still lists them,
	/// and return the [`Finished`] broadcast.
	///
	/// Input still held after a [`Republished`], or while a step was judged, is read first,
	/// republishing again at any step in it. Fails when the input ended before the identity
	/// declared a track, so nothing was announced. A finish that fails aborts every track with
	/// the cause.
	pub fn finish(mut self) -> Result<Finished> {
		loop {
			let next = match self.decode(&[]) {
				Ok(None) => self.settle(),
				next => next,
			};
			match next {
				Ok(Some(republished)) => {
					tracing::warn!(step = %republished.step, "the source changed at end of input; republishing")
				}
				Ok(None) => return self.identity.finish(true),
				Err(err) => {
					self.abort(moq_net::Error::Transport(format!("{err:#}")));
					return Err(err);
				}
			}
		}
	}

	/// Abort the current identity's catalog and media tracks with `err`. Consumes the importer.
	pub fn abort(self, err: moq_net::Error) {
		let Identity { import, catalog, .. } = self.identity;
		catalog.abort(err.clone());
		import.abort(err);
	}
}

/// Announce `route` once the catalog track `pending` has a group, returning whether it has.
pub(super) fn listed(pending: &mut Option<moq_net::track::Consumer>, route: &moq_net::origin::Route) -> Result<bool> {
	let Some(listed) = pending else {
		return Ok(true);
	};
	if !matches!(
		listed.poll_largest(&kio::Waiter::noop()),
		std::task::Poll::Ready(Some(_))
	) {
		return Ok(false);
	}
	route.announce()?;
	*pending = None;
	Ok(true)
}

/// A [`Republish`] [finished](Republish::finish): every track of its last identity has ended,
/// and they stay servable while it is held.
#[must_use = "dropping it releases the tracks and withdraws the broadcast"]
pub struct Finished {
	import: super::Finished<Ext>,
	_catalog: catalog::Producer<Ext>,
	route: moq_net::origin::Route,
}

impl Finished {
	/// The finished broadcast, such as for a [`Linger`](crate::Linger).
	pub fn broadcast(&self) -> &moq_net::broadcast::Producer {
		self.route.broadcast()
	}

	/// What every elementary stream delivered; see [`Import::stats`].
	pub fn stats(&self) -> stats::Snapshot {
		self.import.stats()
	}

	/// Abort every track with `err`. Readers still draining a track get its clean end instead once
	/// every group below its end finished.
	pub fn abort(self, err: moq_net::Error) {
		self.import.abort(err);
	}
}

#[cfg(test)]
mod tests {
	use super::super::step_test::{SWITCH, flagged_switch, shift};
	use super::*;

	/// 5 s of H.264 and AAC at a constant 400 kb/s, so the first identity measures a rate.
	const CLIP: &[u8] = include_bytes!("test_data/bbb5s_cbr.ts");

	/// Whether `stats` counts a delivered unit.
	fn delivered(stats: &stats::Snapshot) -> bool {
		stats.streams.values().any(|stream| stream.units > 0)
	}

	/// A switch to an encoder whose clock started hours earlier comes back from `decode` as a
	/// value. The identity before it finishes, and the next opens under a new identity, its
	/// catalog carrying the measured mux rate, hidden until that catalog goes out. However the
	/// input is split, the next identity reads it from the step on.
	#[test]
	fn a_switch_republishes_under_a_new_identity_with_the_rate() {
		let open = || {
			let origin = crate::source::produce_origin();
			Republish::new(
				origin,
				moq_net::Path::from_static("demo"),
				moq_net::Identity::random(),
				catalog::Config::default(),
			)
			.unwrap()
		};
		let mut measured = open();
		assert!(measured.decode(CLIP).unwrap().is_none());
		let rate = measured.catalog().snapshot().ext.mpegts.mux_rate;
		assert!(rate.is_some(), "the clip measures a rate");

		let mut data = CLIP.to_vec();
		data.extend(shift(CLIP, |_| -SWITCH));
		for chunk in [data.len(), 188 * 7, 1_000] {
			let mut republish = open();
			let first = republish.identity();
			let mut republished = Vec::new();
			for piece in data.chunks(chunk) {
				let mut next = republish.decode(piece).unwrap();
				while let Some(step) = next {
					assert!(
						!republish.identity.route.is_announced(),
						"{chunk}: announced before its catalog"
					);
					assert_eq!(
						republish.catalog().snapshot().ext.mpegts.mux_rate,
						rate,
						"{chunk}: the next catalog carries the measured rate"
					);
					republished.push(step);
					next = republish.decode(&[]).unwrap();
				}
			}
			let [step] = &republished[..] else {
				panic!("{chunk}: one switch: {republished:?}");
			};
			assert!(
				matches!(step.step, Step::Rewind { flagged: false, .. }),
				"{chunk}: {step:?}"
			);
			assert!(delivered(&step.stats), "{chunk}: the first identity's counts");
			assert_ne!(republish.identity(), first, "{chunk}: a new identity");
			assert!(
				republish.identity.route.is_announced(),
				"{chunk}: the next identity announced with its catalog"
			);
			let finished = republish.finish().unwrap();
			assert!(
				delivered(&finished.stats()),
				"{chunk}: the next identity read the input after the step"
			);
		}
	}

	/// A source that steps in the same chunk as its first media, before its first catalog went
	/// out, republishes rather than failing: only the last identity has to declare a track.
	#[test]
	fn a_switch_before_the_first_catalog_republishes() {
		// 1.5 s of input, then the step, all in one chunk.
		let mut data = CLIP[..400 * 188].to_vec();
		data.extend(shift(CLIP, |_| -SWITCH));
		let origin = crate::source::produce_origin();
		let mut republish = Republish::new(
			origin,
			moq_net::Path::from_static("demo"),
			moq_net::Identity::random(),
			catalog::Config::default(),
		)
		.unwrap();
		let first = republish.identity.route.clone();
		let step = republish.decode(&data).unwrap().expect("a switch");
		assert!(matches!(step.step, Step::Rewind { .. }), "{step:?}");
		assert!(!first.is_announced(), "the first identity never went out");
		assert!(republish.decode(&[]).unwrap().is_none());
		let finished = republish.finish().unwrap();
		assert!(delivered(&finished.stats()));
	}

	/// A switch the input ends while judging is found by `finish`, which republishes the
	/// broadcast after it rather than drop it.
	#[test]
	fn a_switch_found_at_the_end_republishes() {
		let origin = crate::source::produce_origin();
		let mut republish = Republish::new(
			origin,
			moq_net::Path::from_static("demo"),
			moq_net::Identity::random(),
			catalog::Config::default(),
		)
		.unwrap();
		let first = republish.identity();
		let decoded = republish.decode(&flagged_switch()).unwrap();
		assert!(decoded.is_none(), "the switch is still judged: {decoded:?}");
		let finished = republish.finish().unwrap();
		assert_ne!(finished.route.identity(), first, "the finish republished");
		assert!(
			delivered(&finished.stats()),
			"the next identity read the input after the step"
		);
	}

	/// Held input that fails to decode at the finish aborts the identity reading it, so its
	/// readers get the cause rather than a clean end.
	#[tokio::test(start_paused = true)]
	async fn a_failure_in_held_input_aborts_the_identity() {
		let mut data = CLIP.to_vec();
		data.extend(shift(CLIP, |_| -SWITCH));
		// A second program appearing mid-stream ends an import that picked none.
		data.extend(
			super::super::fixture::Mux::default()
				.pat(&[(1, 0x100), (2, 0x200)])
				.bytes(),
		);
		let origin = crate::source::produce_origin();
		let mut republish = Republish::new(
			origin,
			moq_net::Path::from_static("demo"),
			moq_net::Identity::random(),
			catalog::Config::default(),
		)
		.unwrap();
		republish.decode(&data).unwrap().expect("a switch");
		// The next identity's media tracks only exist within the finish, so read its catalog, which
		// is aborted with them. A held copy keeps a mere drop from ending it.
		let _copy = republish.catalog().clone();
		let mut catalog = republish
			.broadcast()
			.consume()
			.track(hang::Catalog::DEFAULT_NAME)
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();
		let err = republish.finish().err().expect("two programs");
		assert!(matches!(err, Error::MultiplePrograms { .. }), "{err}");
		let ended = kio::clock::timeout(std::time::Duration::from_secs(1), async {
			loop {
				if let Err(err) = catalog.recv_group().await.transpose()? {
					return Some(err);
				}
			}
		})
		.await
		.expect("the catalog ends");
		assert!(ended.is_some(), "the catalog ends with the cause, not cleanly");
	}

	/// Input that ends before declaring a track fails, and nothing was announced.
	#[test]
	fn an_input_with_no_track_fails_unannounced() {
		let origin = crate::source::produce_origin();
		let republish = Republish::new(
			origin,
			moq_net::Path::from_static("demo"),
			moq_net::Identity::random(),
			catalog::Config::default(),
		)
		.unwrap();
		let route = republish.identity.route.clone();
		let err = republish.finish().err().expect("nothing was declared");
		assert!(matches!(err, Error::Empty("it declared a track")), "{err}");
		assert!(!route.is_announced());
	}
}
