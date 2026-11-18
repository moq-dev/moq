//! A relay restarting under its old hop id on the production graph.
//!
//! A relay with a fixed cluster id drains: its publisher reconnects to a sibling
//! relay, keeping its own origin hop, and its old session closes. The relay's peers
//! lose it when the drain ends, cleanly or with the close never arriving, and it
//! comes back under the same hop id. Viewers watch from every relay throughout.
//! Afterwards every relay serves a new request, including for a track no viewer
//! holds open, whose subscription must travel the whole chain to the publisher.

mod support;

use std::{
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
	time::Duration,
};

use moq_net::{Error, Hop, Timestamp, Version, origin, track};
use support::{
	harness::{MockConnectOptions, MockPair, connect_mock, peer_with_latency},
	production::{PRODUCTION, PRODUCTION_LATENCY_MS},
};

/// Bound on a request that should succeed, on the paused clock.
const WAIT: Duration = Duration::from_secs(30);

/// How long a session whose close never arrived lingers before its transport idles it out.
const IDLE: Duration = Duration::from_secs(30);

/// The publisher's origin hop, kept across its move.
const PUBLISHER: u64 = 100;

/// The publisher's relay, a hub, and its sibling in the same PoP.
const FROM: usize = 3;
const TO: usize = 21;

/// A relay far from the publisher that restarts afterwards, as a manual restart
/// to clear a strand would.
const FAR: usize = 13;

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	tokio::spawn(support::harness::run(driver));
	producer
}

/// Connect `publisher` to `relay` as a client publishing into it.
async fn publish(version: Version, publisher: &origin::Producer, relay: &origin::Producer) -> MockPair {
	let mut options = MockConnectOptions::new(version);
	options.client_publish = Some(publisher.consume());
	options.server_subscribe = Some(relay.clone());
	connect_mock(options).await
}

/// End a session: closed now when `clean`, otherwise silent until it idles out.
fn end(pair: &MockPair, clean: bool) {
	if clean {
		pair.client.abort(Error::Cancel);
		pair.server.abort(Error::Cancel);
	} else {
		pair.client_transport.set_latency(Duration::from_secs(3600));
		pair.server_transport.set_latency(Duration::from_secs(3600));
	}
}

/// Write one frame in a fresh group.
fn write(track: &track::Producer, n: u64, payload: &str) {
	let mut group = track.append_group().unwrap();
	group
		.write_frame(Timestamp::from_millis(n * 1000).unwrap(), payload.as_bytes().to_vec())
		.unwrap();
	group.finish().unwrap();
}

/// Keep the publisher live: one group a second, its payload the group's count.
fn ticker(video: track::Producer) -> (Arc<AtomicU64>, tokio::task::JoinHandle<()>) {
	let count = Arc::new(AtomicU64::new(0));
	let task = tokio::spawn({
		let count = count.clone();
		async move {
			loop {
				let n = count.fetch_add(1, Ordering::Relaxed) + 1;
				write(&video, n, &n.to_string());
				tokio::time::sleep(Duration::from_secs(1)).await;
			}
		}
	});
	(count, task)
}

/// A player at `relay` that keeps watching `demo`, subscribing again a second after
/// its track ends.
fn viewer(relay: origin::Producer) -> tokio::task::JoinHandle<()> {
	tokio::spawn(async move {
		loop {
			if let Ok(demo) = relay.consume().request_broadcast("demo").await
				&& let Ok(track) = demo.track("video")
				&& let Ok(mut video) = track.subscribe(None).await
			{
				while let Ok(Some(mut group)) = video.recv_group().await {
					while let Ok(Some(_)) = group.read_frame().await {}
				}
			}
			tokio::time::sleep(Duration::from_secs(1)).await;
		}
	})
}

/// Request `demo` at `relay`, read a video group newer than `after`, then the
/// catalog, which no viewer holds open.
async fn serve(relay: &origin::Producer, after: u64) -> Result<(), Error> {
	let demo = relay.consume().request_broadcast("demo").await?;
	let mut video = demo.track("video")?.subscribe(None).await?;
	loop {
		let mut group = video.recv_group().await?.ok_or(Error::Cancel)?;
		let Some(frame) = group.read_frame().await? else {
			continue;
		};
		if std::str::from_utf8(&frame.payload).unwrap().parse::<u64>().unwrap() > after {
			break;
		}
	}
	let mut catalog = demo.track("catalog")?.subscribe(None).await?;
	let mut group = catalog.recv_group().await?.ok_or(Error::Cancel)?;
	group.read_frame().await?.ok_or(Error::Cancel)?;
	Ok(())
}

/// The production graph's relays, one link per [`PRODUCTION`] edge, and a viewer on each.
struct Fleet {
	version: Version,
	nodes: Vec<origin::Producer>,
	links: Vec<MockPair>,
	viewers: Vec<tokio::task::JoinHandle<()>>,
}

impl Fleet {
	async fn new(version: Version) -> Self {
		let nodes: Vec<_> = (1..=34).map(produce_origin).collect();
		let mut links = Vec::new();
		for (&(a, b), &ms) in PRODUCTION.iter().zip(PRODUCTION_LATENCY_MS) {
			links.push(peer_with_latency(version, &nodes[a], &nodes[b], Duration::from_millis(ms)).await);
		}
		let viewers = nodes.iter().cloned().map(viewer).collect();
		Self {
			version,
			nodes,
			links,
			viewers,
		}
	}

	/// End `relay`'s links with its drain, then bring it back under its old hop id
	/// and peer it again. Returns the old links, for an unclean end to idle out.
	async fn restart(&mut self, relay: usize, clean: bool) -> Vec<MockPair> {
		let touching: Vec<_> = (0..PRODUCTION.len())
			.filter(|&i| PRODUCTION[i].0 == relay || PRODUCTION[i].1 == relay)
			.collect();
		for &i in &touching {
			end(&self.links[i], clean);
		}
		self.viewers[relay].abort();
		tokio::time::sleep(Duration::from_secs(5)).await;

		self.nodes[relay] = produce_origin(relay as u64 + 1);
		self.viewers[relay] = viewer(self.nodes[relay].clone());
		let mut old = Vec::new();
		for i in touching {
			let (a, b) = PRODUCTION[i];
			let latency = Duration::from_millis(PRODUCTION_LATENCY_MS[i]);
			let new = peer_with_latency(self.version, &self.nodes[a], &self.nodes[b], latency).await;
			old.push(std::mem::replace(&mut self.links[i], new));
		}
		old
	}

	/// Every relay serves a new request for `demo`.
	async fn assert_serves(&self, step: &str, count: &AtomicU64) {
		let after = count.load(Ordering::Relaxed);
		let mut requests = tokio::task::JoinSet::new();
		for (relay, node) in self.nodes.iter().enumerate() {
			let node = node.clone();
			requests.spawn(async move { (relay, tokio::time::timeout(WAIT, serve(&node, after)).await) });
		}
		let mut stranded: Vec<_> = requests
			.join_all()
			.await
			.into_iter()
			.filter_map(|(relay, outcome)| match outcome {
				Ok(Ok(())) => None,
				Ok(Err(err)) => Some((relay, err.to_string())),
				Err(_) => Some((relay, "hung".to_string())),
			})
			.collect();
		stranded.sort();
		assert!(stranded.is_empty(), "{step}: {stranded:?}");
	}
}

async fn restart_under_the_same_hop(clean: bool) {
	let version: Version = "moq-lite-06".parse().unwrap();
	let mut fleet = Fleet::new(version).await;

	let publisher = produce_origin(PUBLISHER);
	let demo = publisher.create_broadcast("demo").unwrap();
	let catalog = demo.create_track("catalog", None).unwrap();
	write(&catalog, 0, "catalog");
	let (count, _ticker) = ticker(demo.create_track("video", None).unwrap());
	demo.announce(Default::default()).unwrap();
	let mut from = Some(publish(version, &publisher, &fleet.nodes[FROM]).await);
	tokio::time::sleep(Duration::from_secs(5)).await;
	fleet.assert_serves("start", &count).await;

	// The relay drains: the publisher moves to its sibling, then its old session ends.
	let _to = publish(version, &publisher, &fleet.nodes[TO]).await;
	tokio::time::sleep(origin::DEFAULT_UPDATE_HOLD * 2).await;
	end(from.as_ref().unwrap(), clean);

	for (relay, step) in [(FROM, "publisher's relay restarted"), (FAR, "far relay restarted")] {
		let mut old = fleet.restart(relay, clean).await;
		old.extend(from.take());
		tokio::time::sleep(IDLE).await;
		for pair in &old {
			end(pair, true);
		}
		tokio::time::sleep(Duration::from_secs(10)).await;
		fleet.assert_serves(step, &count).await;
	}
}

#[tokio::test(start_paused = true)]
async fn clean_restart_under_the_same_hop() {
	restart_under_the_same_hop(true).await;
}

/// The close never reaches the peers, so each holds a silent session to the old
/// relay, under the same hop id as the new one, until it idles out.
#[tokio::test(start_paused = true)]
async fn unclean_restart_under_the_same_hop() {
	restart_under_the_same_hop(false).await;
}
