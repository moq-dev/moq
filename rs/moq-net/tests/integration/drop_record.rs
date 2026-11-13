#![cfg(feature = "async")]

//! How each reader's disposition record settles drops, skips, and evictions through a relay:
//! a publisher origin, a relay origin, and a viewer origin joined with lite sessions.

use std::collections::BTreeSet;
use std::time::Duration;

use moq_net::track::{Delay, Info, Range, Subscription};
use moq_net::{Timescale, Timestamp, Version, cache, origin, track};

use crate::support;
use crate::support::harness::{MockConnectOptions, MockPair, connect_mock, spawn};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Groups 0 through 9 present one per second.
const GROUPS: u64 = 10;

/// Reaches groups 6 and up: group 5 ends at 6s, a full 3s behind the 9s edge.
const NARROW: Duration = Duration::from_millis(2500);

const LITE07: &str = "moq-lite-07-wip";

fn origin(hop: u64, pool: cache::Pool) -> origin::Producer {
	let mut config = origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.pool = pool;
	let (origin, driver) = origin::Producer::new(config);
	spawn(driver);
	origin.with_proxy(true)
}

/// A millisecond track with no publisher max age, so only a reader's budget skips its groups.
fn timed() -> Info {
	Info::default().with_timescale(Timescale::MILLI).with_max_age(None)
}

/// Write group `key` presented at `millis`, and finish it.
fn write(source: &track::Producer, key: u64, millis: u64) {
	let mut group = source.create_group(moq_net::group::Info { start: key }).unwrap();
	group
		.write_frame(Timestamp::from_millis(millis).unwrap(), &b"x"[..])
		.unwrap();
	group.close().unwrap();
}

/// A publisher, a relay subscribing to it, and a viewer subscribing to the relay.
struct Chain {
	source: track::Producer,
	relay: origin::Producer,
	viewer: origin::Producer,
	/// The relay's session to the publisher: the client is the relay.
	upstream: MockPair,
	/// The viewer's session to the relay: the server is the relay.
	downstream: MockPair,
	_hold: Vec<Box<dyn std::any::Any>>,
}

impl Chain {
	/// Each hop's wire, and each origin's pool, publisher first.
	async fn new(versions: [&str; 2], pools: [cache::Pool; 3]) -> Self {
		Self::with_credit(versions, pools, None).await
	}

	/// [`Self::new`], with the publisher granted `credit` uni streams open toward the relay.
	async fn with_credit(versions: [&str; 2], pools: [cache::Pool; 3], credit: Option<usize>) -> Self {
		let [publisher, relay, viewer] = pools;
		let publisher = origin(1, publisher);
		let relay = origin(2, relay);
		let viewer = origin(3, viewer);
		let mut options = MockConnectOptions::new(versions[0].parse::<Version>().unwrap());
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(relay.clone());
		options.transport.max_concurrent_uni_streams = credit;
		let upstream = connect_mock(options).await;
		let mut options = MockConnectOptions::new(versions[1].parse::<Version>().unwrap());
		options.server_publish = Some(relay.consume());
		options.client_subscribe = Some(viewer.clone());
		let downstream = connect_mock(options).await;
		let broadcast = publisher
			.route("live", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		let source = broadcast.broadcast().create_track("video", timed()).unwrap();
		broadcast.announce().unwrap();
		Self {
			source,
			relay,
			viewer,
			upstream,
			downstream,
			_hold: vec![Box::new(publisher), Box::new(broadcast)],
		}
	}

	/// Groups 0 through 9, one per second.
	fn populate(&self) {
		for key in 0..GROUPS {
			write(&self.source, key, key * 1000);
		}
	}

	/// The relay's SUBSCRIBE and FETCH streams to the publisher, by their stream types 2 and 3.
	fn asked_upstream(&self) -> (usize, usize) {
		asked(&self.upstream)
	}

	/// The SUBSCRIBE and FETCH streams on both hops.
	fn asked(&self) -> [(usize, usize); 2] {
		[asked(&self.upstream), asked(&self.downstream)]
	}

	async fn track(origin: &origin::Producer) -> track::Consumer {
		let remote = kio::clock::timeout(TIMEOUT, origin.consume().follow("live", None).unwrap().available())
			.await
			.expect("resolve timeout")
			.unwrap()
			.expect("the broadcast resolves");
		remote.track("video").unwrap()
	}

	/// No session on either hop has closed.
	fn assert_open(&self, name: &str) {
		for (hop, pair) in [("upstream", &self.upstream), ("downstream", &self.downstream)] {
			assert_eq!(
				pair.client_transport.close_reason(),
				None,
				"{name}: the {hop} session closed"
			);
		}
	}

	/// Subscribe from the viewer.
	async fn subscribe(&self, subscription: Subscription) -> track::Subscriber {
		let track = Self::track(&self.viewer).await;
		kio::clock::timeout(TIMEOUT, track.subscribe(subscription))
			.await
			.expect("subscribe timeout")
			.unwrap()
	}

	/// Subscribe on the relay's own cache.
	async fn subscribe_relay(&self, subscription: Subscription) -> track::Subscriber {
		let track = Self::track(&self.relay).await;
		kio::clock::timeout(TIMEOUT, track.subscribe(subscription))
			.await
			.expect("subscribe timeout")
			.unwrap()
	}
}

/// The SUBSCRIBE and FETCH streams `pair`'s client opened, by their stream types 2 and 3.
fn asked(pair: &MockPair) -> (usize, usize) {
	let types = pair.client_transport.bidi_types();
	let count = |kind| types.iter().filter(|opened| **opened == kind).count();
	(count(2), count(3))
}

/// A reader of `ranges` with max delay `budget`.
fn reading(budget: impl Into<Option<Duration>>, ranges: impl IntoIterator<Item = Range>) -> Subscription {
	Subscription::default()
		.with_delay(Delay::default().with_max(budget))
		.with_ranges(ranges)
}

/// Every group `subscriber` receives until it goes quiet for a second, or ends.
async fn received(subscriber: &mut track::Subscriber) -> BTreeSet<u64> {
	let mut got = BTreeSet::new();
	while let Ok(group) = kio::clock::timeout(Duration::from_secs(1), subscriber.recv_group()).await {
		let Some(group) = group.expect("the reader runs on") else {
			break;
		};
		assert!(got.insert(group.start), "group {} delivered twice", group.start);
	}
	got
}

/// A pool with no byte target that keeps every group for good.
fn forever() -> cache::Pool {
	cache::Pool::unbounded()
}

/// A pool that reclaims what nobody touched for `expiry`.
fn expiring(expiry: Duration) -> cache::Pool {
	cache::Pool::new(cache::Config::default().with_expiry(expiry))
}

/// A group a relay evicted while its publisher still holds it is never reported dropped: a
/// new reader, a reader that widens its budget, and a reader still owed it each fill it by
/// range from the publisher.
#[kio::test]
async fn a_relay_eviction_is_filled_by_range_for_every_reader() {
	const EXPIRY: Duration = Duration::from_secs(10);
	kio::clock::timeout(TIMEOUT * 10, async {
		let chain = Chain::new([LITE07, LITE07], [forever(), expiring(EXPIRY), expiring(EXPIRY)]).await;
		chain.populate();

		// A reader with no budget warms the relay with every group, then leaves.
		let mut warm = chain.subscribe(reading(None, [Range::oldest()])).await;
		assert_eq!(received(&mut warm).await, (0..GROUPS).collect());
		drop(warm);

		let mut widened = chain.subscribe(reading(NARROW, [Range::oldest()])).await;
		assert_eq!(received(&mut widened).await, (6..GROUPS).collect());
		// Owed every group from 0, on the relay's own cache, and reading none of them yet.
		let mut owed = chain.subscribe_relay(reading(None, [support::keys(0..)])).await;

		// Nobody touches them, so the relay and the viewer reclaim all but the live edge. Every
		// group is stale by the wall clock now, so only a reader with no budget wants them.
		kio::clock::sleep(EXPIRY * 3).await;
		let (subscribes, _) = chain.asked_upstream();

		widened.update(reading(None, [Range::oldest()])).unwrap();
		assert_eq!(received(&mut widened).await, (0..6).collect(), "the widened reader");

		let mut new = chain.subscribe(reading(None, [support::keys(1..4)])).await;
		assert_eq!(received(&mut new).await, (1..4).collect(), "the new reader");

		assert_eq!(received(&mut owed).await, (0..GROUPS).collect(), "the owed reader");
		let (filled, fetches) = chain.asked_upstream();
		assert!(
			filled > subscribes,
			"the relay filled what it evicted from the publisher"
		);
		assert_eq!(fetches, 0, "every fill rides a subscription");
	})
	.await
	.expect("timed out");
}

/// What settles the run between group 0 and the far group in [`a_huge_run_settles_in_one_step`].
#[derive(Clone, Copy, Debug)]
enum Huge {
	/// The publisher never produces it, which its end names as one drop.
	Dropped,
	/// The publisher skips it.
	Skipped,
	/// Dropped, then forgotten by the relay and the viewer, so a later reader asks again.
	Forgotten,
}

/// A single run spanning 2^40 keys is settled by each cursor in one step, whether it is
/// dropped, skipped, or forgotten and judged again: every reader, narrow or wide, gets the
/// groups on either side and ends at once, on mock time, rather than walking the run.
#[kio::test]
async fn a_huge_run_settles_in_one_step() {
	const FAR: u64 = 1 << 40;
	const EXPIRY: Duration = Duration::from_secs(10);
	for huge in [Huge::Dropped, Huge::Skipped, Huge::Forgotten] {
		kio::clock::timeout(TIMEOUT * 10, async {
			let chain = Chain::new([LITE07, LITE07], [forever(), expiring(EXPIRY), expiring(EXPIRY)]).await;
			let everything = || reading(None, [support::keys(0..)]);
			let mut narrow = chain.subscribe(reading(NARROW, [support::keys(0..)])).await;
			let mut wide = chain.subscribe(everything()).await;
			write(&chain.source, 0, 0);
			if let Huge::Skipped = huge {
				chain.source.skip_groups(1..FAR);
			}
			// Presented just after group 0, since its key in milliseconds overflows.
			write(&chain.source, FAR, 1);
			let _finished = chain.source.clone().finish().unwrap();

			let started = kio::clock::now();
			for (name, reader) in [("narrow", &mut narrow), ("wide", &mut wide)] {
				assert_eq!(drain(reader).await, [0, FAR].into(), "{huge:?}: the {name} reader");
			}
			assert!(
				kio::clock::now() - started < Duration::from_secs(1),
				"{huge:?}: ended after {:?}",
				kio::clock::now() - started
			);

			if let Huge::Forgotten = huge {
				drop((narrow, wide));
				kio::clock::sleep(EXPIRY * 3).await;
				let (subscribes, _) = chain.asked_upstream();
				let started = kio::clock::now();
				let mut late = chain.subscribe(everything()).await;
				assert_eq!(drain(&mut late).await, [0, FAR].into(), "the later reader");
				assert!(
					kio::clock::now() - started < Duration::from_secs(1),
					"judged again after {:?}",
					kio::clock::now() - started
				);
				assert!(chain.asked_upstream().0 > subscribes, "the relay asked again");
			}
		})
		.await
		.unwrap_or_else(|_| panic!("{huge:?}: timed out"));
	}
}

/// Every group `subscriber` receives until the track ends cleanly.
async fn drain(subscriber: &mut track::Subscriber) -> BTreeSet<u64> {
	let mut got = BTreeSet::new();
	while let Some(group) = kio::clock::timeout(TIMEOUT, subscriber.recv_group())
		.await
		.expect("the reader stalled")
		.expect("the reader runs on")
	{
		assert!(got.insert(group.start), "group {} delivered twice", group.start);
	}
	got
}

const LITE06: &str = "moq-lite-06";

/// Through a lite-06 hop, a group the publisher reported dropped stays dropped for every
/// reader, whatever budget it widens to, with no session error. Lite-07 end to end, the same
/// stale report is reopened by the widening update instead. A shrink sends no drops: what an
/// update narrows away is still there for a later reader.
#[kio::test]
async fn a_lite06_hop_keeps_a_reported_group_dropped() {
	for versions in [[LITE07, LITE06], [LITE06, LITE06], [LITE07, LITE07]] {
		reported_drop_through(versions).await;
	}
}

/// [`a_lite06_hop_keeps_a_reported_group_dropped`] with the lite-06 hop upstream of a lite-07
/// one: a lite-06 drop is final for the relay's subscription, which no update re-offers, but
/// it was judged under that subscription's budget. The relay forwards it as a stale one, the
/// viewer's widening update reopens it, and the group is fetched from the publisher, which
/// still holds it, as the relay drills' FETCH recovery requires.
#[kio::test]
async fn a_lite06_upstream_drop_is_fetched_for_a_widened_lite07_viewer() {
	reported_drop_through([LITE06, LITE07]).await;
}

async fn reported_drop_through(versions: [&str; 2]) {
	// Reaches groups 5 and up by media time.
	const B1: Duration = Duration::from_millis(3500);
	// Reaches every group, by media time and by the wall clock this test spends.
	const B2: Duration = Duration::from_secs(60);
	let name = format!("{} then {}", versions[0], versions[1]);
	kio::clock::timeout(TIMEOUT * 10, async {
		let chain = Chain::new(versions, [forever(), forever(), forever()]).await;
		for key in (0..GROUPS).filter(|key| *key != 7) {
			write(&chain.source, key, key * 1000);
		}
		let mut first = chain.subscribe(reading(B1, [Range::oldest()])).await;
		assert_eq!(received(&mut first).await, [5, 6, 8, 9].into(), "{name}");

		// Group 7 lands once its successor is older than B1 by the wall clock, so the
		// publisher reports it stale under B1.
		kio::clock::sleep(B1 + Duration::from_secs(1)).await;
		write(&chain.source, 7, 7000);
		assert_eq!(received(&mut first).await, BTreeSet::new(), "{name}");

		// A lite-07 viewer hop leaves the stale report open to a widening update, which fills
		// the group from upstream; over a lite-06 viewer hop the drop is final.
		let want: BTreeSet<u64> = match versions[1] == LITE06 {
			true => (0..GROUPS).filter(|key| *key != 7).collect(),
			false => (0..GROUPS).collect(),
		};
		first.update(reading(B2, [Range::oldest()])).unwrap();
		assert_eq!(
			received(&mut first).await,
			&want - &[5, 6, 8, 9].into(),
			"{name}: the widened reader"
		);
		let mut second = chain.subscribe(reading(B2, [Range::oldest()])).await;
		assert_eq!(received(&mut second).await, want, "{name}: the later reader");

		let _finished = chain.source.clone().finish().unwrap();
		for (reader, which) in [(&mut first, "first"), (&mut second, "second")] {
			assert_eq!(drain(reader).await, BTreeSet::new(), "{name}: the {which} reader ends");
		}
		chain.assert_open(&name);
	})
	.await
	.unwrap_or_else(|_| panic!("{name}: timed out"));

	// The shrink reaches the publisher before any group it narrows away exists.
	kio::clock::timeout(TIMEOUT * 10, async {
		let chain = Chain::new(versions, [forever(), forever(), forever()]).await;
		let mut shrunk = chain.subscribe(reading(None, [support::keys(0..)])).await;
		shrunk.update(reading(None, [support::keys(8..)])).unwrap();
		kio::clock::sleep(Duration::from_secs(1)).await;
		chain.populate();
		assert_eq!(received(&mut shrunk).await, [8, 9].into(), "{name}: the shrunk reader");

		let mut later = chain.subscribe(reading(None, [support::keys(0..)])).await;
		assert_eq!(
			received(&mut later).await,
			(0..GROUPS).collect(),
			"{name}: the later reader"
		);
		chain.assert_open(&name);
	})
	.await
	.unwrap_or_else(|_| panic!("{name}: shrink timed out"));
}

/// An origin's own eviction and retention expiry of a group it still owed a subscriber is a
/// final drop (EVICTED, not OLD), which the relay forwards as it is: the viewer's reader settles
/// on it and ends at once, and neither a widening update nor a later reader reopens it, so
/// nothing asks for the group again on either hop.
#[kio::test]
async fn an_origin_eviction_drop_is_forwarded_final() {
	const EXPIRY: Duration = Duration::from_secs(10);
	const PAYLOAD: usize = 16 * 1024;
	for expiry in [true, false] {
		kio::clock::timeout(TIMEOUT * 10, async {
			let pool = match expiry {
				true => expiring(EXPIRY),
				false => cache::Pool::new(cache::Config::default().with_capacity(4 * PAYLOAD as u64)),
			};
			// One stream of credit: group 0's, held from the relay, so the rest queue.
			let chain = Chain::with_credit([LITE07, LITE07], [pool, forever(), forever()], Some(1)).await;
			let mut reader = chain.subscribe(reading(None, [support::keys(0..)])).await;
			chain.upstream.server_transport.hold_unis();
			let write = |key: u64| {
				let mut group = chain.source.create_group(moq_net::group::Info { start: key }).unwrap();
				group
					.write_frame(Timestamp::from_millis(key * 1000).unwrap(), vec![0u8; PAYLOAD])
					.unwrap();
				group.close().unwrap();
			};
			for key in 0..4 {
				write(key);
			}
			let last = match expiry {
				// Nobody reads the queued groups, so the origin reclaims all but the newest.
				true => {
					kio::clock::sleep(EXPIRY * 3).await;
					3
				}
				// The newer groups push the queued ones out.
				false => {
					for key in 4..8 {
						write(key);
					}
					7
				}
			};
			chain.upstream.server_transport.release_unis();
			let got = received(&mut reader).await;
			assert!(got.contains(&last), "expiry={expiry}: {got:?}");
			assert!(got.len() <= last as usize, "expiry={expiry}: nothing evicted: {got:?}");
			let finished = chain.source.clone().finish().unwrap();
			let started = kio::clock::now();
			assert_eq!(drain(&mut reader).await, BTreeSet::new(), "expiry={expiry}");
			assert!(
				kio::clock::now() - started < Duration::from_secs(1),
				"expiry={expiry}: the evicted groups held the end for {:?}",
				kio::clock::now() - started
			);

			// A later reader may subscribe afresh once the finished track's copies are gone,
			// so only the update is held to asking nothing again.
			let mut later = chain.subscribe(reading(None, [support::keys(0..)])).await;
			assert_eq!(drain(&mut later).await, got, "expiry={expiry}: the later reader");
			let asked = chain.asked();
			reader
				.update(reading(Duration::from_secs(3600), [support::keys(0..)]))
				.unwrap();
			assert_eq!(
				drain(&mut reader).await,
				BTreeSet::new(),
				"expiry={expiry}: the updated reader"
			);
			assert_eq!(
				chain.asked(),
				asked,
				"expiry={expiry}: an evicted group was asked again"
			);
			drop(finished);
		})
		.await
		.unwrap_or_else(|_| panic!("expiry={expiry}: timed out"));
	}
}

/// After a widening from max delay B1 to B2 for a group the publisher no longer holds, its
/// stale drop under B1, still in flight when the relay sends the B2 update, settles no reader;
/// the drop the publisher sends again under the B2 update settles the B2 reader, over lite-07.
#[kio::test]
async fn a_drop_under_a_superseded_budget_settles_no_reader() {
	// Reaches groups 5 and up by media time.
	const B1: Duration = Duration::from_millis(3500);
	// Reaches every group, by media time and by the wall clock this test spends.
	const B2: Duration = Duration::from_secs(60);
	// The publisher reclaims an unread group well within one trip to the relay and back.
	const EXPIRY: Duration = Duration::from_secs(2);
	// One way between the publisher and the relay.
	const LATENCY: Duration = Duration::from_secs(10);
	kio::clock::timeout(TIMEOUT * 10, async {
		let chain = Chain::new([LITE07, LITE07], [expiring(EXPIRY), forever(), forever()]).await;
		for key in (0..GROUPS).filter(|key| *key != 7) {
			write(&chain.source, key, key * 1000);
		}
		let mut reader = chain.subscribe(reading(B1, [Range::oldest()])).await;
		assert_eq!(received(&mut reader).await, [5, 6, 8, 9].into());

		// Group 7 lands once its successor is older than B1 by the wall clock: the publisher
		// drops it as stale under B1 at once, and reclaims it soon after, unread.
		kio::clock::sleep(B1 + Duration::from_secs(1)).await;
		chain.upstream.client_transport.set_latency(LATENCY);
		let sent = kio::clock::now();
		write(&chain.source, 7, 7000);
		let _finished = chain.source.clone().finish().unwrap();

		// The B2 update leaves the relay while the B1 drop is halfway there.
		kio::clock::sleep(LATENCY / 2).await;
		// Any request the relay opens upstream from now on never arrives, so only the
		// subscription can settle group 7.
		chain.upstream.client_transport.hold_bidis();
		reader.update(reading(B2, [Range::oldest()])).unwrap();
		let ended = kio::clock::timeout(LATENCY * 10, reader.recv_group())
			.await
			.expect("the reader never settled group 7")
			.unwrap()
			.map(|group| group.start);
		assert_eq!(ended, None, "group 7 was reclaimed");
		// The B1 drop reached the relay at one latency, the B2 update the publisher at one and
		// a half, and its drop the relay at two and a half.
		let elapsed = kio::clock::now() - sent;
		assert!(
			elapsed >= LATENCY * 5 / 2 && elapsed < LATENCY * 5 / 2 + Duration::from_secs(1),
			"settled after {elapsed:?}"
		);
	})
	.await
	.expect("timed out");
}

/// A run dropped before the track's first group is settled through a relay for a later
/// reader whose range covers it: the reader gets the group after it and ends, as one reading the
/// relay's own cache does.
#[kio::test]
async fn a_run_dropped_before_the_first_group_settles_a_later_range() {
	let chain = Chain::new([LITE07, LITE07], [forever(), forever(), forever()]).await;
	let mut reader = chain.subscribe(reading(None, [support::keys(0..)])).await;
	for key in 0..2 {
		let group = chain.source.create_group(moq_net::group::Info { start: key }).unwrap();
		group.abort(moq_net::Error::App(1)).unwrap();
	}
	write(&chain.source, 2, 0);
	assert_eq!(received(&mut reader).await, [2].into());

	let mut viewer = chain.subscribe(reading(None, [support::keys(0..3)])).await;
	assert_eq!(drain(&mut viewer).await, [2].into(), "on the viewer");
	let mut relayed = chain.subscribe_relay(reading(None, [support::keys(0..3)])).await;
	assert_eq!(drain(&mut relayed).await, [2].into(), "on the relay");
}

/// Under pool pressure on the relay, a reader whose record holds only delivered runs and
/// budget-independent drops folds them into its floor and reads on, while one holding lite-07
/// stale reports within reach of the max age bound ends rather than forgo the re-offer: with
/// `Lagged` on the relay itself, and `TooFarBehind` over the wire.
#[kio::test]
async fn relay_pool_pressure_folds_final_runs_and_ends_stale_reports() {
	/// The relay's readers each record this many disjoint dropped runs.
	const RUNS: u64 = 200;
	/// The groups that land on the relay at once once its pool shrinks.
	const BURST: u64 = 1000;
	let budget = Duration::from_secs(2);
	for stale in [false, true] {
		kio::clock::timeout(TIMEOUT * 10, async {
			let pool = cache::Pool::new(cache::Config::default().with_expiry(None));
			let chain = Chain::new([LITE07, LITE07], [forever(), pool.clone(), forever()]).await;
			let mut local = chain.subscribe_relay(reading(budget, [support::keys(0..)])).await;
			let mut remote = chain.subscribe(reading(budget, [support::keys(0..)])).await;
			let odd = write_odd(&chain, RUNS);
			for (reader, name) in [(&mut local, "local"), (&mut remote, "remote")] {
				assert_eq!(received(reader).await, odd, "stale={stale}: the {name} reader");
			}
			settle_even(&chain, RUNS, stale.then_some(budget * 2)).await;
			for (reader, name) in [(&mut local, "local"), (&mut remote, "remote")] {
				assert_eq!(
					received(reader).await,
					BTreeSet::new(),
					"stale={stale}: the {name} reader"
				);
			}

			// The pool shrinks, and a burst of groups accrues the relay's debt: its own groups
			// and drop set go first, then its readers' records.
			pool.resize(4 * 1024);
			for key in 2 * RUNS..2 * RUNS + BURST {
				write(&chain.source, key, key);
			}
			let mut read = [0, 0];
			let mut ended = [None, None];
			for (index, reader) in [&mut local, &mut remote].into_iter().enumerate() {
				while let Ok(group) = kio::clock::timeout(Duration::from_secs(1), reader.recv_group()).await {
					match group {
						Ok(Some(_)) => read[index] += 1,
						Ok(None) => panic!("stale={stale}: reader {index} ended cleanly"),
						Err(err) => {
							ended[index] = Some(err);
							break;
						}
					}
				}
			}
			match stale {
				false => {
					assert!(ended.iter().all(Option::is_none), "the final runs fold: {ended:?}");
					// Under this much pressure the relay may evict a burst group before serving it,
					// so only the local reader's count is exact.
					assert_eq!(read[0], BURST, "the local reader reads on");
					assert!(read[1] > 0, "the remote reader reads on");
				}
				true => {
					let [local, remote] = ended;
					assert!(matches!(local, Some(moq_net::Error::Lagged)), "local: {local:?}");
					assert!(
						matches!(remote, Some(moq_net::Error::Stream(moq_net::StreamError::TooFarBehind))),
						"remote: {remote:?}"
					);
				}
			}
			chain.assert_open(&format!("stale={stale}"));
		})
		.await
		.unwrap_or_else(|_| panic!("stale={stale}: timed out"));
	}
}

/// Write the odd keys below `2 * runs`, presented a millisecond apart so only the wall
/// clock makes one stale, and return them.
fn write_odd(chain: &Chain, runs: u64) -> BTreeSet<u64> {
	for run in 0..runs {
		write(&chain.source, 2 * run + 1, 2 * run + 1);
	}
	(0..runs).map(|run| 2 * run + 1).collect()
}

/// Settle the even keys below `2 * runs` as disjoint runs. With `stale`, each group lands
/// that long after its successor, so a reader with a shorter budget drops it as stale; without,
/// each is aborted, a drop no budget changes.
async fn settle_even(chain: &Chain, runs: u64, stale: Option<Duration>) {
	match stale {
		Some(late) => {
			kio::clock::sleep(late).await;
			for run in 0..runs {
				write(&chain.source, 2 * run, 2 * run);
			}
		}
		None => {
			for run in 0..runs {
				let group = chain
					.source
					.create_group(moq_net::group::Info { start: 2 * run })
					.unwrap();
				group.abort(moq_net::Error::App(1)).unwrap();
			}
		}
	}
}
