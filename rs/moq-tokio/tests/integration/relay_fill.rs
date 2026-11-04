//! Rust peers on either side of a JavaScript relay (`test/relay-fill/relay.ts`), over real
//! WebSocket sessions. The relay's copy FETCHes upstream what a downstream reader misses: the
//! history below where its feed started, and the groups it received and then evicted.
//!
//! Each reader has a session and copy of its own in front of the relay, so a group it wants
//! that the relay lacks reaches the relay's copy rather than a Rust copy another reader warmed.
//!
//! Needs Bun, so it runs in the interop workflow (`just test relay-fill`).
use std::{collections::BTreeMap, process::Stdio, time::Duration};

use anyhow::Context;
use moq_net::track::{Delay, Info, Range, Subscription};
use moq_net::{Timescale, Timestamp, Version};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout};

const TIMEOUT: Duration = Duration::from_secs(30);

/// Groups 0 through 4 present one per second, so a budget picks how far back a reader reaches.
const GROUPS: u64 = 5;

/// A group is as old as the start of the one after it, so this reaches groups 3 and up.
const NARROW: Duration = Duration::from_millis(500);

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

	/// A reader behind a downstream session of its own.
	async fn subscribe(&mut self, subscription: Subscription) -> anyhow::Result<moq_net::track::Subscriber> {
		self.command("dial", "dialed").await?;
		let viewer = self.viewers.recv().await.context("no downstream session")?;
		let remote = viewer.consume().follow("history", None)?.available().await?;
		Ok(remote.track("video")?.subscribe(subscription).await?)
	}
}

/// Every group `subscriber` hands out until it goes quiet, with its first frame.
async fn received(subscriber: &mut moq_net::track::Subscriber) -> anyhow::Result<BTreeMap<u64, String>> {
	let mut got = BTreeMap::new();
	while let Ok(group) = kio::clock::timeout(Duration::from_secs(1), subscriber.recv_group()).await {
		let mut group = group?.ok_or_else(|| anyhow::anyhow!("the track ended"))?;
		let frame = group
			.read_frame()
			.await?
			.ok_or_else(|| anyhow::anyhow!("empty group"))?;
		let payload = String::from_utf8(frame.payload.to_vec())?;
		anyhow::ensure!(
			got.insert(group.sequence, payload).is_none(),
			"group {} delivered twice",
			group.sequence
		);
	}
	Ok(got)
}

fn groups(range: std::ops::Range<u64>) -> BTreeMap<u64, String> {
	range.map(|sequence| (sequence, format!("group {sequence}"))).collect()
}

async fn relay(version: Version) -> anyhow::Result<()> {
	let publisher = moq_tokio::origin::spawn();
	let broadcast = publisher.create_broadcast("history")?;
	let track = broadcast.create_track(
		"video",
		Info::default().with_timescale(Timescale::MILLI).with_max_age(None),
	)?;
	for sequence in 0..GROUPS {
		let mut group = track.create_group(moq_net::group::Info { sequence })?;
		group.write_frame(
			Timestamp::from_millis(sequence * 1000)?,
			format!("group {sequence}").into_bytes(),
		)?;
		group.finish()?;
	}
	broadcast.announce(Default::default())?;
	let (upstream, upstream_serve) = listen(version, true, move || publisher.clone()).await?;
	let (sessions, viewers) = tokio::sync::mpsc::unbounded_channel();
	let (downstream, downstream_serve) = listen(version, false, move || {
		let viewer = moq_tokio::origin::spawn();
		let _ = sessions.send(viewer.clone());
		viewer
	})
	.await?;

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

	let expect = async |subscriber: &mut moq_net::track::Subscriber, want: BTreeMap<u64, String>, what: &str| {
		let got = received(subscriber).await?;
		anyhow::ensure!(got == want, "{version} {what}: got {got:?}, wanted {want:?}");
		anyhow::Ok(())
	};

	// The relay's copy starts where the publisher's budget put it: the live edge before lite-06.
	let narrow = Subscription::default()
		.with_delay(Delay::default().with_max(NARROW))
		.with_ranges([Range::oldest()]);
	let mut first = relay.subscribe(narrow).await?;
	let start = match version.alpn() {
		"moq-lite-05" => GROUPS - 1,
		_ => 3,
	};
	expect(&mut first, groups(start..GROUPS), "narrow").await?;

	// Below the copy's start, a reader with no budget fills what it reaches. Before lite-06 it
	// names the floor it reaches for; lite-06 has the publisher re-offer it instead.
	let wide = Subscription::default()
		.with_delay(Delay::default().with_max(None))
		.with_ranges([match version.alpn() {
			"moq-lite-05" => Range::groups(0..),
			_ => Range::oldest(),
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
	if version.alpn() != "moq-lite-05" {
		let forwarded = relay.fetches().await?;
		anyhow::ensure!(
			forwarded == 0,
			"{version}: the relay forwarded {forwarded} FETCHes past its copy"
		);
	}

	drop((first, below, inside));
	drop(relay);
	anyhow::ensure!(child.wait().await?.success(), "the relay failed on {version}");
	drop((track, broadcast));
	upstream_serve.abort();
	downstream_serve.abort();
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
