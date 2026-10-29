#![cfg(feature = "async")]

//! Subscribers asking for different ranges of one track share a single upstream
//! subscription, and each still gets the groups its own range asks for.

use crate::support;

use std::time::Duration;

use moq_net::track::{Info, Range, Subscription};
use moq_net::{Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

const LITE: &[&str] = &["moq-lite-05", "moq-lite-06", "moq-lite-07-wip"];

const TRANSPORT: &[&str] = &["moq-transport-18", "moq-transport-22"];

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) =
		moq_net::origin::Producer::new(moq_net::origin::Config::new(moq_net::Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

fn write_group(track: &moq_net::track::Producer, sequence: u64) {
	let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
	group.write_frame(Timestamp::now(), &b"x"[..]).unwrap();
	group.finish().unwrap();
}

/// The next group's sequence, or `None` once the track ends or nothing arrives in time.
async fn next(sub: &mut moq_net::track::Subscriber) -> Option<u64> {
	let group = moq_clock::timeout(TIMEOUT, sub.recv_group()).await.ok()?;
	group.expect("recv_group").map(|group| group.sequence)
}

async fn read_all(sub: &mut moq_net::track::Subscriber) -> Vec<u64> {
	let mut got = Vec::new();
	while let Some(sequence) = next(sub).await {
		got.push(sequence);
	}
	got
}

async fn subscribe(remote: &moq_net::broadcast::Consumer, subscription: Subscription) -> moq_net::track::Subscriber {
	remote
		.track("quiet")
		.unwrap()
		.subscribe(subscription)
		.await
		.expect("subscribe")
}

/// What a subscriber from group 4 and a live one each read from a quiet track whose
/// newest group is 3, then from group 4 once it arrives. `version` connects them to the
/// publisher, through a relay hop when `relay` is set; `None` reads in-process. The one
/// from group 4 subscribes first, and its request reaches the publisher before the live
/// one joins.
async fn ahead_then_live(version: Option<&str>, relay: bool) -> (Vec<u64>, Vec<u64>) {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast
		.create_track("quiet", Info::default().with_timescale(moq_net::Timescale::MILLI))
		.unwrap();
	broadcast.announce(Default::default()).unwrap();
	for sequence in 0..=3 {
		write_group(&track, sequence);
	}

	let relay_origin = produce_origin(2);
	let subscriber = produce_origin(3);
	let mut sessions = Vec::new();
	let remote = match version {
		None => broadcast.consume(),
		Some(version) => {
			let version: Version = version.parse().unwrap();
			if relay {
				let mut options = MockConnectOptions::new(version);
				options.server_publish = Some(publisher.consume());
				options.client_subscribe = Some(relay_origin.clone());
				sessions.push(connect_mock(options).await);
			}
			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(match relay {
				true => relay_origin.consume(),
				false => publisher.consume(),
			});
			options.client_subscribe = Some(subscriber.clone());
			sessions.push(connect_mock(options).await);

			let consumer = subscriber.consume();
			moq_clock::timeout(TIMEOUT, consumer.follow("bcast", None).unwrap().available())
				.await
				.expect("resolve timeout")
				.expect("broadcast resolves")
		}
	};

	let mut ahead = subscribe(&remote, Subscription::default().with_range(4..)).await;
	moq_clock::timeout(TIMEOUT, crate::support::wanted(track.demand()))
		.await
		.expect("no subscriber appeared")
		.unwrap();
	moq_clock::sleep(Duration::from_millis(10)).await;
	let mut live = subscribe(&remote, Subscription::default().with_ranges([Range::live()])).await;

	// The live subscriber reads group 3 before group 4 supersedes it.
	let mut live_got: Vec<u64> = next(&mut live).await.into_iter().collect();
	write_group(&track, 4);
	track.finish().unwrap();

	live_got.extend(read_all(&mut live).await);
	let ahead_got = read_all(&mut ahead).await;
	drop((track, sessions, relay_origin, subscriber, broadcast, publisher));
	(ahead_got, live_got)
}

/// Lite-07 carries every range a subscriber asks for, so the publisher is asked for only
/// the groups they name; an older wire carries the one span covering them. Either way the
/// subscriber reads just its own groups.
#[moq_clock::test]
async fn sparse_ranges_reach_the_publisher_on_lite07() {
	let sparse = [Range::groups(0..1), Range::groups(3..4)];
	for version in LITE {
		let publisher = produce_origin(1);
		let broadcast = publisher.create_broadcast("bcast").unwrap();
		let track = broadcast
			.create_track("quiet", Info::default().with_timescale(moq_net::Timescale::MILLI))
			.unwrap();
		broadcast.announce(Default::default()).unwrap();

		let subscriber = produce_origin(2);
		let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(subscriber.clone());
		let session = connect_mock(options).await;
		let consumer = subscriber.consume();
		let remote = moq_clock::timeout(TIMEOUT, consumer.follow("bcast", None).unwrap().available())
			.await
			.expect("resolve timeout")
			.expect("broadcast resolves");

		let subscription = Subscription::default()
			.with_max_delay(Duration::from_secs(60))
			.with_ranges(sparse);
		let mut reader = subscribe(&remote, subscription).await;
		moq_clock::timeout(TIMEOUT, crate::support::wanted(track.demand()))
			.await
			.expect("no subscriber appeared")
			.unwrap();
		let asked = track.subscription().expect("the publisher holds the demand").ranges;
		match *version {
			"moq-lite-07-wip" => assert_eq!(asked, sparse, "{version}"),
			_ => assert_eq!(asked.len(), 1, "{version}: one span, got {asked:?}"),
		}

		// Written once the demand is read: lite-07 ends the subscription once it covers them.
		for sequence in 0..=4 {
			write_group(&track, sequence);
		}
		track.finish().unwrap();
		let mut got = read_all(&mut reader).await;
		got.sort_unstable();
		assert_eq!(got, [0, 3], "{version}");
		drop((track, session, subscriber, broadcast, publisher));
	}
}

/// A subscriber from group 4 merged with a live one doesn't floor the shared subscription
/// at 4: the live one still gets group 3.
#[moq_clock::test]
async fn a_live_subscriber_merged_with_one_ahead_still_gets_the_newest_group() {
	let want = (vec![4], vec![3, 4]);
	let mut failures = Vec::new();

	let got = ahead_then_live(None, false).await;
	if got != want {
		failures.push(format!("in-process: {got:?}"));
	}
	for version in LITE.iter().chain(TRANSPORT) {
		for relay in [false, true] {
			let got = ahead_then_live(Some(version), relay).await;
			if got != want {
				failures.push(format!("{version} relay={relay}: {got:?}"));
			}
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}

/// A timed draft-22 track, with or without a forwarding hop.
struct RangeFixture {
	track: moq_net::track::Producer,
	remote: moq_net::broadcast::Consumer,
	_sessions: Vec<support::harness::MockPair>,
	_origins: [moq_net::origin::Producer; 3],
	_broadcast: moq_net::origin::Publication,
}

async fn range_fixture(relay: bool) -> RangeFixture {
	range_fixture_version("moq-transport-22", relay).await
}

async fn range_fixture_version(version: &str, relay: bool) -> RangeFixture {
	range_fixture_history(version, relay, 6).await
}

async fn range_fixture_history(version: &str, relay: bool, history: u64) -> RangeFixture {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("ranges").unwrap();
	let track = broadcast
		.create_track("quiet", Info::default().with_timescale(moq_net::Timescale::MILLI))
		.unwrap();
	broadcast.announce(Default::default()).unwrap();
	for sequence in 0..history {
		let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
		group
			.write_frame(Timestamp::from_millis(sequence * 1000).unwrap(), b"x".as_slice())
			.unwrap();
		group.finish().unwrap();
	}
	let relay_origin = produce_origin(2);
	let subscriber = produce_origin(3);
	let version = version.parse().unwrap();
	let mut sessions = Vec::new();
	if relay {
		let mut options = MockConnectOptions::new(version);
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(relay_origin.clone());
		sessions.push(connect_mock(options).await);
	}
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(if relay {
		relay_origin.consume()
	} else {
		publisher.consume()
	});
	options.client_subscribe = Some(subscriber.clone());
	sessions.push(connect_mock(options).await);
	let consumer = subscriber.consume();
	let remote = consumer.follow("ranges", None).unwrap().available().await.unwrap();
	RangeFixture {
		track,
		remote,
		_sessions: sessions,
		_origins: [publisher, relay_origin, subscriber],
		_broadcast: broadcast,
	}
}

/// Mixed fixed and live ranges retain history for both live and finished cached sources.
#[moq_clock::test]
async fn initial_mixed_ranges_keep_the_absolute_history() {
	for version in ["moq-transport-18", "moq-transport-22"] {
		for relay in [false, true] {
			for finished in [false, true] {
				let fixture = range_fixture_version(version, relay).await;
				if finished {
					let mut warm = subscribe(
						&fixture.remote,
						Subscription::default()
							.with_ranges([Range::live()])
							.with_max_delay(None),
					)
					.await;
					assert_eq!(next(&mut warm).await, Some(5));
					fixture.track.finish().unwrap();
					assert_eq!(next(&mut warm).await, None);
				}
				let subscription = Subscription::default()
					.with_ranges([Range::groups(1..3), Range::live()])
					.with_max_delay(None);
				let mut reader = subscribe(&fixture.remote, subscription).await;
				let mut got = Vec::new();
				for _ in 0..3 {
					got.push(next(&mut reader).await.expect("a mixed range group is missing"));
				}
				got.sort_unstable();
				assert_eq!(got, [1, 2, 5], "{version} relay={relay} finished={finished}");
			}
		}
	}
}

/// A widening fill crosses cached groups without replacing them or losing the groups
/// after them. A later narrowing reaches the publisher and can be widened again.
#[moq_clock::test]
async fn draft22_updates_both_bounds_across_cached_groups() {
	for relay in [false, true] {
		let mut fixture = range_fixture(relay).await;
		let initial = Subscription::default()
			.with_range(2..4)
			.with_max_delay(None)
			.with_order(moq_net::track::Order::Ascending);
		let mut reader = subscribe(&fixture.remote, initial.clone()).await;
		let mut seen = [next(&mut reader).await.unwrap(), next(&mut reader).await.unwrap()];
		seen.sort_unstable();
		assert_eq!(seen, [2, 3]);
		reader.update(initial.clone().with_range(0..6)).unwrap();
		let mut got = Vec::new();
		for _ in 0..4 {
			let seen = next(&mut reader).await;
			got.push(seen.expect("a widened group never arrived"));
		}
		got.sort_unstable();
		assert_eq!(got, [0, 1, 4, 5], "relay={relay}");
		reader.update(initial.clone().with_range(4..5)).unwrap();
		moq_clock::timeout(
			TIMEOUT,
			kio::wait(|waiter| {
				assert!(
					reader.poll_recv_group(waiter).is_pending(),
					"the narrowed reader has no unread group"
				);
				loop {
					if fixture
						.track
						.subscription()
						.is_some_and(|subscription| subscription.ranges == [Range::groups(4..5)])
					{
						return std::task::Poll::Ready(());
					}
					if fixture.track.poll_subscription_changed(waiter).is_pending() {
						return std::task::Poll::Pending;
					}
				}
			}),
		)
		.await
		.expect("the narrowed range never reached the publisher");
		reader.update(initial.with_range(0..)).unwrap();
		let mut group = fixture
			.track
			.create_group(moq_net::group::Info { sequence: 6 })
			.unwrap();
		group
			.write_frame(Timestamp::from_millis(6000).unwrap(), b"later".as_slice())
			.unwrap();
		group.finish().unwrap();
		assert_eq!(next(&mut reader).await, Some(6));
	}
}

/// A warm copy resolves a time end before updating upstream; an unknown end is refused.
#[moq_clock::test]
async fn draft22_resolves_cached_time_ends() {
	for relay in [false, true] {
		let mut fixture = range_fixture(relay).await;
		let initial = Subscription::default()
			.with_range(2..4)
			.with_max_delay(None)
			.with_order(moq_net::track::Order::Ascending);
		let mut reader = subscribe(&fixture.remote, initial.clone()).await;
		let mut seen = [next(&mut reader).await.unwrap(), next(&mut reader).await.unwrap()];
		seen.sort_unstable();
		assert_eq!(seen, [2, 3]);
		let unknown = initial.clone().with_ranges([Range {
			start: moq_net::track::Anchor::Absolute(0),
			end: Some(moq_net::track::Anchor::Time(Timestamp::from_millis(10_000).unwrap())),
		}]);
		assert!(matches!(reader.update(unknown), Err(moq_net::Error::Unsupported)));
		assert_eq!(reader.subscription().ranges, initial.ranges);
		reader
			.update(initial.with_ranges([Range {
				start: moq_net::track::Anchor::Absolute(0),
				end: Some(moq_net::track::Anchor::Time(Timestamp::from_millis(2500).unwrap())),
			}]))
			.unwrap();
		let mut got = vec![next(&mut reader).await.unwrap(), next(&mut reader).await.unwrap()];
		got.sort_unstable();
		assert_eq!(got, [0, 1], "relay={relay}");
		moq_clock::timeout(TIMEOUT, async {
			loop {
				if fixture
					.track
					.subscription()
					.is_some_and(|subscription| subscription.ranges == [Range::groups(0..3)])
				{
					break;
				}
				fixture.track.subscription_changed().await.unwrap();
			}
		})
		.await
		.expect("the resolved group end never reached the publisher");
	}
	let fixture = range_fixture(false).await;
	let requested = Subscription::default().with_ranges([Range {
		start: moq_net::track::Anchor::Absolute(0),
		end: Some(moq_net::track::Anchor::Time(Timestamp::from_millis(2500).unwrap())),
	}]);
	assert!(matches!(
		fixture.remote.track("quiet").unwrap().subscribe(requested).await,
		Err(moq_net::Error::Unsupported)
	));
}

/// A quiet current group does not hold its filled head while a later range update
/// asks for older history; the subgroup header transfers ownership before any tail byte.
#[moq_clock::test]
async fn draft22_can_widen_while_the_current_group_is_quiet() {
	for relay in [false, true] {
		let fixture = range_fixture(relay).await;
		let mut quiet = fixture
			.track
			.create_group(moq_net::group::Info { sequence: 6 })
			.unwrap();
		quiet
			.write_frame(Timestamp::from_millis(6000).unwrap(), b"head".as_slice())
			.unwrap();
		let mut live = subscribe(&fixture.remote, Subscription::default().with_ranges([Range::live()])).await;
		let mut current = moq_clock::timeout(TIMEOUT, live.recv_group())
			.await
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(current.sequence, 6);
		assert_eq!(
			moq_clock::timeout(TIMEOUT, current.read_frame())
				.await
				.unwrap()
				.unwrap()
				.unwrap()
				.payload,
			b"head".as_slice()
		);
		let mut history = subscribe(
			&fixture.remote,
			Subscription::default().with_range(0..2).with_max_delay(None),
		)
		.await;
		let mut got = [next(&mut history).await.unwrap(), next(&mut history).await.unwrap()];
		got.sort_unstable();
		assert_eq!(got, [0, 1], "relay={relay}");
		quiet.finish().unwrap();
		assert!(
			moq_clock::timeout(TIMEOUT, current.read_frame())
				.await
				.unwrap()
				.unwrap()
				.is_none()
		);
	}
}

/// Draft-18 peers already join live and fetch the head. Their filters can narrow that
/// established subscription and widen it over groups published meanwhile.
#[moq_clock::test]
async fn draft18_updates_narrow_then_widen() {
	let version = "moq-transport-18";
	for relay in [false, true] {
		let mut fixture = range_fixture_version(version, relay).await;
		let initial = Subscription::default().with_range(0..).with_max_delay(None);
		let mut reader = subscribe(&fixture.remote, initial.clone()).await;
		let mut seen = Vec::new();
		for _ in 0..6 {
			seen.push(next(&mut reader).await.expect("initial history"));
		}
		seen.sort_unstable();
		assert_eq!(seen, [0, 1, 2, 3, 4, 5], "{version} relay={relay}");
		reader.update(initial.clone().with_range(4..5)).unwrap();
		moq_clock::timeout(
			TIMEOUT,
			kio::wait(|waiter| {
				assert!(reader.poll_recv_group(waiter).is_pending());
				loop {
					if fixture
						.track
						.subscription()
						.is_some_and(|subscription| subscription.ranges == [Range::groups(4..5)])
					{
						return std::task::Poll::Ready(());
					}
					if fixture.track.poll_subscription_changed(waiter).is_pending() {
						return std::task::Poll::Pending;
					}
				}
			}),
		)
		.await
		.unwrap_or_else(|_| panic!("{version} relay={relay}: narrowing did not reach the publisher"));
		for sequence in 6..8 {
			let mut group = fixture.track.create_group(moq_net::group::Info { sequence }).unwrap();
			group
				.write_frame(
					Timestamp::from_millis(sequence * 1000).unwrap(),
					sequence.to_be_bytes().as_slice(),
				)
				.unwrap();
			group.finish().unwrap();
		}
		reader.update(initial).unwrap();
		let mut seen = Vec::new();
		for _ in 0..2 {
			let mut group = moq_clock::timeout(TIMEOUT, reader.recv_group())
				.await
				.unwrap_or_else(|_| panic!("{version} relay={relay}: widening lost history; got {seen:?}"))
				.unwrap()
				.unwrap();
			seen.push(group.sequence);
			let frame = moq_clock::timeout(TIMEOUT, group.read_frame())
				.await
				.unwrap()
				.unwrap()
				.unwrap();
			assert_eq!(frame.payload, group.sequence.to_be_bytes().as_slice());
			assert!(
				moq_clock::timeout(TIMEOUT, group.read_frame())
					.await
					.unwrap()
					.unwrap()
					.is_none()
			);
		}
		seen.sort_unstable();
		assert_eq!(seen, [6, 7], "{version} relay={relay}");
	}
}

/// Empty ranges retire only demand no other reader holds. A later widening reuses
/// the copy and joins the new live group, without disturbing a sibling reader.
#[moq_clock::test]
async fn empty_ranges_retire_the_wire_and_can_widen_again() {
	for version in ["moq-transport-18", "moq-transport-22"] {
		for relay in [false, true] {
			let mut fixture = range_fixture_version(version, relay).await;
			let live = Subscription::default().with_ranges([Range::live()]);
			let mut reader = subscribe(&fixture.remote, live.clone()).await;
			let mut sibling = subscribe(&fixture.remote, live.clone()).await;
			assert_eq!(next(&mut reader).await, Some(5));
			assert_eq!(next(&mut sibling).await, Some(5));
			reader.update(live.clone().with_ranges([])).unwrap();
			assert!(reader.poll_recv_group(&kio::Waiter::noop()).is_pending());
			let mut group = fixture
				.track
				.create_group(moq_net::group::Info { sequence: 6 })
				.unwrap();
			group
				.write_frame(Timestamp::from_millis(6000).unwrap(), b"sibling".as_slice())
				.unwrap();
			group.finish().unwrap();
			assert_eq!(next(&mut sibling).await, Some(6), "{version} relay={relay}");
			reader.update(live.clone()).unwrap();
			assert_eq!(next(&mut reader).await, Some(6));
			drop(sibling);
			reader.update(live.clone().with_ranges([])).unwrap();
			moq_clock::timeout(
				TIMEOUT,
				kio::wait(|waiter| {
					assert!(reader.poll_recv_group(waiter).is_pending());
					loop {
						if fixture.track.subscription().is_none() {
							return std::task::Poll::Ready(());
						}
						if fixture.track.poll_subscription_changed(waiter).is_pending() {
							return std::task::Poll::Pending;
						}
					}
				}),
			)
			.await
			.unwrap_or_else(|_| panic!("{version} relay={relay}: empty demand retained a wire subscription"));
			let mut group = fixture
				.track
				.create_group(moq_net::group::Info { sequence: 7 })
				.unwrap();
			group
				.write_frame(Timestamp::from_millis(7000).unwrap(), b"resumed".as_slice())
				.unwrap();
			group.finish().unwrap();
			reader.update(live).unwrap();
			assert_eq!(next(&mut reader).await, Some(7), "{version} relay={relay}");
		}
	}
}

/// A fill that widens an initially empty copy must finish every completed group,
/// including the snapshot head, before the publisher produces another group.
#[moq_clock::test]
async fn draft22_cold_widening_finishes_every_filled_group() {
	for relay in [false, true] {
		let mut fixture = range_fixture_history("moq-transport-22", relay, 0).await;
		let initial = Subscription::default().with_range(2..).with_max_delay(None);
		let mut reader = subscribe(&fixture.remote, initial.clone()).await;
		for start in [0, 2, 4] {
			let future = start + 2;
			reader.update(initial.clone().with_range(future..)).unwrap();
			moq_clock::timeout(
				TIMEOUT,
				kio::wait(|waiter| {
					assert!(reader.poll_recv_group(waiter).is_pending());
					loop {
						if fixture
							.track
							.subscription()
							.is_some_and(|subscription| subscription.ranges == [Range::groups(future..)])
						{
							return std::task::Poll::Ready(());
						}
						if fixture.track.poll_subscription_changed(waiter).is_pending() {
							return std::task::Poll::Pending;
						}
					}
				}),
			)
			.await
			.unwrap_or_else(|_| panic!("relay={relay}: narrowing to {future} stalled"));
			for sequence in start..future {
				let mut group = fixture.track.create_group(moq_net::group::Info { sequence }).unwrap();
				group
					.write_frame(Timestamp::ZERO, sequence.to_be_bytes().as_slice())
					.unwrap();
				group.finish().unwrap();
			}
			reader.update(initial.clone().with_range(start..)).unwrap();
			let mut sequences = Vec::new();
			for _ in start..future {
				let mut group = moq_clock::timeout(TIMEOUT, reader.recv_group())
					.await
					.unwrap_or_else(|_| {
						panic!("relay={relay}: a filled group from {start} did not arrive; got {sequences:?}")
					})
					.unwrap()
					.unwrap();
				sequences.push(group.sequence);
				let frame = moq_clock::timeout(TIMEOUT, group.read_frame())
					.await
					.unwrap()
					.unwrap()
					.unwrap();
				assert_eq!(frame.payload, group.sequence.to_be_bytes().as_slice());
				assert!(
					moq_clock::timeout(TIMEOUT, group.read_frame())
						.await
						.unwrap_or_else(|_| panic!("relay={relay}: group {} never finished", group.sequence))
						.unwrap()
						.is_none()
				);
			}
			sequences.sort_unstable();
			assert_eq!(sequences, [start, start + 1]);
		}
	}
}

/// A pending refusal is local to the new reader even before it is awaited.
#[moq_clock::test]
async fn unresolved_time_reader_never_changes_sibling_demand() {
	for version in ["moq-transport-18", "moq-transport-22"] {
		for relay in [false, true] {
			let mut fixture = range_fixture_version(version, relay).await;
			let mut healthy = subscribe(&fixture.remote, Subscription::default()).await;
			assert_eq!(next(&mut healthy).await, Some(5));
			healthy.update(Subscription::default().with_range(4..8)).unwrap();
			moq_clock::timeout(
				TIMEOUT,
				kio::wait(|waiter| {
					assert!(healthy.poll_recv_group(waiter).is_pending());
					loop {
						if fixture
							.track
							.subscription()
							.is_some_and(|s| s.ranges == [Range::groups(4..8)])
						{
							return std::task::Poll::Ready(());
						}
						if fixture.track.poll_subscription_changed(waiter).is_pending() {
							return std::task::Poll::Pending;
						}
					}
				}),
			)
			.await
			.unwrap();

			let track = fixture.remote.track("quiet").unwrap();
			// Pin the public track instance through an info request before creating the pending reader.
			let query = track.info();
			let track = query.track();

			let mut refused = Some(track.subscribe(Subscription::default().with_ranges([Range {
				start: moq_net::track::Anchor::Absolute(0),
				end: Some(moq_net::track::Anchor::Time(Timestamp::from_millis(10000).unwrap())),
			}])));
			for sequence in 6..8 {
				if sequence == 7 {
					assert!(matches!(
						refused.take().unwrap().await,
						Err(moq_net::Error::Unsupported)
					));
				}
				let mut group = fixture.track.create_group(moq_net::group::Info { sequence }).unwrap();
				group
					.write_frame(
						Timestamp::from_millis(sequence * 1000).unwrap(),
						sequence.to_be_bytes().as_slice(),
					)
					.unwrap();
				group.finish().unwrap();
				let mut got = moq_clock::timeout(TIMEOUT, healthy.recv_group())
					.await
					.unwrap()
					.unwrap_or_else(|err| panic!("{version} relay={relay}: pending refusal aborted sibling: {err:?}"))
					.unwrap();
				assert_eq!(got.sequence, sequence);
				assert_eq!(
					got.read_frame().await.unwrap().unwrap().payload,
					sequence.to_be_bytes().as_slice()
				);
				assert!(got.read_frame().await.unwrap().is_none());
			}
		}
	}
}

/// A lite-07 publisher ends a bounded subscription once every group in range is
/// delivered or dropped, releasing its demand, and the subscriber keeps the copy: the
/// track does not end, the same range reads again from the cache, and a wider one
/// subscribes again for the rest, past a group that never existed. Older versions park
/// instead, since their subscriber reads a FIN as the track's end.
#[moq_clock::test]
async fn a_covered_bounded_subscription_ends_on_lite07() {
	for version in LITE {
		let publisher = produce_origin(1);
		let broadcast = publisher.create_broadcast("bcast").unwrap();
		let track = broadcast
			.create_track("quiet", Info::default().with_timescale(moq_net::Timescale::MILLI))
			.unwrap();
		broadcast.announce(Default::default()).unwrap();
		for sequence in [0, 1, 2, 4, 5] {
			write_group(&track, sequence);
		}

		let subscriber = produce_origin(2);
		let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(subscriber.clone());
		let session = connect_mock(options).await;
		let consumer = subscriber.consume();
		let remote = moq_clock::timeout(TIMEOUT, consumer.follow("bcast", None).unwrap().available())
			.await
			.expect("resolve timeout")
			.expect("broadcast resolves");

		let bounded = |range: std::ops::Range<u64>| {
			Subscription::default()
				.with_max_delay(Duration::from_secs(60))
				.with_order(moq_net::track::Order::Ascending)
				.with_ranges([Range::groups(range)])
		};
		let mut reader = subscribe(&remote, bounded(1..3)).await;
		assert_eq!(next(&mut reader).await, Some(1), "{version}");
		assert_eq!(next(&mut reader).await, Some(2), "{version}");

		let released = moq_clock::timeout(TIMEOUT, crate::support::unwanted(track.demand())).await;
		if *version != "moq-lite-07-wip" {
			assert!(released.is_err(), "{version}: the publisher parks");
			drop((reader, track, session, subscriber, broadcast, publisher));
			continue;
		}
		released.expect("the covered subscription ends").unwrap();
		assert!(
			moq_clock::timeout(TIMEOUT, reader.finished()).await.is_err(),
			"the track goes on"
		);

		let mut again = subscribe(&remote, bounded(1..3)).await;
		assert_eq!(next(&mut again).await, Some(1));
		assert_eq!(next(&mut again).await, Some(2));
		assert!(track.subscription().is_none(), "the cache answers the same range");

		let mut wider = subscribe(&remote, bounded(1..6)).await;
		let mut got = Vec::new();
		for _ in 0..4 {
			got.extend(next(&mut wider).await);
		}
		assert_eq!(got, [1, 2, 4, 5]);
		// Group 3 may still arrive on a live track, so it is dropped once the feed gives up
		// on it, past the arrivals grace.
		moq_clock::timeout(Duration::from_secs(60), crate::support::unwanted(track.demand()))
			.await
			.expect("the wider subscription ends past the missing group")
			.unwrap();
		drop((reader, again, wider, track, session, subscriber, broadcast, publisher));
	}
}
