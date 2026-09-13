//! A room is a path prefix. Participants are discovered from the announce stream.

use std::task::{Poll, ready};

use moq_net::{PathOwned, announce, broadcast, origin};

use crate::path::{Kind, parse};

/// One (un)announce of a participant broadcast.
#[derive(Clone)]
pub struct Event {
	/// Participant identity (everything before `camera`/`screen`).
	pub identity: PathOwned,
	/// `camera` or `screen`.
	pub kind: Kind,
	/// Broadcast path relative to the room prefix.
	pub path: PathOwned,
	/// The live broadcast, or `None` when it went offline.
	pub broadcast: Option<broadcast::Consumer>,
}

impl std::fmt::Debug for Event {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Event")
			.field("identity", &self.identity)
			.field("kind", &self.kind)
			.field("path", &self.path)
			.field("online", &self.broadcast.is_some())
			.finish()
	}
}

/// Runs the announce loop and yields remote participant broadcasts.
///
/// Skips paths that are not `{identity}/camera.hang` or `{identity}/screen.hang`, and
/// skips the local identity so a publisher does not see itself as a remote.
pub struct Room {
	announced: announce::Consumer,
	local: Option<PathOwned>,
}

impl std::fmt::Debug for Room {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Room")
			.field("local", &self.local)
			.finish_non_exhaustive()
	}
}

impl Room {
	/// Watch announcements on `origin`, skipping `local` when set.
	pub fn new(origin: &origin::Consumer, local: Option<PathOwned>) -> Self {
		Self {
			announced: origin.announced(),
			local,
		}
	}

	/// Poll for the next remote camera/screen (un)announce.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Option<Event>> {
		loop {
			let Some(update) = ready!(self.announced.poll_next(waiter)) else {
				return Poll::Ready(None);
			};
			let Some(parsed) = parse(&update.path) else {
				continue;
			};
			if self.local.as_ref().is_some_and(|id| *id == parsed.identity) {
				continue;
			}
			return Poll::Ready(Some(Event {
				identity: parsed.identity,
				kind: parsed.kind,
				path: update.path,
				broadcast: update.broadcast,
			}));
		}
	}

	/// Wait for the next remote camera/screen (un)announce.
	pub async fn next(&mut self) -> Option<Event> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use moq_net::{Origin, Path, broadcast::Route};

	#[tokio::test]
	async fn yields_camera_and_skips_local_and_unknown() {
		let origin = Origin::random().produce();
		let local = Path::new("alice").to_owned();
		let mut room = Room::new(&origin.consume(), Some(local));

		let _alice = origin
			.create_broadcast("alice/camera", Route::announced())
			.expect("alice camera");
		let _bob = origin
			.create_broadcast("bob/camera", Route::announced())
			.expect("bob camera");
		let _noise = origin
			.create_broadcast("bob/chat", Route::announced())
			.expect("unknown kind");

		let event = room.next().await.expect("bob camera");
		assert_eq!(event.identity.as_str(), "bob");
		assert_eq!(event.kind, Kind::Camera);
		assert!(event.broadcast.is_some());

		drop(_bob);
		let gone = room.next().await.expect("bob unannounce");
		assert_eq!(gone.identity.as_str(), "bob");
		assert_eq!(gone.kind, Kind::Camera);
		assert!(gone.broadcast.is_none());
	}

	#[tokio::test]
	async fn yields_screen() {
		let origin = Origin::random().produce();
		let mut room = Room::new(&origin.consume(), None);

		let _screen = origin
			.create_broadcast("bob/screen", Route::announced())
			.expect("bob screen");

		let event = room.next().await.expect("bob screen");
		assert_eq!(event.identity.as_str(), "bob");
		assert_eq!(event.kind, Kind::Screen);
		assert!(event.broadcast.is_some());
	}
}
