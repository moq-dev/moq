#![cfg(feature = "async")]

//! The lite-07 range table (`tests/range-table.json`), which `js/net/src/range-table.test.ts`
//! passes too: each case's reader gets exactly the listed groups direct from the origin,
//! through a relay copy cold, warm, and as a late join, two relay hops away, and across a route
//! switch mid-read. A warm copy also answers from what it holds exactly when the case says it
//! may. Every group arrives whole: a switch stalls the first route partway through, so the second
//! supplies the groups the first never delivered and the tail of one it delivered only the head of.

use crate::support;

use std::time::Duration;

use moq_net::track::{Delay, Info, Order, Range, Start, Subscription};
use moq_net::{Timescale, Timestamp, Version, cache, origin, track};
use serde_json::Value;
use support::harness::{MockConnectOptions, MockPair, connect_mock};

/// How long a reader goes without a group before it has read everything it will.
const QUIET: Duration = Duration::from_secs(1);

/// How long a reader waits for each group it still wants.
const ARRIVAL: Duration = Duration::from_secs(20);

/// The one-way delay of a copy's upstream while it shows what it answers from cache alone:
/// longer than [`QUIET`], shorter than the cache's idle window.
const UPSTREAM: Duration = Duration::from_secs(5);

/// Where a case runs.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
	/// Through the origin's own track.
	Direct,
	/// Through a copy nothing read before.
	Cold,
	/// Through a copy its readers filled from the origin's history.
	Warm,
	/// Through a copy its readers joined before the origin wrote a group, so it received each live.
	Late,
	/// Through a copy holding what its readers filled, one relay hop further, so the copy's
	/// relay serves it over the wire.
	Chain,
	/// Through one copy, then another holding what the copy's readers filled, mid-read.
	Switch,
}

impl Mode {
	fn name(self) -> &'static str {
		match self {
			Mode::Direct => "direct",
			Mode::Cold => "cold",
			Mode::Warm => "warm",
			Mode::Late => "late",
			Mode::Chain => "chain",
			Mode::Switch => "switch",
		}
	}
}

/// A reader's subscription as the table states it.
#[derive(Clone, Debug)]
struct Ask {
	ranges: Vec<Range>,
}

impl Ask {
	fn parse(value: &Value) -> Self {
		let start = |range: &Value| match range.get("start") {
			None => Start::Oldest,
			Some(start) => match start.get("live") {
				Some(live) => Start::Live(Duration::from_millis(live.as_u64().expect("a lag"))),
				None => Start::Time(start.as_u64().expect("a time")),
			},
		};
		let ranges = match value.get("ranges") {
			Some(ranges) => ranges
				.as_array()
				.expect("ranges")
				.iter()
				.map(|range| Range {
					start: start(range),
					end: range.get("end").map(|end| end.as_u64().expect("a time")),
				})
				.collect(),
			None => vec![Range::default()],
		};
		Self { ranges }
	}

	fn subscription(&self) -> Subscription {
		Subscription::default()
			.with_delay(Delay::default().with_max(None))
			.with_order(Order::Ascending)
			.with_ranges(self.ranges.clone())
	}
}

#[derive(Debug)]
struct Case {
	name: String,
	groups: Vec<u64>,
	/// The frames in each group, one per millisecond from its start.
	frames: u64,
	append: bool,
	/// How long after its start each group ends whole, `None` for no declared end.
	span: Option<u64>,
	/// The groups the origin leaves unfinished.
	open: Vec<u64>,
	copy: Vec<Ask>,
	evict: bool,
	read: Ask,
	deliver: Vec<u64>,
	asks: Option<bool>,
	/// The modes this language still gets wrong.
	known: Vec<String>,
}

fn cases() -> Vec<Case> {
	let table: Value = serde_json::from_str(include_str!("../range-table.json")).expect("the range table parses");
	let keys = |value: &Value| -> Vec<u64> {
		value
			.as_array()
			.expect("group starts")
			.iter()
			.map(|key| key.as_u64().expect("a group start"))
			.collect()
	};
	let groups = keys(&table["groups"]);
	let frames = table["frames"].as_u64().expect("frames per group");
	table["cases"]
		.as_array()
		.expect("cases")
		.iter()
		.map(|case| Case {
			name: case["name"].as_str().expect("a name").to_string(),
			groups: case.get("groups").map(keys).unwrap_or_else(|| groups.clone()),
			frames,
			append: case
				.get("append")
				.is_none_or(|append| append.as_bool().expect("append")),
			span: case.get("span").map(|span| span.as_u64().expect("a span")),
			open: case.get("open").map(keys).unwrap_or_default(),
			copy: case
				.get("copy")
				.map(|copy| copy.as_array().expect("copy").iter().map(Ask::parse).collect())
				.unwrap_or_default(),
			evict: case.get("evict").is_some_and(|evict| evict.as_bool().expect("evict")),
			read: Ask::parse(&case["read"]),
			deliver: keys(&case["deliver"]),
			asks: case.get("asks").map(|asks| asks.as_bool().expect("asks")),
			known: case
				.get("known")
				.into_iter()
				.flat_map(|known| known.as_array().expect("known"))
				.filter(|known| known["lang"].as_str().is_none_or(|lang| lang == "rs"))
				.map(|known| known["mode"].as_str().expect("a mode").to_string())
				.collect(),
		})
		.collect()
}

fn produce_origin(hop: u64, pool: Option<cache::Pool>) -> origin::Producer {
	let mut config = origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	if let Some(pool) = pool {
		config.pool = pool;
	}
	config.proxy = true;
	let (producer, driver) = origin::Producer::new(config);
	support::harness::spawn(driver);
	producer
}

/// Have `to` pull what `from` publishes over lite-07, priced at `cost` when given.
async fn link(from: &origin::Producer, to: &origin::Producer, cost: Option<u64>) -> MockPair {
	let mut options = MockConnectOptions::new("moq-lite-07-wip".parse::<Version>().unwrap());
	options.server_publish = Some(from.consume());
	options.client_subscribe = Some(to.clone());
	options.cost = cost;
	connect_mock(options).await
}

/// The origin, holding every group it writes for good, and its empty track.
struct Source {
	origin: origin::Producer,
	track: track::Producer,
	_route: origin::Route,
	// The groups the case leaves unfinished, held so they stay open.
	open: std::sync::Mutex<Vec<moq_net::group::Producer>>,
}

impl Source {
	fn new() -> Self {
		let origin = produce_origin(1, Some(cache::Pool::unbounded()));
		let route = origin.route("room", origin::RouteOptions::hidden()).unwrap();
		let track = route
			.broadcast()
			.create_track(
				"data",
				Info::default().with_timescale(Timescale::MILLI).with_max_age(None),
			)
			.unwrap();
		route.announce().unwrap();
		Self {
			origin,
			track,
			_route: route,
			open: Default::default(),
		}
	}

	fn write(&self, case: &Case) {
		if let Some(live) = self.write_head(case) {
			self.write_tail(case, live);
		}
	}

	/// Write every group but the tail of the last, returning it still open: the live edge
	/// mid-group, which a reader may have only the head of when its route changes.
	fn write_head(&self, case: &Case) -> Option<(u64, moq_net::group::Producer)> {
		let mut last = None;
		for &start in &case.groups {
			if let Some(live) = last.take() {
				self.write_tail(case, live);
			}
			let mut group = match case.append {
				true => self.track.append_group(Timestamp::from_millis(start).unwrap()).unwrap(),
				false => self.track.create_group(moq_net::group::Info { start }).unwrap(),
			};
			for frame in 0..case.frames.div_ceil(2) {
				group
					.write_frame(Timestamp::from_millis(start + frame).unwrap(), &b"x"[..])
					.unwrap();
			}
			last = Some((start, group));
		}
		last
	}

	fn write_tail(&self, case: &Case, (start, mut group): (u64, moq_net::group::Producer)) {
		for frame in case.frames.div_ceil(2)..case.frames {
			group
				.write_frame(Timestamp::from_millis(start + frame).unwrap(), &b"x"[..])
				.unwrap();
		}
		match case.span {
			_ if case.open.contains(&start) => self.open.lock().unwrap().push(group),
			Some(span) => drop(group.finish_at(start + span).unwrap()),
			None => drop(group.finish().unwrap()),
		}
	}
}

/// Let every task settle: time is simulated, so this returns once the runtime is idle.
async fn settle() {
	kio::clock::sleep(Duration::from_millis(500)).await;
}

/// The broadcast as `origin` serves it, once it does.
async fn broadcast(origin: &origin::Producer) -> moq_net::broadcast::Consumer {
	kio::clock::timeout(ARRIVAL, origin.consume().follow("room", None).unwrap().available())
		.await
		.expect("the broadcast resolves")
		.unwrap()
		.unwrap()
}

async fn subscribe(broadcast: &moq_net::broadcast::Consumer, ask: &Ask) -> moq_net::Result<track::Subscriber> {
	kio::clock::timeout(ARRIVAL, broadcast.track("data").unwrap().subscribe(ask.subscription()))
		.await
		.expect("the subscribe answers")
}

/// What a reader got: each group as it arrived, then whether it ended or failed.
#[derive(Default)]
struct Got {
	groups: Vec<moq_net::group::Consumer>,
	ended: bool,
	error: Option<String>,
}

impl Got {
	/// Receive until `want` groups arrived, each within [`ARRIVAL`], then until none arrives
	/// for [`QUIET`].
	async fn read(&mut self, reader: &mut track::Subscriber, want: usize) {
		while !self.ended && self.error.is_none() {
			let wait = match self.groups.len() < want {
				true => ARRIVAL,
				false => QUIET,
			};
			match kio::clock::timeout(wait, reader.recv_group()).await {
				Err(_) => return,
				Ok(Ok(Some(group))) => self.groups.push(group),
				Ok(Ok(None)) => self.ended = true,
				Ok(Err(err)) => self.error = Some(err.to_string()),
			}
		}
	}

	/// Each group's start, as it arrived.
	fn starts(&self) -> Vec<u64> {
		self.groups.iter().map(|group| group.start).collect()
	}

	/// The groups in order, failing a group received twice, one missing any of its
	/// `frames` frames, or an error. A group in `open` never ends, so it is read only
	/// through its frames.
	async fn delivered(&mut self, frames: u64, open: &[u64]) -> Result<Vec<u64>, String> {
		let starts = self.starts();
		if let Some(error) = &self.error {
			return Err(format!("failed after {starts:?}: {error}"));
		}
		for group in &mut self.groups {
			let want: Vec<u64> = (group.start..group.start + frames).collect();
			let mut got = Vec::new();
			while !(open.contains(&group.start) && got.len() == want.len()) {
				match kio::clock::timeout(ARRIVAL, group.read_frame()).await {
					Ok(Ok(Some(frame))) => got.push(frame.timestamp.value()),
					Ok(Ok(None)) => break,
					Ok(Err(err)) => return Err(format!("group {} failed after frames {got:?}: {err}", group.start)),
					Err(_) => return Err(format!("group {} stalled after frames {got:?}", group.start)),
				}
			}
			if got != want {
				return Err(format!("group {} delivered frames {got:?}", group.start));
			}
		}
		let mut sorted = starts.clone();
		sorted.sort_unstable();
		sorted.dedup();
		match sorted.len() == starts.len() {
			true => Ok(sorted),
			false => Err(format!("delivered a group twice: {starts:?}")),
		}
	}
}

/// Subscribe the copy's readers and read each out: what the copy then holds.
async fn warm(broadcast: &moq_net::broadcast::Consumer, case: &Case) -> Vec<track::Subscriber> {
	let mut readers = Vec::new();
	for ask in &case.copy {
		let mut reader = subscribe(broadcast, ask).await.expect("a copy reader subscribes");
		// Each group's frames too, so the copy pulls them whole, as a JavaScript copy always does.
		while let Ok(Ok(Some(mut group))) = kio::clock::timeout(QUIET, reader.recv_group()).await {
			while let Ok(Ok(Some(_))) = kio::clock::timeout(QUIET, group.read_frame()).await {}
		}
		readers.push(reader);
	}
	readers
}

/// Run `case` in `mode`: whether each reader ended, or how it went wrong.
async fn run(case: &Case, mode: Mode) -> Result<bool, String> {
	let source = Source::new();
	let want = case.deliver.len();
	let mut got = Got::default();
	// A reader joining once the switch settled, which must get the same.
	let mut after = None;
	// Kept to the end of the run, so the copy's readers stay subscribed.
	let mut _readers = Vec::new();
	let mut _links = Vec::new();
	let mut cached = None;

	match mode {
		Mode::Direct => {
			source.write(case);
			let remote = broadcast(&source.origin).await;
			let mut reader = subscribe(&remote, &case.read).await.map_err(|err| err.to_string())?;
			got.read(&mut reader, want).await;
		}
		Mode::Cold | Mode::Warm | Mode::Late => {
			let copy = produce_origin(2, None);
			let upstream = link(&source.origin, &copy, None).await;
			let remote = broadcast(&copy).await;
			match mode {
				Mode::Late => {
					let mut joined = Vec::new();
					for ask in &case.copy {
						let remote = remote.clone();
						let ask = ask.clone();
						joined.push(kio::clock::sim::spawn(async move { subscribe(&remote, &ask).await }));
					}
					settle().await;
					source.write(case);
					for join in joined {
						let mut reader = join
							.await
							.expect("the subscribe runs")
							.expect("a copy reader subscribes");
						Got::default().read(&mut reader, 0).await;
						_readers.push(reader);
					}
				}
				_ => {
					source.write(case);
					if mode == Mode::Warm {
						_readers = warm(&remote, case).await;
					}
				}
			}
			if case.evict {
				// Past the idle window and the sweep trailing it, so the copy keeps only the live edge.
				kio::clock::sleep(cache::DEFAULT_EXPIRY * 2 + Duration::from_secs(2)).await;
			}

			// Show what a warm copy answers alone: nothing from upstream arrives within QUIET. A
			// late join's copy received every group live from where its readers joined, so it holds
			// more than the case's `asks` assumes.
			let warm = mode == Mode::Warm;
			if warm {
				upstream.client_transport.set_latency(UPSTREAM);
			}
			let mut reader = subscribe(&remote, &case.read).await.map_err(|err| err.to_string())?;
			if warm {
				got.read(&mut reader, 0).await;
				cached = Some((got.groups.len(), got.ended));
				upstream.client_transport.set_latency(Duration::ZERO);
			}
			got.read(&mut reader, want).await;
			_links.push(upstream);
		}
		Mode::Chain => {
			source.write(case);
			let relay = produce_origin(2, None);
			let front = produce_origin(3, None);
			_links.push(link(&source.origin, &relay, None).await);
			_readers = warm(&broadcast(&relay).await, case).await;
			_links.push(link(&relay, &front, None).await);
			let remote = broadcast(&front).await;
			let mut reader = subscribe(&remote, &case.read).await.map_err(|err| err.to_string())?;
			got.read(&mut reader, want).await;
		}
		Mode::Switch => {
			// The live edge stays mid-group until after the switch.
			let live = source.write_head(case);
			let relay_a = produce_origin(2, None);
			let relay_b = produce_origin(3, None);
			let front = produce_origin(4, None);
			_links.push(link(&source.origin, &relay_a, None).await);
			_links.push(link(&source.origin, &relay_b, None).await);
			_readers = warm(&broadcast(&relay_b).await, case).await;
			let a_to_front = link(&relay_a, &front, None).await;

			// The first route delivers the oldest half of the range and the head of its newest
			// group, then stalls: one Group Stream each.
			let remote = broadcast(&front).await;
			let a = &a_to_front.server_transport;
			a.hold_unis();
			let mut reader = subscribe(&remote, &case.read).await.map_err(|err| err.to_string())?;
			settle().await;
			let held = a.held_unis();
			// Every group stalled, so the switch leaves the new route what the old never released.
			if held != want {
				return Err(format!("the first route opened {held} Group Streams for {want} groups"));
			}
			let oldest = (want / 2).min(held.saturating_sub(1));
			for _ in 0..oldest {
				a.release_oldest_uni();
			}
			if held > oldest {
				a.release_newest_uni();
			}
			got.read(&mut reader, held.min(oldest + 1)).await;

			// A standby route, priced to lose while the first stands, then the first's session
			// dies and the live group goes on.
			_links.push(link(&relay_b, &front, Some(1_000)).await);
			settle().await;
			a_to_front.server.abort(moq_net::Error::Cancel);
			a_to_front.client.abort(moq_net::Error::Cancel);
			if let Some(live) = live {
				source.write_tail(case, live);
			}
			got.read(&mut reader, want).await;

			let mut late = subscribe(&remote, &case.read).await.map_err(|err| err.to_string())?;
			let mut late_got = Got::default();
			late_got.read(&mut late, want).await;
			after = Some(late_got);
		}
	}

	let delivered = got.delivered(case.frames, &case.open).await?;
	if delivered != case.deliver {
		return Err(format!("delivered {delivered:?}, ended: {}", got.ended));
	}
	let mut ended = got.ended;
	if let Some(mut after) = after {
		ended &= after.ended;
		let delivered = after
			.delivered(case.frames, &case.open)
			.await
			.map_err(|err| format!("after the switch: {err}"))?;
		if delivered != case.deliver {
			return Err(format!("after the switch, delivered {delivered:?}"));
		}
	}
	if let (Some(asks), Some((cached, ended))) = (case.asks, cached) {
		// Nothing to deliver is answered alone only by ending before upstream could answer.
		let answered = cached == want && (want > 0 || ended);
		if answered == asks {
			return Err(match asks {
				true => format!("answered {:?} from the copy alone, which must ask", got.starts()),
				false => format!("asked upstream, answering only {cached} groups from the copy"),
			});
		}
	}
	Ok(ended)
}

/// Run every case the mode applies to, failing with each that diverges. A known divergence is
/// skipped: some reproduce only on some orderings, so one passing proves nothing.
async fn table(mode: Mode) {
	let mut failures = Vec::new();
	for case in cases() {
		let warmed = matches!(mode, Mode::Warm | Mode::Late);
		if (warmed && case.copy.is_empty()) || case.known.iter().any(|known| known == mode.name()) {
			continue;
		}
		match run(&case, mode).await {
			Err(err) => failures.push(format!("{}: {err}", case.name)),
			// A switch never leaves a read open that ends direct from the origin.
			Ok(false) if mode == Mode::Switch && run(&case, Mode::Direct).await == Ok(true) => {
				failures.push(format!("{}: never ended, as it does direct", case.name));
			}
			Ok(_) => {}
		}
	}
	assert!(failures.is_empty(), "{}: {failures:#?}", mode.name());
}

#[kio::test]
async fn direct() {
	table(Mode::Direct).await;
}

#[kio::test]
async fn cold() {
	table(Mode::Cold).await;
}

#[kio::test]
async fn warm_copy() {
	table(Mode::Warm).await;
}

#[kio::test]
async fn late_join() {
	table(Mode::Late).await;
}

#[kio::test]
async fn chain() {
	table(Mode::Chain).await;
}

#[kio::test]
async fn route_switch() {
	table(Mode::Switch).await;
}
