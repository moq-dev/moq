#![cfg(feature = "async")]

//! Failover along a chain of relays, replaying what `rs/moq-net/verify/failover/FailoverChain.tla`
//! checks: a group the subscriber started continues on another route at the same frame
//! while any path still holds it.
//!
//! The publisher `P` is pulled by relays `A` and `B`. The subscribing relay `E` reads through
//! `A` until a cheaper route through `B` appears; `B` resumes it from the newest group `E`
//! handed out. Then `A`'s session to `E` dies with an older group half read, so `E` fetches
//! the rest of that group from `B`, whose subscription never asked for it.

use crate::route_change::{link, payload, priced_link, produce_origin, settle, timed};
use crate::support::harness::MockPair;

use std::time::Duration;

use crate::support::AppendNext;
use crate::support::AtStart;
use futures::StreamExt;
use futures::channel::mpsc;
use moq_net::{Error, Version, group, origin, track};

/// Within the subscriber's one-minute budget, so a group that never continues is a hang
/// rather than a group given up as too old.
const PATIENCE: Duration = Duration::from_secs(30);

type Frames = mpsc::UnboundedReceiver<moq_net::Result<Option<Vec<u8>>>>;

struct Chain {
	/// Group 0, open at the publisher with its first frame written.
	group0: Option<group::Producer>,
	/// Group 0 as `E`'s subscriber reads it, past its first frame.
	read0: Option<group::Consumer>,
	track: track::Producer,
	_sub: track::Subscriber,
	_read1: group::Consumer,
	_relays: Vec<origin::Producer>,
	_broadcast: origin::Route,
	_links: Vec<MockPair>,
	/// `A`'s sessions to `P` and, with the mesh, to `B`.
	a_links: Vec<MockPair>,
}

impl Chain {
	/// `E` holds group 0 half read through `A`, and group 1 handed out, while `B` serves it
	/// from group 1 on and `A`'s session to `E` is gone.
	async fn new() -> Self {
		Self::build(false).await
	}

	/// [`Self::new`], with `B` routing through `A`, which is cheaper than its own link to `P`
	/// (`EndedRelay.cfg`).
	async fn mesh() -> Self {
		Self::build(true).await
	}

	async fn build(mesh: bool) -> Self {
		let version: Version = "moq-lite-07-wip".parse().unwrap();
		let publisher = produce_origin(1);
		let relay_a = produce_origin(2);
		let relay_b = produce_origin(3);
		let relay_e = produce_origin(4);

		let broadcast = publisher.route("live", origin::RouteOptions::hidden()).unwrap();
		let track = broadcast.broadcast().create_track("video", timed()).unwrap();
		broadcast.announce().unwrap();

		let mut a_links = vec![link(version, &publisher, &relay_a).await];
		let mut links = vec![priced_link(version, &publisher, &relay_b, mesh.then_some(1_000)).await];
		if mesh {
			a_links.push(link(version, &relay_a, &relay_b).await);
		}
		let a_to_e = priced_link(version, &relay_a, &relay_e, Some(1_000)).await;

		let remote = relay_e
			.consume()
			.follow("live", None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.unwrap();
		let prefs =
			track::Subscription::default().with_delay(track::Delay::default().with_max(Duration::from_secs(60)));
		let mut sub = remote.track("video").unwrap().subscribe(prefs).await.unwrap();

		let mut group0 = track.append_next().unwrap();
		group0.write_frame(group0.at_start(), payload(0, 0)).unwrap();
		let mut read0 = sub.recv_group().await.unwrap().unwrap();
		assert_eq!(read0.start, 0);
		assert_eq!(
			read0.read_frame().await.unwrap().unwrap().payload.to_vec(),
			payload(0, 0)
		);

		let mut group1 = track.append_next().unwrap();
		group1.write_frame(group1.at_start(), payload(1, 0)).unwrap();
		group1.close().unwrap();
		let mut read1 = sub.recv_group().await.unwrap().unwrap();
		assert_eq!(read1.start, 1);
		assert_eq!(
			read1.read_frame().await.unwrap().unwrap().payload.to_vec(),
			payload(1, 0)
		);

		// The cheaper route through `B` wins; `E` asks it from group 1.
		links.push(link(version, &relay_b, &relay_e).await);
		settle().await;

		a_to_e.server.abort(Error::Cancel);
		a_to_e.client.abort(Error::Cancel);
		settle().await;

		Self {
			group0: Some(group0),
			read0: Some(read0),
			track,
			_sub: sub,
			_read1: read1,
			_relays: vec![relay_a, relay_b, relay_e],
			_broadcast: broadcast,
			_links: links,
			a_links,
		}
	}

	/// `A` dies: its sessions to `P` and `B` end.
	async fn kill_a(&mut self) {
		for link in self.a_links.drain(..) {
			link.server.abort(Error::Cancel);
			link.client.abort(Error::Cancel);
		}
		settle().await;
	}

	/// Read group 0's next frame in the background, which fetches the rest from `B`.
	fn read_on(&mut self) -> Frames {
		let (tx, rx) = mpsc::unbounded();
		let mut read0 = self.read0.take().expect("read once");
		kio::clock::sim::spawn(async move {
			let frame = read0.read_frame().await;
			let _ = tx.unbounded_send(frame.map(|frame| frame.map(|frame| frame.payload.to_vec())));
		});
		rx
	}

	/// The publisher ends the track at group 2 while group 0 is still open.
	async fn end(&mut self) {
		self.track.finish_at(track::Position::group(2), None).unwrap();
		settle().await;
	}

	/// The publisher writes the rest of group 0, which every relay can still reach.
	fn finish_group0(&mut self) {
		let mut group0 = self.group0.take().expect("finished once");
		group0.write_frame(group0.at_start(), payload(0, 1)).unwrap();
		group0.close().unwrap();
	}
}

/// Without the end, the rest of group 0 comes through `B`, as it must with the end too.
#[kio::test]
async fn a_half_read_group_continues_through_the_new_route() {
	let mut chain = Chain::new().await;
	let mut rx = chain.read_on();
	settle().await;
	chain.finish_group0();
	let frame = kio::clock::timeout(PATIENCE, rx.next()).await.expect("group 0 hung");
	assert_eq!(frame.unwrap().unwrap(), Some(payload(0, 1)));
}

/// `E`'s fetch of the rest of group 0 is in flight through `B` when the publisher ends the
/// track. `B`'s copy, asked from group 1, completes, but its fetch from `P` lives on, so the
/// rest of group 0 arrives (`NoSkipWhileHeld` in `verify/failover/FailoverChain.tla`).
#[kio::test]
async fn ending_the_track_keeps_a_fetch_in_flight() {
	let mut chain = Chain::new().await;
	let mut rx = chain.read_on();
	settle().await;
	chain.end().await;
	chain.finish_group0();
	let frame = kio::clock::timeout(PATIENCE, rx.next()).await.expect("group 0 hung");
	assert_eq!(frame.unwrap().unwrap(), Some(payload(0, 1)));
}

/// The publisher ends the track before `E` reads on, so `E`'s copy from `B`, asked from
/// group 1, completes and `E`'s track ends. Reading group 0 on still fetches the rest from
/// `B`'s completed copy (`Settles` in `verify/failover/FailoverChain.tla`).
#[kio::test]
async fn a_fetch_from_a_completed_copy_goes_on_the_wire() {
	let mut chain = Chain::new().await;
	chain.end().await;
	let mut rx = chain.read_on();
	chain.finish_group0();
	let frame = kio::clock::timeout(PATIENCE, rx.next()).await.expect("group 0 hung");
	assert_eq!(frame.unwrap().unwrap(), Some(payload(0, 1)));
}

/// Reading group 0 on only past the linger, once the finished copies may be gone, still
/// fetches the rest.
#[kio::test]
async fn a_fetch_after_the_finished_copies_linger_goes_on_the_wire() {
	let mut chain = Chain::new().await;
	chain.end().await;
	kio::clock::sleep(Duration::from_secs(40)).await;
	let mut rx = chain.read_on();
	chain.finish_group0();
	let frame = kio::clock::timeout(PATIENCE, rx.next()).await.expect("group 0 hung");
	assert_eq!(frame.unwrap().unwrap(), Some(payload(0, 1)));
}

/// `E`'s fetch of the rest of group 0 goes through `B`, which serves it through `A`. The
/// publisher ends the track, then `A` dies: `B`'s fetch fails, so `B`'s front, whose track
/// ended, moves it to `P`, which `B` still reaches, and the rest arrives (`NoSkipWhileHeld`
/// in `verify/failover/FailoverChain.tla` with `Mesh`).
#[kio::test]
async fn a_fetch_moves_off_a_dying_route_after_the_end() {
	let mut chain = Chain::mesh().await;
	let mut rx = chain.read_on();
	settle().await;
	chain.end().await;
	chain.kill_a().await;
	chain.finish_group0();
	let frame = kio::clock::timeout(PATIENCE, rx.next()).await.expect("group 0 hung");
	assert_eq!(frame.unwrap().unwrap(), Some(payload(0, 1)));
}
