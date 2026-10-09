//! Subscribers with different floors share one upstream subscription, and each still
//! receives what its own floor and `max_delay` allow.
//!
//! A floor and `live` are separate: the aggregate is the lowest floor plus `live` when
//! anyone wants it. A resumed subscriber whose floor sits above the live edge must never
//! hide the latest group from a subscriber that wants it, and a wire without a `live`
//! field asks for enough that the relay can filter locally.

mod support;

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version, broadcast, group, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(5);

/// Long enough to say a group is not coming.
const QUIET: Duration = Duration::from_millis(500);

/// A budget no group outlives.
const FOREVER: Duration = Duration::from_millis((1 << 53) - 1);

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

async fn link(version: Version, from: &origin::Producer, to: &origin::Producer) -> MockPair {
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(from.consume());
	options.client_subscribe = Some(to.clone());
	connect_mock(options).await
}

async fn request(origin: &origin::Producer) -> broadcast::Consumer {
	let consumer = origin.consume();
	moq_net_sim::timeout(TIMEOUT, consumer.routed("live"))
		.await
		.expect("announce timeout")
		.expect("routed");
	moq_net_sim::timeout(TIMEOUT, consumer.request_broadcast("live", None))
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves")
}

/// Announce `live/track`, retained for good.
fn publish(origin: &origin::Producer) -> (broadcast::Producer, track::Producer) {
	let broadcast = origin.create_broadcast("live").unwrap();
	let track = broadcast
		.create_track("track", track::Info::default().with_max_age(FOREVER))
		.unwrap();
	broadcast.announce(Default::default()).unwrap();
	(broadcast, track)
}

/// Write a finished group of `frames` frames, stamped `millis`.
fn write_group(track: &mut track::Producer, sequence: u64, millis: u64, frames: usize) {
	let mut group = track.create_group(group::Info { sequence }).unwrap();
	for _ in 0..frames {
		group
			.write_frame(Timestamp::from_millis(millis).unwrap(), &b"x"[..])
			.unwrap();
	}
	group.finish().unwrap();
}

/// A floor alone, as a resumed subscriber names where it left off.
fn floored(floor: track::Position) -> track::Subscription {
	track::Subscription::default().with_live(false).with_floor(floor)
}

/// The next group and the index of its first frame, or `None` if nothing arrives.
async fn next(sub: &mut track::Subscriber, wait: Duration) -> Option<(u64, u64)> {
	let group = moq_net_sim::timeout(wait, sub.recv_group()).await.ok()?;
	let group = group.expect("recv_group")?;
	Some((group.sequence, group.index()))
}

/// The sequences of the next `count` groups, sorted.
async fn groups(sub: &mut track::Subscriber, count: usize) -> Vec<u64> {
	let mut got = Vec::new();
	while got.len() < count {
		match next(sub, TIMEOUT).await {
			Some((sequence, _)) => got.push(sequence),
			None => break,
		}
	}
	got.sort();
	got
}

/// Run `scenario` for each version and report every failure together.
async fn each_version<F: std::future::Future<Output = Result<(), String>>>(
	versions: &[&str],
	scenario: impl Fn(Version) -> F,
) {
	let mut failures = Vec::new();
	for version in versions {
		let version: Version = version.parse().unwrap();
		match moq_net_sim::timeout(Duration::from_secs(60), scenario(version)).await {
			Ok(Ok(())) => {}
			Ok(Err(err)) => failures.push(format!("{version}: {err}")),
			Err(_) => failures.push(format!("{version}: scenario timed out")),
		}
	}
	assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// Wait until the publisher's aggregate satisfies `done`.
async fn aggregate(track: &track::Producer, done: impl Fn(&track::Subscription) -> bool) -> Result<(), String> {
	let mut track = track.clone();
	moq_net_sim::timeout(TIMEOUT, async {
		while !track.subscription().is_some_and(|sub| done(&sub)) {
			track.subscription_changed().await.unwrap();
		}
	})
	.await
	.map_err(|_| format!("the publisher never saw the demand: {:?}", track.subscription()))
}

/// A quiet track's newest group is 3. A resumed subscriber floors at 4, and a live one
/// joins after it through the same relay. The live one gets group 3 at once, the resumed
/// one does not, and both get group 4 once it exists. `downstream` reads one more hop
/// away instead of in-process at the relay.
async fn starve(version: Version, downstream: bool) -> Result<(), String> {
	let publisher = produce_origin(1);
	let relay = produce_origin(2);
	let (_broadcast, mut track) = publish(&publisher);
	for sequence in 0..4 {
		write_group(&mut track, sequence, sequence * 100, 1);
	}

	let _upstream = link(version, &publisher, &relay).await;
	let subscriber = produce_origin(3);
	let _downstream = link(version, &relay, &subscriber).await;
	let remote = request(if downstream { &subscriber } else { &relay }).await;

	let mut resumed = moq_net_sim::timeout(
		TIMEOUT,
		remote
			.track("track")
			.unwrap()
			.subscribe(floored(track::Position::group(4))),
	)
	.await
	.map_err(|_| "resume never resolved")?
	.map_err(|err| format!("resume: {err}"))?;
	aggregate(&track, |sub| sub.floor == Some(track::Position::group(4))).await?;

	let mut live = moq_net_sim::timeout(TIMEOUT, remote.track("track").unwrap().subscribe(None))
		.await
		.map_err(|_| "live never resolved")?
		.map_err(|err| format!("live: {err}"))?;
	match next(&mut live, TIMEOUT).await {
		Some((3, 0)) => {}
		other => return Err(format!("the live subscriber got {other:?}, not group 3")),
	}
	if let Some(got) = next(&mut resumed, QUIET).await {
		return Err(format!("the resume got {got:?} below its floor"));
	}

	write_group(&mut track, 4, 400, 1);
	for (who, sub) in [("live", &mut live), ("resumed", &mut resumed)] {
		match next(sub, TIMEOUT).await {
			Some((4, 0)) => {}
			other => return Err(format!("the {who} subscriber got {other:?}, not group 4")),
		}
	}
	Ok(())
}

#[moq_net_sim::test]
async fn a_floor_above_the_live_edge_never_starves_a_live_subscriber() {
	each_version(&["moq-lite-06", "moq-lite-07-wip"], |version| starve(version, false)).await;
}

#[moq_net_sim::test]
async fn a_floor_above_the_live_edge_never_starves_a_live_subscriber_downstream() {
	each_version(&["moq-lite-06", "moq-lite-07-wip"], |version| starve(version, true)).await;
}

/// A buffering live subscriber (max age > 0) shares an upstream subscription with a floor
/// above the live edge, and still receives the backlog its budget allows.
#[moq_net_sim::test]
async fn a_buffered_live_subscriber_keeps_its_backlog_beside_a_floor() {
	each_version(
		&["moq-lite-06", "moq-lite-07-wip", "moq-transport-22"],
		|version| async move {
			let publisher = produce_origin(1);
			let relay = produce_origin(2);
			let (_broadcast, mut track) = publish(&publisher);
			for sequence in 2..5 {
				write_group(&mut track, sequence, sequence * 100, 1);
			}
			let _upstream = link(version, &publisher, &relay).await;

			let remote = request(&relay).await;
			let handle = remote.track("track").unwrap();
			// Registered together, so the first upstream request already merges them.
			let resumed = handle.subscribe(floored(track::Position::group(5)));
			let live = handle.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(10)));
			let _resumed = resumed.await.map_err(|err| format!("resume: {err}"))?;
			let mut live = live.await.map_err(|err| format!("live: {err}"))?;

			match groups(&mut live, 3).await.as_slice() {
				[2, 3, 4] => Ok(()),
				got => Err(format!("the buffered live subscriber got {got:?}, not 2..=4")),
			}
		},
	)
	.await;
}

/// A floor partway through the latest group merges with `live`, which starts the group at
/// its first frame: the live subscriber still gets frames 0-4.
#[moq_net_sim::test]
async fn a_frame_floor_merged_with_live_keeps_the_head_of_the_group() {
	each_version(&["moq-lite-06", "moq-lite-07-wip"], |version| async move {
		let publisher = produce_origin(1);
		let relay = produce_origin(2);
		let (_broadcast, mut track) = publish(&publisher);
		write_group(&mut track, 3, 300, 8);
		let _upstream = link(version, &publisher, &relay).await;

		let remote = request(&relay).await;
		let handle = remote.track("track").unwrap();
		let resumed = handle.subscribe(floored(track::Position { group: 3, frame: 5 }));
		let live = handle.subscribe(None);
		let _resumed = resumed.await.map_err(|err| format!("resume: {err}"))?;
		let mut live = live.await.map_err(|err| format!("live: {err}"))?;

		let group = moq_net_sim::timeout(TIMEOUT, live.recv_group())
			.await
			.map_err(|_| "no group arrived")?
			.map_err(|err| format!("recv: {err}"))?
			.ok_or("the track ended")?;
		if (group.sequence, group.index()) != (3, 0) {
			return Err(format!("got group {} from frame {}", group.sequence, group.index()));
		}
		let mut group = group;
		let mut frames = 0;
		while let Ok(Ok(Some(_))) = moq_net_sim::timeout(TIMEOUT, group.read_frame()).await {
			frames += 1;
		}
		match frames {
			8 => Ok(()),
			frames => Err(format!("got {frames} of group 3's 8 frames")),
		}
	})
	.await;
}

/// `live` merged with a floor of 2 through a peer whose SUBSCRIBE has no `Live` field: the
/// relay asks from group 0 and filters locally, so the floor still gets groups 2-4.
#[moq_net_sim::test]
async fn live_merged_with_a_floor_reaches_back_through_an_older_lite_peer() {
	each_version(&["moq-lite-03", "moq-lite-04", "moq-lite-05"], |version| async move {
		let publisher = produce_origin(1);
		let relay = produce_origin(2);
		let (_broadcast, mut track) = publish(&publisher);
		for sequence in 2..5 {
			write_group(&mut track, sequence, sequence * 100, 1);
		}
		let _upstream = link(version, &publisher, &relay).await;

		let remote = request(&relay).await;
		let handle = remote.track("track").unwrap();
		let floor = handle.subscribe(floored(track::Position::group(2)).with_max_delay(FOREVER));
		let live = handle.subscribe(None);
		let mut floor = floor.await.map_err(|err| format!("floor: {err}"))?;
		let _live = live.await.map_err(|err| format!("live: {err}"))?;

		match groups(&mut floor, 3).await.as_slice() {
			[2, 3, 4] => Ok(()),
			got => Err(format!("the floored subscriber got {got:?}, not 2..=4")),
		}
	})
	.await;
}

/// A buffering `live` merged with a floor above the live edge, over a moq-transport
/// upstream: the relay subscribes from group 0 and still delivers the fresh groups 2-4.
///
/// Before draft-20 that start is a joining FETCH from group 0, which spans several groups
/// and is refused (one group per FETCH), so only the live edge arrives there.
#[moq_net_sim::test]
async fn live_merged_with_a_floor_reaches_back_through_moq_transport() {
	each_version(&["moq-transport-16", "moq-transport-22"], |version| async move {
		let publisher = produce_origin(1);
		let relay = produce_origin(2);
		let (_broadcast, mut track) = publish(&publisher);
		for sequence in 2..5 {
			write_group(&mut track, sequence, sequence * 100, 1);
		}
		let _upstream = link(version, &publisher, &relay).await;

		let remote = request(&relay).await;
		let handle = remote.track("track").unwrap();
		let resumed = handle.subscribe(floored(track::Position::group(5)));
		let live = handle.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(10)));
		let _resumed = resumed.await.map_err(|err| format!("resume: {err}"))?;
		let mut live = live.await.map_err(|err| format!("live: {err}"))?;

		let expected: &[u64] = match version.to_string().as_str() {
			"moq-transport-16" => &[4],
			_ => &[2, 3, 4],
		};
		match groups(&mut live, expected.len()).await {
			got if got == expected => Ok(()),
			got => Err(format!("the buffered live subscriber got {got:?}, not {expected:?}")),
		}
	})
	.await;
}
