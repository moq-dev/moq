//! The `sink_%u` request pad as its own GObject, so the track it publishes can be named from a
//! pipeline description through `GstChildProxy` (`moqsink sink_0::track=camera`).

use std::sync::{LazyLock, Mutex, MutexGuard};

use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use hang::moq_net;

use super::MediaContainer;
use super::pad::Pad;
use super::session::{CAT, CompletionHandle, RUNTIME};

/// What the pad's track is doing, as reported by the `track-status` property.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, glib::Enum)]
#[repr(u32)]
#[enum_type(name = "GstMoqSinkPadStatus")]
pub enum Status {
	/// Requested, with no producer yet: CAPS has not arrived, or the element is not started.
	#[default]
	#[enum_value(name = "No producer yet", nick = "pending")]
	Pending = 0,
	/// CAPS built a producer and the broadcast reserved the track.
	#[enum_value(name = "Publishing", nick = "active")]
	Active = 1,
	/// The track was finalized cleanly.
	#[enum_value(name = "Track finalized", nick = "ended")]
	Ended = 2,
	/// The pad was invalidated; `track-error` carries the reason.
	#[enum_value(name = "Terminal pad failure", nick = "error")]
	Error = 3,
}

#[derive(Debug, Default)]
struct Settings {
	/// The name asked for through the property. Outlives the producer, so a restarted element reserves
	/// the same name again.
	requested: Option<String>,
	/// The name the broadcast actually reserved. Present only while that producer lives, and while it is
	/// present the property is fixed.
	effective: Option<String>,
	/// The wire container selected for this pad's media producer.
	container: MediaContainer,
	/// The most bits per second the encoder feeding this pad can produce. Zero takes no reservation.
	bandwidth_ceiling: u64,
	/// What the track is doing, read back through `status`.
	status: Status,
	/// The reason the pad was invalidated, read back through `track-error`.
	error: Option<String>,
}

/// Subscriber demand and an optional share of the connection's send estimate for one live track.
struct Bandwidth {
	demand: moq_net::track::Demand,
	allocator: moq_net::bandwidth::Allocator,
	/// Keeps the claim alive; the separate consumer can be cloned into the watcher.
	reservation: Option<moq_net::bandwidth::Reservation>,
	grant: Option<moq_net::bandwidth::Consumer>,
	watch_task: Option<tokio::task::JoinHandle<()>>,
}

/// Everything owned by one request pad, protected by one lock.
pub(super) struct PadLifecycle {
	settings: Settings,
	pub(super) media: Pad,
	bandwidth: Option<Bandwidth>,
	pub(super) ended: bool,
	pub(super) releasing: bool,
	/// This pad's link to its publication. `None` is no live session for the pad, which is why a buffer
	/// arriving before one exists is refused rather than dropped.
	pub(super) completion: Option<CompletionHandle>,
}

impl Default for PadLifecycle {
	fn default() -> Self {
		Self {
			settings: Settings::default(),
			media: Pad::new(),
			bandwidth: None,
			ended: false,
			releasing: false,
			completion: None,
		}
	}
}

impl Bandwidth {
	fn new(demand: moq_net::track::Demand, allocator: moq_net::bandwidth::Allocator, ceiling: u64) -> Self {
		let reservation =
			(ceiling > 0).then(|| allocator.reserve(&demand, moq_net::bandwidth::Rate::from_bps(ceiling)));
		let grant = reservation.as_ref().map(moq_net::bandwidth::Reservation::consume);
		Self {
			demand,
			allocator,
			reservation,
			grant,
			watch_task: None,
		}
	}

	/// Start notifications after this value is installed in the lifecycle. Property getters read the
	/// handles directly, so the tasks carry no mirrored state that could go stale.
	fn watch(&mut self, pad: glib::WeakRef<MoqSinkPad>) {
		self.stop_watch();
		let mut demand = self.demand.clone();
		let mut grant = self.grant.clone();
		// Activation emits the initial notification synchronously. Prime this clone so the task only
		// reports subsequent changes instead of immediately duplicating that notification.
		let _ = demand.poll_wanted_changed(&moq_net::kio::Waiter::noop());
		if let Some(grant) = &mut grant {
			let _ = grant.poll_changed(&moq_net::kio::Waiter::noop());
		}
		self.watch_task = Some(RUNTIME.spawn(async move {
			loop {
				tokio::select! {
					result = demand.wanted_changed() => {
						notify_demand(&pad);
						if result.is_err() {
							return;
						}
					}
					result = async {
						match &mut grant {
							Some(grant) => grant.changed().await,
							None => std::future::pending().await,
						}
					} => {
						notify_demand(&pad);
						if result.is_err() {
							return;
						}
					}
				}
			}
		}));
	}

	fn stop_watch(&mut self) {
		if let Some(task) = self.watch_task.take() {
			task.abort();
		}
	}

	/// Retune the live reservation. Returns whether the property's structure changed immediately.
	fn set_ceiling(&mut self, ceiling: u64, pad: glib::WeakRef<MoqSinkPad>) -> bool {
		match (&self.reservation, ceiling) {
			(Some(reservation), 1..) => {
				reservation.update(moq_net::bandwidth::Rate::from_bps(ceiling));
				false
			}
			(None, 0) => false,
			_ => {
				self.stop_watch();
				self.reservation = (ceiling > 0).then(|| {
					self.allocator
						.reserve(&self.demand, moq_net::bandwidth::Rate::from_bps(ceiling))
				});
				self.grant = self.reservation.as_ref().map(moq_net::bandwidth::Reservation::consume);
				self.watch(pad);
				true
			}
		}
	}

	fn snapshot(&self) -> gst::Structure {
		let wanted = self.demand.wanted();
		let mut state = gst::Structure::builder("moq-track-demand").field("wanted", wanted.is_some());
		if let Some(group) = wanted {
			state = state.field("group", group);
		}
		if let Some(grant) = &self.grant {
			state = state.field("probed", grant.probed());
			if let Some(rate) = grant.peek() {
				state = state.field("grant-bps", rate.as_bps());
			}
		}
		state.build()
	}
}

impl Drop for Bandwidth {
	fn drop(&mut self) {
		// Abort the watcher before releasing the claim. A task already running may still notify, but the
		// property getter reads the current lifecycle rather than stale task state.
		self.stop_watch();
	}
}

fn notify_demand(pad: &glib::WeakRef<MoqSinkPad>) {
	if let Some(pad) = pad.upgrade() {
		pad.notify("track-demand");
	}
}

/// Property notifications earned by one lifecycle operation.
#[derive(Default)]
pub(super) struct Notifications {
	track: bool,
	status: bool,
	error: bool,
	demand: bool,
}

/// The GObject implementation backing a named `moqsink` request pad.
#[derive(Default)]
pub struct MoqSinkPadImp {
	lifecycle: Mutex<PadLifecycle>,
}

impl Settings {
	/// Set the status, reporting whether it moved.
	fn set_status(&mut self, status: Status) -> bool {
		let changed = self.status != status;
		self.status = status;
		changed
	}
}

impl PadLifecycle {
	/// The configured track name while this pad is not releasing.
	pub(super) fn requested(&self) -> Option<&str> {
		self.settings.requested.as_deref()
	}

	/// The wire container configured for this pad's media producer.
	pub(super) fn container(&self) -> MediaContainer {
		self.settings.container
	}

	/// The encoder ceiling configured for this pad, in bits per second.
	pub(super) fn bandwidth_ceiling(&self) -> u64 {
		self.settings.bandwidth_ceiling
	}

	/// Record the name reserved by a successful CAPS event.
	pub(super) fn commit(&mut self, track: String) -> Notifications {
		let before = self
			.settings
			.effective
			.clone()
			.or_else(|| self.settings.requested.clone());
		self.settings.effective = Some(track);
		Notifications {
			track: before != self.settings.effective,
			status: self.settings.set_status(Status::Active),
			error: false,
			demand: false,
		}
	}

	/// Commit the track and start exposing its demand and optional bandwidth reservation.
	pub(super) fn activate(
		&mut self,
		track: String,
		allocator: moq_net::bandwidth::Allocator,
		pad: &MoqSinkPad,
	) -> Notifications {
		let mut changes = self.commit(track);
		// CAPS renegotiation replaces the media producer. Retire the old producer's demand and
		// reservation before binding the replacement, while resume paths use `restore_bandwidth`
		// directly because their retained producer still owns the same demand.
		self.bandwidth = None;
		changes.demand = self.restore_bandwidth(allocator, pad).is_some();
		changes
	}

	/// Stop reporting demand and release this pad's share while its retained media producer winds down.
	pub(super) fn retire_bandwidth(&mut self) -> Option<Notifications> {
		self.bandwidth.take()?;
		Some(Notifications {
			demand: true,
			..Default::default()
		})
	}

	/// Restore demand reporting when an ended pad resumes with its media producer still retained.
	pub(super) fn restore_bandwidth(
		&mut self,
		allocator: moq_net::bandwidth::Allocator,
		pad: &MoqSinkPad,
	) -> Option<Notifications> {
		if self.bandwidth.is_some() {
			return None;
		}
		let demand = self.media.demand()?;
		let ceiling = self.bandwidth_ceiling();
		let bandwidth = self.bandwidth.insert(Bandwidth::new(demand, allocator, ceiling));
		bandwidth.watch(pad.downgrade());
		Some(Notifications {
			demand: true,
			..Default::default()
		})
	}

	/// Record a terminal failure. The first reason wins: `error` is terminal for the run, so a later
	/// failure on an already-failed pad must not rewrite why it stopped. Cleared by [`Self::reset`].
	pub(super) fn fail(&mut self, reason: String) -> Notifications {
		let error = self.settings.error.is_none();
		self.settings.error.get_or_insert(reason);
		let demand = self.bandwidth.take().is_some();
		Notifications {
			status: self.settings.set_status(Status::Error),
			error,
			demand,
			..Default::default()
		}
	}

	/// Record clean finalization unless the failure is the more useful terminal state.
	pub(super) fn end(&mut self) -> Notifications {
		let status = self.settings.status != Status::Error && self.settings.set_status(Status::Ended);
		let demand = self.bandwidth.take().is_some();
		Notifications {
			status,
			demand,
			..Default::default()
		}
	}

	/// Reset this pad for another run while preserving its requested name.
	pub(super) fn reset(&mut self) -> Notifications {
		let before = self.settings.effective.take();
		let track = before.is_some() && before != self.settings.requested;
		let error = self.settings.error.take().is_some();
		let status = self.settings.set_status(Status::Pending);
		let demand = self.bandwidth.take().is_some();
		self.media = Pad::new();
		self.ended = false;
		self.completion = None;
		Notifications {
			track,
			status,
			error,
			demand,
		}
	}
}

#[glib::object_subclass]
impl ObjectSubclass for MoqSinkPadImp {
	const NAME: &'static str = "MoqSinkPad";
	type Type = MoqSinkPad;
	type ParentType = gst::Pad;
}

impl ObjectImpl for MoqSinkPadImp {
	fn properties() -> &'static [glib::ParamSpec] {
		static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
			vec![
				// MUTABLE_PLAYING because a pad requested while the element runs is configurable right
				// there; the window closes at this pad's CAPS event, which no state-based flag can express.
				glib::ParamSpecString::builder("track")
					.nick("Track")
					.blurb(
						"Name this pad publishes as, both in the broadcast and in the catalog. Writable \
						 in any state until the CAPS event reserves the track, and read-only from then \
						 on, when it reads back the reserved name. Going back to READY releases the \
						 reservation and makes it writable again. Empty keeps the generated name \
						 (0.avc3, 0.aac, ...)",
					)
					.mutable_playing()
					.build(),
				glib::ParamSpecEnum::builder::<MediaContainer>("container")
					.nick("Media container")
					.blurb(
						"Wire container used for this media track. Writable in any state until the \
						 CAPS event reserves the track; opaque application tracks are unchanged",
					)
					.default_value(MediaContainer::Legacy)
					.mutable_playing()
					.build(),
				glib::ParamSpecUInt64::builder("bandwidth-ceiling")
					.nick("Bandwidth ceiling")
					.blurb(
						"Maximum bitrate in bits per second for the encoder feeding this track. A \
						 nonzero value reserves a fair share of the connection estimate and exposes \
						 it through track-demand; zero observes demand without taking a reservation. \
						 Writable at any time; changes retune a live reservation or configure the next run",
					)
					.default_value(0)
					.mutable_playing()
					.build(),
				glib::ParamSpecEnum::builder::<Status>("track-status")
					.nick("Status")
					.blurb(
						"What this pad's track is doing: pending until CAPS builds a producer, active \
						 once the broadcast reserved the track, ended when it was finalized, error when \
						 the pad was invalidated. A connection drop is the element's `status`",
					)
					.read_only()
					.build(),
				glib::ParamSpecString::builder("track-error")
					.nick("Track error")
					.blurb(
						"Why this pad was invalidated, or null when it was not. Set with status=error \
						 and cleared when the pad is released or the element goes back to READY",
					)
					.read_only()
					.build(),
				glib::ParamSpecBoxed::builder::<gst::Structure>("track-demand")
					.nick("Track demand")
					.blurb(
						"Current subscriber demand and optional bandwidth grant. Null before the track \
						 is active; otherwise a moq-track-demand structure with wanted, optional group, \
						 and, when bandwidth-ceiling is nonzero, probed and optional grant-bps fields",
					)
					.read_only()
					.build(),
			]
		});
		PROPS.as_ref()
	}

	fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
		let mut lifecycle = self.lifecycle.lock().unwrap();
		if lifecycle.releasing {
			gst::warning!(
				CAT,
				obj = self.obj(),
				"{} ignored: the pad is being released",
				pspec.name()
			);
			return;
		}
		if pspec.name() == "bandwidth-ceiling" {
			let ceiling = value.get().unwrap();
			lifecycle.settings.bandwidth_ceiling = ceiling;
			let demand = lifecycle
				.bandwidth
				.as_mut()
				.is_some_and(|bandwidth| bandwidth.set_ceiling(ceiling, self.obj().downgrade()));
			drop(lifecycle);
			if demand {
				self.obj().notify("track-demand");
			}
			return;
		}
		// A producer keeps its setup for its whole life, so a later write would read back without ever
		// reaching the live track. The bandwidth ceiling is handled above because its reservation can
		// be retuned without replacing the producer.
		if lifecycle.settings.effective.is_some() {
			gst::warning!(
				CAT,
				obj = self.obj(),
				"{} ignored: the track is already reserved",
				pspec.name()
			);
			return;
		}
		match pspec.name() {
			// An empty name is not a track name: it selects the generated one.
			"track" => {
				lifecycle.settings.requested = value.get::<Option<String>>().unwrap().filter(|name| !name.is_empty())
			}
			"container" => lifecycle.settings.container = value.get().unwrap(),
			_ => unreachable!(),
		}
	}

	fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
		let lifecycle = self.lifecycle.lock().unwrap();
		match pspec.name() {
			"track" => lifecycle
				.settings
				.effective
				.clone()
				.or_else(|| lifecycle.settings.requested.clone())
				.to_value(),
			"container" => lifecycle.settings.container.to_value(),
			"bandwidth-ceiling" => lifecycle.settings.bandwidth_ceiling.to_value(),
			"track-status" => lifecycle.settings.status.to_value(),
			"track-error" => lifecycle.settings.error.clone().to_value(),
			"track-demand" => lifecycle.bandwidth.as_ref().map(Bandwidth::snapshot).to_value(),
			_ => unreachable!(),
		}
	}
}

impl GstObjectImpl for MoqSinkPadImp {}
impl PadImpl for MoqSinkPadImp {}

glib::wrapper! {
	/// A `moqsink` request pad: one track of the broadcast.
	pub struct MoqSinkPad(ObjectSubclass<MoqSinkPadImp>) @extends gst::Pad, gst::Object;
}

impl MoqSinkPad {
	/// Lock this pad's complete lifecycle state.
	pub(super) fn lifecycle(&self) -> MutexGuard<'_, PadLifecycle> {
		self.imp().lifecycle.lock().unwrap()
	}

	/// Emit property notifications after the lifecycle lock has been released.
	pub(super) fn notify_changes(&self, changes: Notifications) {
		if changes.track {
			self.notify("track");
		}
		if changes.status {
			self.notify("track-status");
		}
		if changes.error {
			self.notify("track-error");
		}
		if changes.demand {
			self.notify("track-demand");
		}
	}

	/// Reset a detached pad without asking GStreamer to remove it twice.
	pub(super) fn reset_detached(&self) {
		let changes = {
			let mut lifecycle = self.lifecycle();
			let changes = lifecycle.reset();
			lifecycle.releasing = false;
			changes
		};
		self.notify_changes(changes);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sink_pad() -> MoqSinkPad {
		gst::init().unwrap();
		gst::PadBuilder::<MoqSinkPad>::new(gst::PadDirection::Sink)
			.name("sink_0")
			.build()
	}

	fn apply(pad: &MoqSinkPad, operation: impl FnOnce(&mut PadLifecycle) -> Notifications) {
		let changes = operation(&mut pad.lifecycle());
		pad.notify_changes(changes);
	}

	#[test]
	fn the_status_follows_the_lifecycle() {
		let pad = sink_pad();
		assert_eq!(pad.property::<Status>("track-status"), Status::Pending);
		assert_eq!(pad.property::<Option<String>>("track-error"), None);

		apply(&pad, |lifecycle| lifecycle.commit("camera".to_string()));
		assert_eq!(pad.property::<Status>("track-status"), Status::Active);

		apply(&pad, PadLifecycle::end);
		assert_eq!(pad.property::<Status>("track-status"), Status::Ended);

		apply(&pad, |lifecycle| lifecycle.fail("no caps".to_string()));
		assert_eq!(pad.property::<Status>("track-status"), Status::Error);
		assert_eq!(
			pad.property::<Option<String>>("track-error").as_deref(),
			Some("no caps")
		);

		apply(&pad, PadLifecycle::reset);
		assert_eq!(
			pad.property::<Status>("track-status"),
			Status::Pending,
			"a released pad is pending again"
		);
		assert_eq!(
			pad.property::<Option<String>>("track-error"),
			None,
			"and carries no stale reason"
		);
	}

	// `error` is terminal for the run, so the reason the pad actually stopped has to survive a later
	// failure. A pad that fails on its bitstream and then sees an unsupported caps event still reports
	// the bitstream.
	#[test]
	fn the_first_failure_reason_wins() {
		let pad = sink_pad();
		apply(&pad, |lifecycle| lifecycle.fail("aac without codec_data".to_string()));
		apply(&pad, |lifecycle| {
			lifecycle.fail("unsupported caps: video/x-raw".to_string())
		});
		assert_eq!(
			pad.property::<Option<String>>("track-error").as_deref(),
			Some("aac without codec_data"),
			"the later failure did not rewrite why the pad stopped"
		);

		apply(&pad, PadLifecycle::reset);
		apply(&pad, |lifecycle| lifecycle.fail("a new run's failure".to_string()));
		assert_eq!(
			pad.property::<Option<String>>("track-error").as_deref(),
			Some("a new run's failure"),
			"but a reset clears it for the next run"
		);
	}

	// A failure is what ended the pad, and it is the more useful of the two, so EOS does not bury it.
	#[test]
	fn ending_a_failed_pad_keeps_the_error() {
		let pad = sink_pad();
		apply(&pad, |lifecycle| lifecycle.fail("bad bitstream".to_string()));
		apply(&pad, PadLifecycle::end);
		assert_eq!(pad.property::<Status>("track-status"), Status::Error);
		assert_eq!(
			pad.property::<Option<String>>("track-error").as_deref(),
			Some("bad bitstream")
		);
	}

	#[test]
	fn track_selection_is_serialized_with_the_lifecycle() {
		gst::init().unwrap();
		let pad = gst::PadBuilder::<MoqSinkPad>::new(gst::PadDirection::Sink)
			.name("sink_0")
			.build();
		assert_eq!(pad.property::<MediaContainer>("container"), MediaContainer::Legacy);
		assert_eq!(pad.property::<u64>("bandwidth-ceiling"), 0);
		assert!(pad.property::<Option<gst::Structure>>("track-demand").is_none());
		pad.set_property("track", "camera");
		pad.set_property("container", MediaContainer::Loc);
		pad.set_property("bandwidth-ceiling", 4_000_000u64);

		assert_eq!(pad.lifecycle().requested(), Some("camera"));
		assert_eq!(pad.lifecycle().container(), MediaContainer::Loc);
		assert_eq!(pad.lifecycle().bandwidth_ceiling(), 4_000_000);
		pad.set_property("track", "other");
		assert_eq!(pad.property::<String>("track"), "other");
		pad.set_property("track", "camera");

		let mut lifecycle = pad.lifecycle();
		assert_eq!(lifecycle.requested(), Some("camera"));
		assert_eq!(lifecycle.container(), MediaContainer::Loc);
		let other = pad.clone();
		assert!(
			std::thread::spawn(move || other.imp().lifecycle.try_lock().is_err())
				.join()
				.unwrap(),
			"a concurrent property write cannot enter during reservation"
		);
		let changes = lifecycle.commit("camera".to_string());
		drop(lifecycle);
		pad.notify_changes(changes);

		pad.set_property("track", "other");
		pad.set_property("container", MediaContainer::Legacy);
		pad.set_property("bandwidth-ceiling", 8_000_000u64);
		assert_eq!(pad.property::<MediaContainer>("container"), MediaContainer::Loc);
		assert_eq!(pad.property::<u64>("bandwidth-ceiling"), 8_000_000);
		apply(&pad, PadLifecycle::reset);
		pad.set_property("container", MediaContainer::Legacy);
		pad.set_property("bandwidth-ceiling", 12_000_000u64);
		assert_eq!(
			pad.property::<String>("track"),
			"camera",
			"the write rejected after reservation was not retained for the next run"
		);
		assert_eq!(pad.property::<MediaContainer>("container"), MediaContainer::Legacy);
		assert_eq!(pad.property::<u64>("bandwidth-ceiling"), 12_000_000);
	}

	#[test]
	fn a_live_bandwidth_ceiling_can_be_retuned_or_released() {
		let pad = sink_pad();
		let estimate = moq_net::bandwidth::Producer::new();
		estimate
			.set(Some(moq_net::bandwidth::Rate::from_bps(10_000_000)))
			.unwrap();
		let allocator = moq_net::bandwidth::Allocator::new(estimate.consume());
		let broadcast = moq_net::broadcast::Info::default().produce();
		let track = broadcast
			.create_track("camera", moq_net::track::Info::default())
			.unwrap();
		let bandwidth = Bandwidth::new(track.demand(), allocator, 4_000_000);
		let old_grant = bandwidth.grant.clone().unwrap();
		pad.lifecycle().bandwidth = Some(bandwidth);

		assert_eq!(old_grant.peek().map(|rate| rate.as_bps()), Some(4_000_000));
		pad.set_property("bandwidth-ceiling", 8_000_000u64);
		assert_eq!(old_grant.peek().map(|rate| rate.as_bps()), Some(8_000_000));

		pad.set_property("bandwidth-ceiling", 0u64);
		assert_eq!(old_grant.peek(), None, "the released reservation is unavailable");
		let state = pad.property::<gst::Structure>("track-demand");
		assert!(!state.has_field("probed"));
		assert!(!state.has_field("grant-bps"));

		pad.set_property("bandwidth-ceiling", 6_000_000u64);
		let state = pad.property::<gst::Structure>("track-demand");
		assert_eq!(state.get::<u64>("grant-bps"), Ok(6_000_000));
	}

	#[test]
	fn terminal_pads_release_their_bandwidth_claims() {
		let estimate = moq_net::bandwidth::Producer::new();
		estimate
			.set(Some(moq_net::bandwidth::Rate::from_bps(10_000_000)))
			.unwrap();
		let allocator = moq_net::bandwidth::Allocator::new(estimate.consume());
		let _allocator_guard = allocator.clone();
		let broadcast = moq_net::broadcast::Info::default().produce();

		let ended = sink_pad();
		let track = broadcast
			.create_track("ended", moq_net::track::Info::default())
			.unwrap();
		let bandwidth = Bandwidth::new(track.demand(), allocator.clone(), 4_000_000);
		let ended_grant = bandwidth.grant.clone().unwrap();
		ended.lifecycle().bandwidth = Some(bandwidth);
		apply(&ended, PadLifecycle::end);
		assert!(ended.property::<Option<gst::Structure>>("track-demand").is_none());
		assert_eq!(ended_grant.peek(), None);
		ended.set_property("bandwidth-ceiling", 2_000_000u64);
		apply(&ended, PadLifecycle::reset);
		assert_eq!(ended.property::<u64>("bandwidth-ceiling"), 2_000_000);

		let failed = sink_pad();
		let track = broadcast
			.create_track("failed", moq_net::track::Info::default())
			.unwrap();
		let bandwidth = Bandwidth::new(track.demand(), allocator, 4_000_000);
		let failed_grant = bandwidth.grant.clone().unwrap();
		failed.lifecycle().bandwidth = Some(bandwidth);
		apply(&failed, |lifecycle| lifecycle.fail("bad bitstream".to_string()));
		assert!(failed.property::<Option<gst::Structure>>("track-demand").is_none());
		assert_eq!(failed_grant.peek(), None);
	}

	#[test]
	fn track_demand_preserves_unavailable_and_zero_grants() {
		let pad = sink_pad();
		let estimate = moq_net::bandwidth::Producer::new();
		let allocator = moq_net::bandwidth::Allocator::new(estimate.consume());
		let broadcast = moq_net::broadcast::Info::default().produce();
		let track = broadcast
			.create_track("camera", moq_net::track::Info::default())
			.unwrap();
		pad.lifecycle().bandwidth = Some(Bandwidth::new(track.demand(), allocator, 4_000_000));

		let state = pad.property::<gst::Structure>("track-demand");
		assert_eq!(state.get::<bool>("wanted"), Ok(false));
		assert!(!state.has_field("group"));
		assert!(!state.has_field("grant-bps"), "an unavailable estimate is absent");
		assert_eq!(state.get::<bool>("probed"), Ok(false));

		estimate.set(Some(moq_net::bandwidth::Rate::ZERO)).unwrap();
		let state = pad.property::<gst::Structure>("track-demand");
		assert_eq!(state.get::<u64>("grant-bps"), Ok(0), "a real zero remains present");

		estimate
			.set(Some(moq_net::bandwidth::Rate::from_bps(2_000_000)))
			.unwrap();
		let subscriber = track.subscribe(None);
		let state = pad.property::<gst::Structure>("track-demand");
		assert_eq!(state.get::<bool>("wanted"), Ok(true));
		assert_eq!(state.get::<u64>("group"), Ok(0));
		assert_eq!(state.get::<u64>("grant-bps"), Ok(2_000_000));
		drop(subscriber);
	}
}
