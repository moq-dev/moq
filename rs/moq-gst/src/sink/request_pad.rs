//! The `sink_%u` request pad as its own GObject, so the track it publishes can be named from a
//! pipeline description through `GstChildProxy` (`moqsink sink_0::track=camera`).

use std::sync::{LazyLock, Mutex, MutexGuard};

use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;

use super::session::CAT;

#[derive(Debug, Default)]
struct Settings {
	/// The name asked for through the property. Outlives the producer, so a restarted element reserves
	/// the same name again.
	requested: Option<String>,
	/// The name the broadcast actually reserved. Present only while that producer lives, and while it is
	/// present the property is fixed.
	effective: Option<String>,
	/// Blocks CAPS from creating a producer while the element removes this pad.
	releasing: bool,
}

/// The GObject implementation backing a named `moqsink` request pad.
#[derive(Debug, Default)]
pub struct MoqSinkPadImp {
	settings: Mutex<Settings>,
}

/// A pending track reservation that serializes property writes until it is committed or dropped.
pub(super) struct TrackReservation<'a> {
	pad: &'a MoqSinkPad,
	settings: MutexGuard<'a, Settings>,
}

impl TrackReservation<'_> {
	/// The name configured when this reservation began.
	pub(super) fn requested(&self) -> Option<&str> {
		self.settings.requested.as_deref()
	}

	/// Fix the property to the name the broadcast reserved, then notify after releasing the lock.
	pub(super) fn commit(self, track: String) {
		let TrackReservation { pad, mut settings } = self;
		let before = settings.effective.clone().or_else(|| settings.requested.clone());
		settings.effective = Some(track);
		let changed = before != settings.effective;
		drop(settings);
		if changed {
			pad.notify("track");
		}
	}

	/// Drop the effective name, then notify after releasing the lock.
	pub(super) fn release(self) {
		let TrackReservation { pad, mut settings } = self;
		let before = settings.effective.take();
		let changed = before.is_some() && before != settings.requested;
		drop(settings);
		if changed {
			pad.notify("track");
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
			]
		});
		PROPS.as_ref()
	}

	fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
		let mut settings = self.settings.lock().unwrap();
		if settings.releasing {
			gst::warning!(
				CAT,
				obj = self.obj(),
				"{} ignored: the pad is being released",
				pspec.name()
			);
			return;
		}
		// A producer keeps the name it reserved for its whole life, so a later write would read back
		// without ever reaching the broadcast or the catalog.
		if settings.effective.is_some() {
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
			"track" => settings.requested = value.get::<Option<String>>().unwrap().filter(|name| !name.is_empty()),
			_ => unreachable!(),
		}
	}

	fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
		let settings = self.settings.lock().unwrap();
		match pspec.name() {
			"track" => settings
				.effective
				.clone()
				.or_else(|| settings.requested.clone())
				.to_value(),
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
	/// Lock the configured name until the caller commits or abandons its track reservation.
	pub(super) fn reserve_track(&self) -> Option<TrackReservation<'_>> {
		let settings = self.imp().settings.lock().unwrap();
		if settings.releasing {
			return None;
		}
		Some(TrackReservation { pad: self, settings })
	}

	/// Mark the pad as releasing and lock its track settings during producer removal.
	pub(super) fn retire_track(&self) -> TrackReservation<'_> {
		let mut settings = self.imp().settings.lock().unwrap();
		settings.releasing = true;
		TrackReservation { pad: self, settings }
	}

	/// Make a detached pad configurable after CAPS can no longer reach the element.
	pub(super) fn finish_release(&self) {
		self.imp().settings.lock().unwrap().releasing = false;
	}

	/// Drop the reserved name once its producer is finalized, so the pad is configurable again on the
	/// next run. The requested name stays: that run reserves the same one.
	pub(super) fn release_track(&self) {
		if let Some(reservation) = self.reserve_track() {
			reservation.release();
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn track_selection_is_locked_until_the_reservation_is_committed() {
		gst::init().unwrap();
		let pad = gst::PadBuilder::<MoqSinkPad>::new(gst::PadDirection::Sink)
			.name("sink_0")
			.build();
		pad.set_property("track", "camera");

		let abandoned = pad.reserve_track().unwrap();
		assert_eq!(abandoned.requested(), Some("camera"));
		drop(abandoned);
		pad.set_property("track", "other");
		assert_eq!(pad.property::<String>("track"), "other");
		pad.set_property("track", "camera");

		let reservation = pad.reserve_track().unwrap();
		assert_eq!(reservation.requested(), Some("camera"));
		let other = pad.clone();
		assert!(
			std::thread::spawn(move || other.imp().settings.try_lock().is_err())
				.join()
				.unwrap(),
			"a concurrent property write cannot enter during reservation"
		);
		reservation.commit("camera".to_string());

		pad.set_property("track", "other");
		pad.release_track();
		assert_eq!(
			pad.property::<String>("track"),
			"camera",
			"the write rejected after reservation was not retained for the next run"
		);
	}
}
