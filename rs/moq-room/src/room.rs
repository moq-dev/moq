//! A room is a path prefix. Participants are discovered from the announce stream.

use std::task::{Poll, ready};

use moq_net::{
	PathOwned, announce, broadcast,
	origin::{self, Requesting},
};

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

/// An in-flight request for the broadcast an active announcement named.
struct Inflight {
	identity: PathOwned,
	kind: Kind,
	path: PathOwned,
	request: Requesting,
}

/// Runs the announce loop and yields remote participant broadcasts.
///
/// Skips paths that are not `{identity}/camera.hang` or `{identity}/screen.hang`, and
/// skips the local identity so a publisher does not see itself as a remote.
pub struct Room {
	origin: origin::Consumer,
	announced: announce::Consumer,
	local: Option<PathOwned>,
	inflight: Option<Inflight>,
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
			origin: origin.clone(),
			local,
			inflight: None,
		}
	}

	/// Poll for the next remote camera/screen (un)announce.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Option<Event>> {
		loop {
			if let Some(event) = ready!(self.poll_inflight(waiter)) {
				return Poll::Ready(Some(event));
			}

			let Some(update) = ready!(self.announced.poll_next(waiter)) else {
				return Poll::Ready(None);
			};
			let path = update.prefix;
			let Some(parsed) = parse(&path) else {
				continue;
			};
			if self.local.as_ref().is_some_and(|id| *id == parsed.identity) {
				continue;
			}
			if !update.kind.is_active() {
				return Poll::Ready(Some(Event {
					identity: parsed.identity,
					kind: parsed.kind,
					path,
					broadcast: None,
				}));
			}

			let mut request = self.origin.request_broadcast(&path).into_inner();
			match kio::Task::poll(&mut request, waiter) {
				Poll::Ready(Ok(broadcast)) => {
					return Poll::Ready(Some(Event {
						identity: parsed.identity,
						kind: parsed.kind,
						path,
						broadcast: Some(broadcast),
					}));
				}
				Poll::Ready(Err(_)) => continue,
				Poll::Pending => {
					self.inflight = Some(Inflight {
						identity: parsed.identity,
						kind: parsed.kind,
						path,
						request,
					});
					return Poll::Pending;
				}
			}
		}
	}

	fn poll_inflight(&mut self, waiter: &kio::Waiter) -> Poll<Option<Event>> {
		let Some(inflight) = self.inflight.as_mut() else {
			return Poll::Ready(None);
		};
		match ready!(kio::Task::poll(&mut inflight.request, waiter)) {
			Ok(broadcast) => {
				let inflight = self.inflight.take().expect("inflight still set");
				Poll::Ready(Some(Event {
					identity: inflight.identity,
					kind: inflight.kind,
					path: inflight.path,
					broadcast: Some(broadcast),
				}))
			}
			Err(_) => {
				self.inflight = None;
				Poll::Ready(None)
			}
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
	use moq_net::{Path, origin::Route};

	fn origin() -> origin::Producer {
		moq_tokio::origin::spawn()
	}

	fn publish(origin: &origin::Producer, path: &str) -> broadcast::Producer {
		let broadcast = origin.create_broadcast(path).expect(path);
		broadcast.announce(Route::default()).expect("announce");
		broadcast
	}

	#[tokio::test]
	async fn yields_camera_and_skips_local_and_unknown() {
		let origin = origin();
		let local = Path::new("alice").to_owned();
		let mut room = Room::new(&origin.consume(), Some(local));

		let _alice = publish(&origin, "alice/camera");
		let _bob = publish(&origin, "bob/camera");
		let _noise = publish(&origin, "bob/chat");

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
		let origin = origin();
		let mut room = Room::new(&origin.consume(), None);

		let _screen = publish(&origin, "bob/screen");

		let event = room.next().await.expect("bob screen");
		assert_eq!(event.identity.as_str(), "bob");
		assert_eq!(event.kind, Kind::Screen);
		assert!(event.broadcast.is_some());
	}
}
