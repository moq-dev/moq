//! A room is a path prefix. Participants are discovered from the announce stream.

use std::task::Poll;

use moq_net::{Path, announce, broadcast, origin};

use crate::path::{Kind, parse};

/// One (un)announce of a participant broadcast.
#[derive(Clone)]
pub struct Event {
	/// Participant identity (everything before `camera`/`screen`).
	pub identity: Path,
	/// `camera` or `screen`.
	pub kind: Kind,
	/// Broadcast path relative to the room prefix.
	pub path: Path,
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

/// A listed participant broadcast, followed across the instances that serve it.
struct Participant {
	identity: Path,
	kind: Kind,
	path: Path,
	following: origin::Following,
}

/// Runs the announce loop and yields remote participant broadcasts.
///
/// Skips paths that are not `{identity}/camera.hang` or `{identity}/screen.hang`, and
/// skips the local identity so a publisher does not see itself as a remote. A participant
/// that restarts is yielded again with its new broadcast.
pub struct Room {
	origin: origin::Consumer,
	announced: announce::Consumer,
	local: Option<Path>,
	participants: Vec<Participant>,
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
	pub fn new(origin: &origin::Consumer, local: Option<Path>) -> Self {
		Self {
			announced: origin.announced(),
			origin: origin.clone(),
			local,
			participants: Vec::new(),
		}
	}

	/// Poll for the next remote camera/screen (un)announce.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Option<Event>> {
		loop {
			match self.announced.poll_next(waiter) {
				Poll::Ready(Some(announce::Event::Present(announce))) => self.listed(announce.prefix),
				Poll::Ready(Some(announce::Event::Absent(announce))) => {
					let Some(index) = self.participants.iter().position(|p| p.path == announce.prefix) else {
						continue;
					};
					let gone = self.participants.swap_remove(index);
					return Poll::Ready(Some(Event {
						identity: gone.identity,
						kind: gone.kind,
						path: gone.path,
						broadcast: None,
					}));
				}
				Poll::Ready(None) => return Poll::Ready(None),
				Poll::Pending => break,
			}
		}
		self.poll_participants(waiter).map(Some)
	}

	/// Follow a listed path, unless it is not a remote participant broadcast.
	fn listed(&mut self, path: Path) {
		let Some(parsed) = parse(&path) else {
			return;
		};
		if self.local.as_ref().is_some_and(|id| *id == parsed.identity) {
			return;
		}
		let Ok(following) = self.origin.follow(&path, None) else {
			return;
		};
		self.participants.push(Participant {
			identity: parsed.identity,
			kind: parsed.kind,
			path,
			following,
		});
	}

	/// The next broadcast a participant's follower hands out. A refusal, such as a handler
	/// refusing the path, yields nothing: the follower waits for another instance to serve it,
	/// and the participant's end comes from the listing.
	fn poll_participants(&mut self, waiter: &kio::Waiter) -> Poll<Event> {
		let mut index = 0;
		while let Some(participant) = self.participants.get_mut(index) {
			match participant.following.poll_available(waiter) {
				Poll::Ready(Ok(Some(broadcast))) => {
					return Poll::Ready(Event {
						identity: participant.identity.clone(),
						kind: participant.kind,
						path: participant.path.clone(),
						broadcast: Some(broadcast),
					});
				}
				Poll::Ready(Ok(None)) => continue,
				// Re-polled on the next wake, rather than again now: a closed origin answers
				// `Closed` every time until the listing ends.
				Poll::Ready(Err(_)) => index += 1,
				Poll::Pending => index += 1,
			}
		}
		Poll::Pending
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

	fn publish(origin: &origin::Producer, path: &str) -> origin::Publication {
		let broadcast = origin.create_broadcast(path).expect(path);
		broadcast.announce(Route::default()).expect("announce");
		broadcast
	}

	#[tokio::test]
	async fn yields_camera_and_skips_local_and_unknown() {
		let origin = origin();
		let local = Path::from_static("alice");
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

	/// A refused participant stays listed and followed: a publisher taking it over is yielded, and
	/// its absence is still reported.
	#[tokio::test(start_paused = true)]
	async fn reports_the_absence_of_a_refused_participant() {
		let wait = std::time::Duration::from_secs(1);
		let origin = origin();
		let mut room = Room::new(&origin.consume(), None);

		let claim = origin.requests("bob/camera").expect("claim");
		let (request, nothing) = tokio::join!(claim.requested_broadcast(), kio::clock::timeout(wait, room.next()));
		assert!(nothing.is_err(), "nothing serves bob yet");
		request.expect("requested").reject(moq_net::Error::NotFound);
		// The refusal yields nothing.
		assert!(kio::clock::timeout(wait, room.next()).await.is_err());

		// A publisher taking the path over from the claim is still followed.
		let bob = publish(&origin, "bob/camera");
		let back = kio::clock::timeout(wait, room.next())
			.await
			.expect("the publisher was never yielded")
			.expect("bob camera");
		assert!(back.broadcast.is_some());

		drop((bob, claim));
		let gone = kio::clock::timeout(wait, room.next())
			.await
			.expect("the absence was never reported")
			.expect("bob unannounce");
		assert_eq!(gone.identity.as_str(), "bob");
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
