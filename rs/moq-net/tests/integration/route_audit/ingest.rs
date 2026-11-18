//! A publisher on two relays: redundant ingest under one Instance, an unsynchronized pair
//! under two, and a GOAWAY handing a publisher to another relay.
//!
//! Each runs on a ring of six relays with a reader on every relay, the publisher's two
//! relays opposite each other at `R0` and `R3`.

use super::{Feed, Source, bloat, measured, readers, ring};
use crate::path_routes::{cut, land, lite07, quiesce};

use std::time::Duration;

use crate::support::Draining;
use moq_net::{Instance, goaway::Goaway};

/// How an ingest session goes.
#[derive(Clone, Copy, Debug)]
enum Loss {
	/// It drops at once.
	Cut,
	/// Its queue deepens to two seconds over two seconds, then it drops.
	Bloat,
}

impl Loss {
	async fn apply(self, pair: &crate::support::harness::MockPair) {
		match self {
			Self::Cut => cut(pair),
			Self::Bloat => bloat(pair, Duration::from_secs(2)).await,
		}
	}
}

/// How redundant ingest serves one Instance.
#[derive(Clone, Copy, Debug)]
enum Redundant {
	/// One encoder holding a session to each relay.
	OneEncoder,
	/// Two synchronized encoders, one per relay, writing the same bytes under one Instance.
	Synchronized,
}

/// One Instance ingested on `R0` and `R3`; the ingest on `lost` goes `how`. Every reader
/// reads on in the broadcast it had, every group once, and no link carries a group twice.
async fn redundant(redundant: Redundant, lost: usize, how: Loss) {
	let mesh = ring(6).await;
	let instance = Instance::random();
	let (sources, feed) = match redundant {
		Redundant::OneEncoder => {
			let source = Source::new(1, None);
			let feed = Feed::start(vec![source.track.clone()], "P");
			(vec![source], feed)
		}
		Redundant::Synchronized => {
			let sources = vec![Source::new(1, Some(&instance)), Source::new(2, Some(&instance))];
			let feed = Feed::start(sources.iter().map(|source| source.track.clone()).collect(), "P");
			(sources, feed)
		}
	};
	let mut pushes = Vec::new();
	for (i, relay) in [0, 3].into_iter().enumerate() {
		let source = &sources[i.min(sources.len() - 1)];
		pushes.push(source.push(&mesh.relays[relay], lite07(), 1).await);
	}
	quiesce().await;
	let mut readers = readers(&mesh).await;

	let since = kio::clock::now();
	how.apply(&pushes[usize::from(lost == 3)]).await;
	quiesce().await;
	for (i, reader) in readers.iter_mut().enumerate() {
		let report = reader.report(since);
		measured(&format!("redundant {redundant:?}, R{lost} lost by {how:?}, reader R{i}"), &report);
		assert!(report.seamless(), "{redundant:?} R{lost} {how:?}: R{i}: {report:?}");
	}
	assert_eq!(mesh.doubled(feed.written()), [], "{redundant:?} R{lost} {how:?}");
}

#[kio::test]
async fn one_encoder_on_two_relays_reads_on_losing_either() {
	for lost in [0, 3] {
		for how in [Loss::Cut, Loss::Bloat] {
			redundant(Redundant::OneEncoder, lost, how).await;
		}
	}
}

#[kio::test]
async fn synchronized_encoders_read_on_losing_either() {
	for lost in [0, 3] {
		for how in [Loss::Cut, Loss::Bloat] {
			redundant(Redundant::Synchronized, lost, how).await;
		}
	}
}

/// Two unsynchronized encoders, `A` on `R0` and `B` on `R3`, each under its own minted
/// Instance. `A`'s ingest goes: the readers on `A` restart onto `B` with nothing of `B`
/// spliced into `A`'s broadcast, and the readers already on `B` read on.
#[kio::test]
async fn an_unsynchronized_pair_restarts_and_splices_nothing() {
	for how in [Loss::Cut, Loss::Bloat] {
		let mesh = ring(6).await;
		let [a, b] = [Source::new(1, None), Source::new(2, None)];
		let _feeds = [
			Feed::start(vec![a.track.clone()], "A"),
			Feed::start(vec![b.track.clone()], "B"),
		];
		let a_push = a.push(&mesh.relays[0], lite07(), 1).await;
		let _b_push = b.push(&mesh.relays[3], lite07(), 1).await;
		quiesce().await;
		let mut readers = readers(&mesh).await;
		let nearest = ["A", "A", "B", "B", "B", "A"];
		for (i, reader) in readers.iter_mut().enumerate() {
			assert_eq!(reader.tag().as_deref(), Some(nearest[i]), "{how:?}: R{i} reads its nearest");
		}

		let since = kio::clock::now();
		how.apply(&a_push).await;
		quiesce().await;
		for (i, reader) in readers.iter_mut().enumerate() {
			let report = reader.report(since);
			measured(&format!("unsynchronized pair, A lost by {how:?}, reader R{i}"), &report);
			assert!(report.clean(), "{how:?}: R{i}: {report:?}");
			assert_eq!(reader.tag().as_deref(), Some("B"), "{how:?}: R{i} ends on B");
			let restarts = usize::from(nearest[i] == "A");
			assert_eq!(report.runs.len(), 1 + restarts, "{how:?}: R{i}: {report:?}");
		}
	}
}

/// What sends the GOAWAY.
#[derive(Clone, Copy, Debug)]
enum Drain {
	/// `R0` moves only the publisher's session.
	Publisher,
	/// `R0` drains: it sends GOAWAY on every session it holds, and closes them all.
	Relay,
}

/// How long the publisher keeps its old session once the new one is up, and a draining
/// relay keeps its own.
const HANDOVER: Duration = Duration::from_secs(2);

/// A publisher on `R0` is sent GOAWAY and moves to `R3`, keeping its old session
/// [`HANDOVER`] after the new one is up. Every reader reads on in the broadcast it had, no
/// group missed or repeated, and no link carries a group twice.
async fn handover(drain: Drain) {
	let mut mesh = ring(6).await;
	let source = Source::new(1, None);
	let feed = Feed::start(vec![source.track.clone()], "P");
	let old = source.push(&mesh.relays[0], lite07(), 1).await;
	quiesce().await;
	let mut readers = readers(&mesh).await;

	let since = kio::clock::now();
	old.server.goaway(Goaway::new()).unwrap();
	let neighbours = match drain {
		Drain::Publisher => Vec::new(),
		Drain::Relay => mesh.neighbours(0),
	};
	for &neighbour in &neighbours {
		mesh.session(0, neighbour).goaway(Goaway::new()).unwrap();
	}
	old.client.draining().recv().await.expect("the publisher hears GOAWAY");
	let _new = source.push(&mesh.relays[3], lite07(), 1).await;
	land().await;
	kio::clock::sleep(HANDOVER).await;
	cut(&old);
	for neighbour in neighbours {
		mesh.cut(0, neighbour);
	}
	quiesce().await;

	for (i, reader) in readers.iter_mut().enumerate() {
		let report = reader.report(since);
		measured(&format!("GOAWAY from {drain:?}, reader R{i}"), &report);
		// A draining relay takes its own reader with it.
		if matches!(drain, Drain::Relay) && i == 0 {
			continue;
		}
		assert!(report.seamless(), "{drain:?}: R{i}: {report:?}");
	}
	assert_eq!(mesh.doubled(feed.written()), [], "{drain:?}");
}

#[kio::test]
async fn a_publisher_moved_by_goaway_misses_and_repeats_nothing() {
	handover(Drain::Publisher).await;
}

#[kio::test]
async fn a_draining_relay_hands_its_publisher_over_without_a_gap() {
	handover(Drain::Relay).await;
}
