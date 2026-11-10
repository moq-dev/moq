//! Shared test support for moq-net integration tests.

pub mod harness;
pub mod mock;

use futures::channel::mpsc;
use moq_net::{datagram, goaway, origin, track};

/// Wait until anyone wants the track, returning the group they want.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub async fn wanted(mut demand: track::Demand) -> moq_net::Result<u64> {
	loop {
		if let Some(wanted) = demand.wanted_changed().await? {
			return Ok(wanted);
		}
	}
}

/// Wait until nobody wants the track.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub async fn unwanted(mut demand: track::Demand) -> moq_net::Result<()> {
	while demand.wanted_changed().await?.is_some() {}
	Ok(())
}

/// Wait until a datagram track's highest subscriber priority is `want`, where `None` means
/// nobody reads it.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub async fn datagram_priority(producer: &datagram::Producer, want: Option<i8>) {
	let mut watch = producer.clone();
	while watch.priority_changed().await != want {}
}

/// Wait until `consumer` sees a route announced at exactly `path`, and return it.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub async fn announced(consumer: &origin::Consumer, path: &str) -> origin::RouteInfo {
	let mut announced = consumer.routes();
	loop {
		match announced.next().await.expect("the origin closed") {
			origin::RouteEvent {
				kind: origin::RouteKind::Start | origin::RouteKind::Update | origin::RouteKind::Restart,
				announce,
			} if announce.prefix.as_str() == path => {
				return announce.route;
			}
			_ => {}
		}
	}
}

/// The kind of an [`origin::RouteEvent`] for a prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub enum Kind {
	Start,
	Update,
	Restart,
	End,
	Offline,
}

/// Every update per prefix the watcher has seen. Await [`kio::clock::sim::idle`]
/// first, so every link, hold and quarantine in the mesh has settled.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub fn drain(watched: &mut mpsc::UnboundedReceiver<(String, Kind)>) -> std::collections::HashMap<String, Vec<Kind>> {
	let mut updates = std::collections::HashMap::<String, Vec<Kind>>::new();
	while let Ok((prefix, kind)) = watched.try_recv() {
		updates.entry(prefix).or_default().push(kind);
	}
	updates
}

/// Watch `announced` from its own task, the way a session's announce writer does:
/// it runs when woken, between the relays' own tasks, rather than only once the
/// test task is polled again, which would coalesce every intermediate update.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub fn watch(mut announced: origin::Routes) -> mpsc::UnboundedReceiver<(String, Kind)> {
	let (tx, rx) = mpsc::unbounded();
	kio::clock::sim::spawn(async move {
		while let Some(event) = announced.next().await {
			let (kind, announce) = match event {
				origin::RouteEvent {
					kind: origin::RouteKind::Start,
					announce,
				} => (Kind::Start, announce),
				origin::RouteEvent {
					kind: origin::RouteKind::Update,
					announce,
				} => (Kind::Update, announce),
				origin::RouteEvent {
					kind: origin::RouteKind::Restart,
					announce,
				} => (Kind::Restart, announce),
				origin::RouteEvent {
					kind: origin::RouteKind::End,
					announce,
				} => (Kind::End, announce),
				origin::RouteEvent {
					kind: origin::RouteKind::Offline,
					announce,
				} => (Kind::Offline, announce),
			};
			if tx.unbounded_send((announce.prefix.to_string(), kind)).is_err() {
				break;
			}
		}
	});
	rx
}

/// Reads of the peer's GOAWAY over [`goaway::Consumer::poll`].
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub trait Draining {
	/// The peer's GOAWAY, or `None` if it has not sent one.
	fn peek(&self) -> Option<goaway::Goaway>;

	/// Wait for the peer's GOAWAY, or `None` if the session closes without one.
	async fn recv(&self) -> Option<goaway::Goaway>;
}

impl Draining for goaway::Consumer {
	fn peek(&self) -> Option<goaway::Goaway> {
		match self.poll(&kio::Waiter::noop()) {
			std::task::Poll::Ready(Ok(goaway)) => Some(goaway),
			_ => None,
		}
	}

	async fn recv(&self) -> Option<goaway::Goaway> {
		kio::wait(|waiter| self.poll(waiter)).await.ok()
	}
}

/// Every version this crate speaks, including the opt-in `-wip` drafts that
/// [`moq_net::Versions::all`] leaves out.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub fn every_version() -> Vec<moq_net::Version> {
	moq_net::Version::ALL.to_vec()
}
