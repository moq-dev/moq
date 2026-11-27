//! Rust peers on either side of a JavaScript relay (`test/relay-fill/relay.ts`), over real
//! WebSocket sessions. The relay's copy FETCHes upstream what a downstream reader misses: the
//! history below where its feed started, and the groups it received and then evicted. The
//! relay's copy also passes the shared lite-07 range table (`moq-net/tests/range-table.json`).
//!
//! Each reader has a session and copy of its own in front of the relay, so a group it wants
//! that the relay lacks reaches the relay's copy rather than a Rust copy another reader warmed.
//!
//! Needs Bun, so it runs in the interop workflow (`just test relay-fill`).
use std::{collections::BTreeMap, process::Stdio, time::Duration};

use anyhow::Context;
use moq_net::track::{Info, Range, Start, Subscription};
use moq_net::{Timescale, Timestamp, Version};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout};

const TIMEOUT: Duration = Duration::from_secs(30);

/// Groups 0 through 4 present one per second, so a budget picks how far back a reader reaches.
const GROUPS: u64 = 5;

/// A group is as old as the start of the one after it, so this reaches groups 3 and up.
const NARROW: Duration = Duration::from_millis(500);

/// A live join's lag past every group, the largest every implementation carries losslessly.
const REACH: Duration = Duration::from_millis((1 << 53) - 1);

/// A WebSocket listener that accepts the relay's sessions in turn, each with the origin
/// `origin` hands it: served to the relay, or fed from it.
async fn listen(
	version: Version,
	publish: bool,
	mut origin: impl FnMut() -> moq_net::origin::Producer + Send + 'static,
) -> anyhow::Result<(url::Url, tokio::task::JoinHandle<anyhow::Result<()>>)> {
	let ws = moq_tokio::websocket::Listener::bind("127.0.0.1:0".parse()?)
		.await?
		.with_protocols([version.alpn()])?;
	let url = format!("ws://{}", ws.local_addr()?).parse()?;
	let mut config = moq_tokio::server::Config::default();
	config.listen.version = vec![version];
	config.websocket = Some(ws);
	let mut server = config.init_streams()?.listen().await?;
	let serve = tokio::spawn(async move {
		let mut sessions = Vec::new();
		while let Ok(request) = server.accept().await {
			let origin = origin();
			let request = match publish {
				true => request.with_publisher(&origin),
				false => request.with_subscriber(origin),
			};
			sessions.push(tokio::spawn(async move {
				let session = request.ok().await?;
				let _ = session.closed().await;
				anyhow::Ok(())
			}));
		}
		for session in sessions {
			session.await??;
		}
		Ok(())
	});
	Ok((url, serve))
}

/// The relay process, driven by the commands `relay.ts` reads, and the downstream sessions
/// it dials.
struct Relay {
	stdin: ChildStdin,
	stdout: Lines<BufReader<ChildStdout>>,
	viewers: tokio::sync::mpsc::UnboundedReceiver<moq_net::origin::Producer>,
}

impl Relay {
	async fn command(&mut self, command: &str, answer: &str) -> anyhow::Result<()> {
		self.stdin.write_all(format!("{command}\n").as_bytes()).await?;
		let line = self.stdout.next_line().await?;
		anyhow::ensure!(
			line.as_deref() == Some(answer),
			"{command}: the relay answered {line:?}"
		);
		Ok(())
	}

	/// How many downstream FETCHes the relay forwarded upstream, past its copy.
	async fn fetches(&mut self) -> anyhow::Result<u64> {
		self.stdin.write_all(b"fetches\n").await?;
		let line = self.stdout.next_line().await?.context("the relay exited")?;
		let count = line.strip_prefix("fetches ").context("not a FETCH count")?;
		Ok(count.parse()?)
	}

	/// FETCH group `sequence` from behind a downstream session of its own, returning its first
	/// frame.
	async fn fetch(&mut self, sequence: u64) -> anyhow::Result<String> {
		self.command("dial", "dialed").await?;
		let viewer = self.viewers.recv().await.context("no downstream session")?;
		let remote = viewer
			.consume()
			.follow("history", None)?
			.available()
			.await?
			.context("offline")?;
		let mut group = remote.track("video")?.fetch_group(sequence, None).await?;
		let frame = group.read_frame().await?.context("empty group")?;
		Ok(String::from_utf8(frame.payload.to_vec())?)
	}

	/// A reader behind a downstream session of its own.
	async fn subscribe(&mut self, subscription: Subscription) -> anyhow::Result<moq_net::track::Subscriber> {
		self.command("dial", "dialed").await?;
		let viewer = self.viewers.recv().await.context("no downstream session")?;
		let remote = viewer
			.consume()
			.follow("history", None)?
			.available()
			.await?
			.context("offline")?;
		Ok(remote.track("video")?.subscribe(subscription).await?)
	}
}

/// How long a reader must stay quiet once it has every group it wants, for a group past them
/// to show up.
const QUIET: Duration = Duration::from_millis(500);

/// How long a reader waits for each group it still wants, inside the version's [`TIMEOUT`].
const ARRIVAL: Duration = Duration::from_secs(10);

/// The groups `subscriber` hands out, with their first frame: until it has every group in
/// `want`, then until it goes quiet, so a group past them still shows.
async fn received(
	subscriber: &mut moq_net::track::Subscriber,
	want: &BTreeMap<u64, String>,
) -> anyhow::Result<BTreeMap<u64, String>> {
	let mut got = BTreeMap::new();
	loop {
		let wait = match want.keys().all(|sequence| got.contains_key(sequence)) {
			true => QUIET,
			false => ARRIVAL,
		};
		let Ok(group) = kio::clock::timeout(wait, subscriber.recv_group()).await else {
			return Ok(got);
		};
		let mut group = group?.ok_or_else(|| anyhow::anyhow!("the track ended"))?;
		let frame = group
			.read_frame()
			.await?
			.ok_or_else(|| anyhow::anyhow!("empty group"))?;
		let payload = String::from_utf8(frame.payload.to_vec())?;
		anyhow::ensure!(
			got.insert(group.start, payload).is_none(),
			"group {} delivered twice",
			group.start
		);
	}
}

fn groups(range: std::ops::Range<u64>) -> BTreeMap<u64, String> {
	range.map(|sequence| (sequence, format!("group {sequence}"))).collect()
}

/// A listener's task, aborted once dropped, so a failed or timed-out run frees its sockets.
struct Serving(tokio::task::JoinHandle<anyhow::Result<()>>);

impl Drop for Serving {
	fn drop(&mut self) {
		self.0.abort();
	}
}

/// The JavaScript relay between `publisher`, served upstream of it, and the downstream sessions
/// it dials.
struct Running {
	relay: Relay,
	child: tokio::process::Child,
	_serves: [Serving; 2],
}

impl Running {
	async fn start(version: Version, publisher: moq_net::origin::Producer) -> anyhow::Result<Self> {
		let (upstream, upstream_serve) = listen(version, true, move || publisher.clone()).await?;
		let upstream_serve = Serving(upstream_serve);
		let (sessions, viewers) = tokio::sync::mpsc::unbounded_channel();
		let (downstream, downstream_serve) = listen(version, false, move || {
			let viewer = moq_tokio::origin::spawn();
			let _ = sessions.send(viewer.clone());
			viewer
		})
		.await?;
		let downstream_serve = Serving(downstream_serve);

		let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/relay-fill/relay.ts");
		let mut child = tokio::process::Command::new("bun")
			.arg(script)
			.arg(upstream.as_str())
			.arg(downstream.as_str())
			.arg(version.alpn())
			.arg("history")
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			.stderr(Stdio::inherit())
			.kill_on_drop(true)
			.spawn()?;
		let mut relay = Relay {
			stdin: child.stdin.take().unwrap(),
			stdout: BufReader::new(child.stdout.take().unwrap()).lines(),
			viewers,
		};
		let ready = relay.stdout.next_line().await?;
		anyhow::ensure!(ready.as_deref() == Some("ready"), "the relay did not start: {ready:?}");
		Ok(Self {
			relay,
			child,
			_serves: [upstream_serve, downstream_serve],
		})
	}

	/// Close the relay's sessions, then wait for it to exit cleanly. The listeners stop either way.
	async fn stop(self) -> anyhow::Result<()> {
		let Self {
			relay,
			mut child,
			_serves,
		} = self;
		drop(relay);
		anyhow::ensure!(child.wait().await?.success(), "the relay failed");
		Ok(())
	}
}

async fn relay(version: Version) -> anyhow::Result<()> {
	let publisher = moq_tokio::origin::spawn();
	let broadcast = publisher.route("history", moq_net::origin::RouteOptions::hidden())?;
	let track = broadcast
		.broadcast()
		.create_track(
			"video",
			Info::default().with_timescale(Timescale::MILLI).with_max_age(None),
		)
		.expect("a free name");
	for sequence in 0..GROUPS {
		let mut group = track.create_group(moq_net::group::Info { start: sequence })?;
		group.write_frame(
			Timestamp::from_millis(sequence * 1000)?,
			format!("group {sequence}").into_bytes(),
		)?;
		group.finish()?;
	}
	broadcast.announce()?;
	let mut running = Running::start(version, publisher).await?;
	let relay = &mut running.relay;

	let expect = async |subscriber: &mut moq_net::track::Subscriber, want: BTreeMap<u64, String>, what: &str| {
		let got = received(subscriber, &want).await?;
		anyhow::ensure!(got == want, "{version} {what}: got {got:?}, wanted {want:?}");
		anyhow::Ok(())
	};

	// The relay's copy starts where the publisher's budget put it: the live edge before lite-06.
	// A join reaching back past every group takes each still within the budget, as a wire before
	// lite-07 asks with its one max age.
	let narrow = Subscription::default().with_ranges([Range::from(Start::Live {
		min: Some(REACH),
		max: Some(NARROW),
	})]);
	let mut first = relay.subscribe(narrow).await?;
	let start = match version.alpn() {
		"moq-lite-05" => GROUPS - 1,
		_ => 3,
	};
	expect(&mut first, groups(start..GROUPS), "narrow").await?;

	// Below the copy's start, a reader with no budget fills what it reaches. Before lite-06 it
	// names the floor it reaches for; lite-06 has the publisher re-offer it instead.
	let wide = Subscription::default().with_ranges([match version.alpn() {
		"moq-lite-05" => Range::from(Start::Time(0)),
		_ => Range::ALL,
	}]);
	let mut below = relay.subscribe(wide.clone()).await?;
	expect(&mut below, groups(0..GROUPS), "below the start").await?;

	// Inside the copy: the relay ages out every group but the live edge. The same subscription
	// changes nothing upstream, so only a FETCH brings back what the copy lost. Lite-06 FETCH
	// carries no budget, so its copy fills nothing below the start and relies on a re-offer an
	// unchanged demand never asks for.
	relay.command("evict", "evicted").await?;
	let mut inside = relay.subscribe(wide).await?;
	let refilled = match version.alpn() {
		"moq-lite-06" => groups(start..GROUPS),
		_ => groups(0..GROUPS),
	};
	expect(&mut inside, refilled, "evicted by the copy").await?;
	// From lite-06 the relay starts each reader at the lowest group its copy fills, so the
	// history arrives over the subscription, not through a FETCH the relay forwards past its
	// copy. Before lite-06 the relay starts at the live edge, below which the reader's own copy
	// FETCHes.
	// From lite-06 none was forwarded. A FETCH of its own shows the count sees them at all.
	let forwarded = relay.fetches().await?;
	if version.alpn() != "moq-lite-05" {
		anyhow::ensure!(
			forwarded == 0,
			"{version}: the relay forwarded {forwarded} FETCHes past its copy"
		);
	}
	anyhow::ensure!(
		relay.fetch(2).await? == "group 2",
		"{version}: the FETCH read the wrong group"
	);
	let counted = relay.fetches().await?;
	if version.alpn() == "moq-lite-07-wip" {
		anyhow::ensure!(
			counted == 0,
			"{version}: lite-07 must serve fetch via bounded SUBSCRIBE, not {counted} legacy FETCHes"
		);
	} else {
		anyhow::ensure!(
			counted == forwarded + 1,
			"{version}: the relay counted {counted} forwarded FETCHes, not {}",
			forwarded + 1
		);
	}

	drop((first, below, inside));
	running.stop().await.with_context(|| version.to_string())?;
	drop((track, broadcast));
	Ok(())
}

#[tokio::test]
#[ignore = "requires Bun; run by just test relay-fill in the interop workflow"]
async fn relay_fill_javascript() {
	for version in ["moq-lite-05", "moq-lite-06", "moq-lite-07-wip"] {
		kio::clock::timeout(TIMEOUT, relay(version.parse().unwrap()))
			.await
			.unwrap()
			.unwrap();
	}
}

/// One reader of the range table, as `rs/moq-net/tests/range-table.json` states it.
fn table_subscription(ask: &serde_json::Value) -> Subscription {
	let start = |range: &serde_json::Value| match range.get("start") {
		None => Start::Oldest,
		Some(start) => match start.get("live") {
			Some(live) => Start::Live {
				min: Some(Duration::from_millis(live.as_u64().expect("a lag"))),
				max: None,
			},
			None => Start::Time(start.as_u64().expect("a time")),
		},
	};
	let ranges = match ask.get("ranges").and_then(serde_json::Value::as_array) {
		Some(ranges) => ranges
			.iter()
			.map(|range| Range {
				start: start(range),
				end: range.get("end").and_then(serde_json::Value::as_u64),
				order: moq_net::track::Order::Ascending,
				..Default::default()
			})
			.collect(),
		None => vec![Range::ALL.with_order(moq_net::track::Order::Ascending)],
	};
	Subscription::default().with_ranges(ranges)
}

/// The group starts `subscriber` hands out, until it has `want` of them, each within
/// [`ARRIVAL`], then until it ends or goes quiet for [`QUIET`].
async fn table_read(subscriber: &mut moq_net::track::Subscriber, want: usize) -> anyhow::Result<Vec<u64>> {
	let mut got = Vec::new();
	loop {
		let wait = match got.len() < want {
			true => ARRIVAL,
			false => QUIET,
		};
		let Ok(group) = kio::clock::timeout(wait, subscriber.recv_group()).await else {
			return Ok(got);
		};
		match group? {
			Some(group) => got.push(group.start),
			None => return Ok(got),
		}
	}
}

/// Run one case of the range table through the relay's copy: cold, or `warm` with the case's
/// copy readers subscribed first, each behind a downstream session of its own.
async fn table_case(case: &serde_json::Value, groups: &[u64], frames: u64, warm: bool) -> anyhow::Result<Vec<u64>> {
	let publisher = moq_tokio::origin::spawn();
	let broadcast = publisher.route("history", moq_net::origin::RouteOptions::hidden())?;
	let track = broadcast
		.broadcast()
		.create_track(
			"video",
			Info::default().with_timescale(Timescale::MILLI).with_max_age(None),
		)
		.expect("a free name");
	let starts: Vec<u64> = match case.get("groups").and_then(serde_json::Value::as_array) {
		Some(starts) => starts.iter().filter_map(serde_json::Value::as_u64).collect(),
		None => groups.to_vec(),
	};
	let append = case.get("append").and_then(serde_json::Value::as_bool) != Some(false);
	let span = case.get("span").and_then(serde_json::Value::as_u64);
	let open: Vec<u64> = (case
		.get("open")
		.and_then(serde_json::Value::as_array)
		.into_iter()
		.flatten())
	.filter_map(serde_json::Value::as_u64)
	.collect();
	// The groups the case leaves unfinished, held so they stay open.
	let mut unfinished = Vec::new();
	for start in starts {
		let time = Timestamp::from_millis(start)?;
		let mut group = match append {
			true => track.append_group(time)?,
			false => track.create_group(moq_net::group::Info { start })?,
		};
		for frame in 0..frames {
			group.write_frame(Timestamp::from_millis(start + frame)?, b"x".to_vec())?;
		}
		match span {
			_ if open.contains(&start) => unfinished.push(group),
			Some(span) => drop(group.finish_at(start + span)?),
			None => drop(group.finish()?),
		}
	}
	broadcast.announce()?;
	let version = "moq-lite-07-wip".parse().map_err(anyhow::Error::msg)?;
	let mut running = Running::start(version, publisher).await?;

	let mut readers = Vec::new();
	if warm {
		for ask in case
			.get("copy")
			.and_then(serde_json::Value::as_array)
			.into_iter()
			.flatten()
		{
			let mut reader = running.relay.subscribe(table_subscription(ask)).await?;
			table_read(&mut reader, 0).await?;
			readers.push(reader);
		}
	}
	if case.get("evict").and_then(serde_json::Value::as_bool) == Some(true) {
		running.relay.command("evict", "evicted").await?;
	}
	let want = case["deliver"].as_array().context("deliver")?.len();
	let mut reader = running.relay.subscribe(table_subscription(&case["read"])).await?;
	let mut got = table_read(&mut reader, want).await?;
	got.sort_unstable();

	drop((reader, readers));
	running.stop().await?;
	drop((track, broadcast));
	Ok(got)
}

/// The lite-07 range table through a JavaScript relay's copy under a Rust origin, cold and warm,
/// as `moq-net`'s `range_table` and `js/net/src/range-table.test.ts` run it within each language.
#[tokio::test]
#[ignore = "requires Bun; run by just test relay-fill in the interop workflow"]
async fn relay_fill_range_table() {
	let table: serde_json::Value =
		serde_json::from_str(include_str!("../../../moq-net/tests/range-table.json")).expect("the range table parses");
	let frames = table["frames"].as_u64().expect("frames per group");
	let groups: Vec<u64> = table["groups"]
		.as_array()
		.expect("groups")
		.iter()
		.filter_map(serde_json::Value::as_u64)
		.collect();
	let mut failures = Vec::new();
	for case in table["cases"].as_array().expect("cases") {
		let name = case["name"].as_str().expect("a name");
		let want: Vec<u64> = case["deliver"]
			.as_array()
			.expect("deliver")
			.iter()
			.filter_map(serde_json::Value::as_u64)
			.collect();
		for (mode, warm) in [("cold", false), ("warm", true)] {
			if warm && case.get("copy").is_none() {
				continue;
			}
			// The JavaScript copy's known divergences in the bun suite, and those only this
			// relay shows.
			let relayed = format!("relay-{mode}");
			let known = (case
				.get("known")
				.and_then(serde_json::Value::as_array)
				.into_iter()
				.flatten())
			.any(|known| {
				known["mode"] == relayed || (known["mode"] == mode && known.get("lang").is_none_or(|lang| lang == "js"))
			});
			if known {
				continue;
			}
			match kio::clock::timeout(TIMEOUT, table_case(case, &groups, frames, warm)).await {
				Ok(Ok(got)) if got == want => {}
				Ok(Ok(got)) => failures.push(format!("{mode}: {name}: delivered {got:?}")),
				Ok(Err(err)) => failures.push(format!("{mode}: {name}: {err:#}")),
				Err(_) => failures.push(format!("{mode}: {name}: timed out")),
			}
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}
