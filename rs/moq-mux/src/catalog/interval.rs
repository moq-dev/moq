//! The broadcast's join interval: `media.join.interval`, declared once from every media track's
//! longest group.
//!
//! Each video and audio track a catalog mints joins as a [`Member`], carrying its hint (the GOP an
//! encoder or ingest knows it writes) or, without one, the longest group its
//! [`Estimator`](super::Estimator) measured over its first window of a second or a group, whichever
//! is longer. Once every member present has one, the catalog declares the largest, unless the
//! publisher already set `media.join` itself. The interval is the grid an HLS or DASH export numbers
//! segments on, so it is never changed once declared: a track that later reports a longer group
//! overruns it, which the export tolerates, and a warning says so.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// Declares the interval on the catalog: called at most once per broadcast, outside every lock.
pub(crate) type Declare = Box<dyn Fn(Duration) + Send + Sync>;

/// Every member's interval, shared by a catalog's clones.
#[derive(Clone, Default)]
pub(crate) struct Intervals(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
	/// Each present member's interval, `None` while it is still measuring.
	members: BTreeMap<u64, Option<Duration>>,
	next: u64,
	/// The interval declared, once declared.
	declared: Option<Duration>,
	/// A member's longer group was already warned about, so the warning does not repeat.
	warned: bool,
}

impl State {
	/// The interval to declare now, if this is the moment: nothing is declared yet, and every
	/// present member has its interval.
	fn ready(&mut self) -> Option<Duration> {
		if self.declared.is_some() || self.members.is_empty() {
			return None;
		}
		let interval = self.members.values().try_fold(Duration::ZERO, |max, interval| Some(max.max((*interval)?)))?;
		self.declared = Some(interval);
		Some(interval)
	}

	/// Warn once that a member's group overran the declared interval.
	fn check(&mut self, interval: Duration) {
		if let Some(declared) = self.declared
			&& interval > declared
			&& !self.warned
		{
			self.warned = true;
			tracing::warn!(
				?interval,
				?declared,
				"a track's groups are longer than the declared join interval; HLS lists them as overruns"
			);
		}
	}
}

impl Intervals {
	/// Join with `hint`, the track's known longest group, or `None` to measure it.
	pub fn join(&self, hint: Option<Duration>, declare: Declare) -> Member {
		let hint = hint.filter(|hint| !hint.is_zero());
		let (id, ready) = {
			let mut state = self.lock();
			let id = state.next;
			state.next += 1;
			state.members.insert(id, hint);
			if let Some(hint) = hint {
				state.check(hint);
			}
			(id, state.ready())
		};
		if let Some(interval) = ready {
			declare(interval);
		}
		Member {
			intervals: self.clone(),
			id,
			interval: hint,
			hinted: hint.is_some(),
			declare,
		}
	}

	/// The interval declared, once every member had one.
	#[cfg(test)]
	pub fn declared(&self) -> Option<Duration> {
		self.lock().declared
	}

	fn lock(&self) -> std::sync::MutexGuard<'_, State> {
		self.0.lock().unwrap_or_else(PoisonError::into_inner)
	}
}

/// One media track's place among the [`Intervals`], removed on drop.
pub(crate) struct Member {
	intervals: Intervals,
	id: u64,
	/// The hint, or the longest group measured so far.
	interval: Option<Duration>,
	/// The interval is a hint, which no measurement replaces.
	hinted: bool,
	declare: Declare,
}

impl Member {
	/// The track's interval: its hint, or what was measured so far.
	pub fn interval(&self) -> Option<Duration> {
		self.interval
	}

	/// Report the longest group measured so far. A hint is never replaced.
	pub fn report(&mut self, measured: Option<Duration>) {
		let Some(measured) = measured.filter(|measured| !measured.is_zero()) else {
			return;
		};
		if self.interval.is_some_and(|interval| interval >= measured) {
			return;
		}
		let ready = {
			let mut state = self.intervals.lock();
			state.check(measured);
			if self.hinted {
				return;
			}
			if let Some(slot) = state.members.get_mut(&self.id) {
				*slot = Some(slot.map_or(measured, |interval| interval.max(measured)));
			}
			state.ready()
		};
		self.interval = Some(measured);
		if let Some(interval) = ready {
			(self.declare)(interval);
		}
	}
}

impl Drop for Member {
	fn drop(&mut self) {
		// A track that ends before measuring no longer holds the declaration back.
		let ready = {
			let mut state = self.intervals.lock();
			state.members.remove(&self.id);
			state.ready()
		};
		if let Some(interval) = ready {
			(self.declare)(interval);
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	fn secs(secs: f64) -> Duration {
		Duration::from_secs_f64(secs)
	}

	fn recorder() -> (Arc<Mutex<Vec<Duration>>>, impl Fn() -> Declare) {
		let declared = Arc::new(Mutex::new(Vec::new()));
		let make = {
			let declared = declared.clone();
			move || -> Declare {
				let declared = declared.clone();
				Box::new(move |interval| declared.lock().unwrap().push(interval))
			}
		};
		(declared, make)
	}

	#[test]
	fn hints_declare_their_largest_at_once() {
		let (declared, declare) = recorder();
		let intervals = Intervals::default();
		let _video = intervals.join(Some(secs(2.0)), declare());
		assert_eq!(*declared.lock().unwrap(), [secs(2.0)]);
		let _audio = intervals.join(Some(secs(4.0)), declare());
		assert_eq!(*declared.lock().unwrap(), [secs(2.0)], "declared once, never raised");
		assert_eq!(intervals.declared(), Some(secs(2.0)));
	}

	#[test]
	fn an_unmeasured_member_holds_the_declaration_back() {
		let (declared, declare) = recorder();
		let intervals = Intervals::default();
		let _video = intervals.join(Some(secs(2.0)), declare());
		let mut audio = intervals.join(None, declare());
		// The video joined alone first, so it declared before the audio was present.
		assert_eq!(*declared.lock().unwrap(), [secs(2.0)]);
		audio.report(Some(secs(0.02)));
		assert_eq!(*declared.lock().unwrap(), [secs(2.0)]);

		let (declared, declare) = recorder();
		let intervals = Intervals::default();
		let mut audio = intervals.join(None, declare());
		let mut video = intervals.join(None, declare());
		audio.report(Some(secs(0.02)));
		assert!(declared.lock().unwrap().is_empty(), "the video is still measuring");
		video.report(Some(secs(2.0)));
		assert_eq!(*declared.lock().unwrap(), [secs(2.0)]);
		video.report(Some(secs(3.0)));
		assert_eq!(*declared.lock().unwrap(), [secs(2.0)], "fixed once declared");
	}

	#[test]
	fn a_member_dropped_while_measuring_releases_the_declaration() {
		let (declared, declare) = recorder();
		let intervals = Intervals::default();
		let mut audio = intervals.join(None, declare());
		let video = intervals.join(None, declare());
		audio.report(Some(secs(1.0)));
		drop(video);
		assert_eq!(*declared.lock().unwrap(), [secs(1.0)]);
	}

	#[test]
	fn a_hint_is_never_replaced_by_a_measurement() {
		let (_, declare) = recorder();
		let intervals = Intervals::default();
		let mut video = intervals.join(Some(secs(2.0)), declare());
		video.report(Some(secs(1.0)));
		video.report(Some(secs(3.0)));
		assert_eq!(video.interval(), Some(secs(2.0)));
	}
}
