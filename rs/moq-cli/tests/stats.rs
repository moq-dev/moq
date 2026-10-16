//! `moq import --stats` and `moq export --echo` over a real relay, read back through the
//! `export stats` and `export echo` sinks: each report counts what its side carried.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::process::Stdio;
use std::time::Duration;

use hang::moq_net;
use moq_mux::container::ts;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout};

const TIMEOUT: Duration = Duration::from_secs(30);
/// Five seconds of HEVC and Opus at a constant 400 kb/s.
const CLIP: &[u8] = include_bytes!("../../moq-mux/src/container/ts/test_data/bbb5s_cbr.ts");
/// Bytes fed every [`PACE`], which keeps [`CLIP`] near real time.
const CHUNK: usize = 188 * 40;
const PACE: Duration = Duration::from_millis(150);
/// Past the two seconds of PCR a live TS catalog waits for, so the broadcast is up by then.
const HALF: usize = CLIP.len() / 2;

/// Feed `clip` to `write` at about real time.
async fn feed(clip: &[u8], mut write: impl AsyncFnMut(&[u8])) {
	let mut pace = tokio::time::interval(PACE);
	for chunk in clip.chunks(CHUNK) {
		pace.tick().await;
		write(chunk).await;
	}
}

struct Relay {
	url: String,
	fingerprint: String,
	origin: moq_net::origin::Producer,
}

async fn relay() -> Relay {
	let _ = moq_tokio::crypto::install_default();
	let fixture = moq_relay::test_relay().await.expect("test relay");
	let origin = fixture.relay.cluster().origin.clone();
	let ready = fixture.relay.ready();
	tokio::spawn(fixture.relay.run());
	ready.wait().await.expect("relay ready");
	Relay {
		url: fixture.url.to_string(),
		fingerprint: fixture.fingerprint,
		origin,
	}
}

fn moq(relay: &Relay, broadcast: &str, args: &[&str]) -> tokio::process::Command {
	let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_moq"));
	// A `MOQ_*` variable in the developer's shell must not reconfigure the child.
	for (name, _) in std::env::vars_os() {
		if name.to_string_lossy().starts_with("MOQ_") {
			command.env_remove(name);
		}
	}
	command
		.args(["--connect", &relay.url, "--connect-tls-fingerprint", &relay.fingerprint])
		.args(["--broadcast", broadcast])
		.args(args)
		.kill_on_drop(true);
	command
}

/// Start `moq export <sink>` on `broadcast`, reading the JSON lines it prints.
fn sink(relay: &Relay, broadcast: &str, sink: &str) -> (Child, Lines<BufReader<ChildStdout>>) {
	let mut child = moq(relay, broadcast, &["export", sink])
		.stdout(Stdio::piped())
		.spawn()
		.expect("spawn the sink");
	let lines = BufReader::new(child.stdout.take().expect("stdout")).lines();
	(child, lines)
}

/// The next line the sink prints, or `None` once it exits.
async fn next<T: serde::de::DeserializeOwned>(lines: &mut Lines<BufReader<ChildStdout>>) -> Option<T> {
	let line = tokio::time::timeout(TIMEOUT, lines.next_line())
		.await
		.expect("the sink printed nothing")
		.expect("read the sink");
	line.map(|line| serde_json::from_str(&line).unwrap_or_else(|err| panic!("`{line}` is not a report: {err}")))
}

async fn wait(child: &mut Child) -> std::process::ExitStatus {
	tokio::time::timeout(TIMEOUT, child.wait())
		.await
		.expect("moq never exited")
		.expect("wait for moq")
}

/// What a TS import of `clip` writes to each rendition, from an in-process import.
fn written(clip: &[u8]) -> BTreeMap<String, hang::stats::Rendition> {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let config =
		moq_mux::catalog::Config::default().with_catalog(moq_mux::catalog::hang::Catalog::<ts::Ext>::default());
	let catalog = moq_mux::catalog::Producer::new(&mut broadcast, config).unwrap();
	let mut import = ts::Import::new(broadcast, catalog.reserve());
	import.decode(clip).unwrap();
	import.finish().unwrap();
	catalog.stats()
}

/// The stats an import names in its catalog count each rendition while it runs, and its last
/// report holds what the import wrote in full.
#[tokio::test]
async fn import_stats_count_what_each_rendition_wrote() {
	let relay = relay().await;
	let expected = written(CLIP);
	assert_eq!(expected.len(), 2, "the clip carries video and audio: {expected:?}");

	let mut import = moq(&relay, "demo", &["import", "--stats", "--max-age", "2s", "ts"])
		.stdin(Stdio::piped())
		.spawn()
		.expect("spawn the import");
	let mut stdin = import.stdin.take().expect("stdin");
	let (mut sink, mut lines) = sink(&relay, "demo", "stats");

	// Half the clip, then a report before the rest: the import reports while it runs, and a
	// stalled input keeps reporting until the sink has joined.
	feed(&CLIP[..HALF], async |chunk| {
		stdin.write_all(chunk).await.expect("write stdin")
	})
	.await;
	let mut reports: Vec<hang::stats::Snapshot> = vec![next(&mut lines).await.expect("a report mid-import")];
	feed(&CLIP[HALF..], async |chunk| {
		stdin.write_all(chunk).await.expect("write stdin")
	})
	.await;
	drop(stdin);
	while let Some(report) = next(&mut lines).await {
		reports.push(report);
	}
	assert!(wait(&mut import).await.success());
	assert!(wait(&mut sink).await.success(), "the sink ends with the broadcast");

	let last = reports.last().unwrap();
	assert_eq!(last.renditions, expected, "the last report holds everything written");
	let (first, written) = (&reports[0].renditions, &last.renditions);
	assert!(
		!first.is_empty()
			&& first
				.iter()
				.all(|(name, rendition)| rendition.frames < written[name].frames),
		"the first report counts half the clip: {first:?}"
	);
	assert!(
		reports.iter().any(|report| report.transport.rtt.is_some()),
		"the import reports its connection"
	);
}

/// A sink that joins once the import has finished, while it lingers, still reads its final
/// counts.
#[tokio::test]
async fn a_late_sink_reads_the_final_stats() {
	let relay = relay().await;
	let expected = written(CLIP);

	let mut import = moq(&relay, "demo", &["import", "--stats", "--max-age", "10s", "ts"])
		.stdin(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.expect("spawn the import");
	let mut stdin = import.stdin.take().expect("stdin");
	stdin.write_all(CLIP).await.expect("write stdin");
	drop(stdin);
	let mut stderr = BufReader::new(import.stderr.take().expect("stderr")).lines();
	let ended = tokio::time::timeout(TIMEOUT, async {
		while let Some(line) = stderr.next_line().await.expect("read the import's stderr") {
			if line.contains("input ended") {
				return;
			}
		}
		panic!("the import exited before its input ended");
	});
	ended.await.expect("the import never reached the end of its input");

	let (mut sink, mut lines) = sink(&relay, "demo", "stats");
	let report: hang::stats::Snapshot = next(&mut lines).await.expect("the final report");
	assert_eq!(report.renditions, expected);
	sink.kill().await.unwrap();
	import.kill().await.unwrap();
}

/// One line of `export echo`.
#[derive(serde::Deserialize)]
struct Feedback {
	broadcast: String,
	feedback: hang::echo::Snapshot,
}

/// An export asked for feedback publishes it where the catalog solicits it, counting what it
/// reads as it reads it, and `export echo` finds it under that prefix.
#[tokio::test]
async fn export_echo_reports_what_the_export_reads() {
	let relay = relay().await;
	let expected = written(CLIP);

	// A publisher soliciting feedback under `viewers`, a sibling of its broadcast.
	let mut broadcast = relay.origin.create_broadcast("demo").expect("create the broadcast");
	let config = moq_mux::catalog::Config::default()
		.with_max_age(Some(Duration::from_secs(1)))
		.with_catalog(moq_mux::catalog::hang::Catalog::<ts::Ext>::default());
	let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, config).unwrap();
	let mut import = ts::Import::new(broadcast.clone(), catalog.reserve());
	let mut solicit = catalog.modify().unwrap();
	solicit.echo = Some(hang::catalog::Echo::new("viewers".to_string()));
	solicit.commit().unwrap();
	broadcast.announce(moq_net::origin::Route::default()).unwrap();

	let (mut sink, mut lines) = sink(&relay, "viewers", "echo");
	let mut export = moq(&relay, "demo", &["export", "--echo", "alice", "ts"])
		.stdout(Stdio::piped())
		.spawn()
		.expect("spawn the export");
	let mut output = export.stdout.take().expect("stdout");
	let draining = tokio::spawn(async move { tokio::io::copy(&mut output, &mut tokio::io::sink()).await });

	// Half the clip, then feedback before the rest, so the export reports while it reads.
	feed(&CLIP[..HALF], async |chunk| import.decode(chunk).unwrap()).await;
	let mut feedback: Vec<Feedback> = vec![next(&mut lines).await.expect("feedback mid-export")];
	feed(&CLIP[HALF..], async |chunk| import.decode(chunk).unwrap()).await;
	import.finish().unwrap();
	catalog.finish().unwrap();

	let status = loop {
		tokio::select! {
			line = next(&mut lines) => feedback.push(line.expect("the echo sink runs until stopped")),
			status = export.wait() => break status.expect("wait for the export"),
		}
	};
	assert!(status.success(), "the export finishes cleanly, got {status}");
	// The export writes its final counts as it exits.
	while let Ok(Some(line)) = tokio::time::timeout(Duration::from_secs(2), next(&mut lines)).await {
		feedback.push(line);
	}
	draining.await.unwrap().expect("drain the export");
	sink.kill().await.unwrap();
	drop(broadcast);

	assert!(
		feedback.iter().all(|line| line.broadcast == "viewers/alice.echo"),
		"the feedback is published at the solicited path"
	);
	let first = &feedback.first().expect("the sink printed feedback").feedback;
	let last = &feedback.last().unwrap().feedback;
	assert!(!last.renditions.is_empty(), "the export reports what it reads");
	for (name, read) in &last.renditions {
		assert!(
			read.frames > 0 && read.frames <= expected[name].frames,
			"{name}: read {} of {} frames",
			read.frames,
			expected[name].frames
		);
		let newest = read.arrival.as_ref().expect("an arrival").timestamp;
		let earliest = first.renditions.get(name).and_then(|read| read.arrival.as_ref());
		assert!(
			earliest.is_none_or(|earliest| earliest.timestamp < newest),
			"{name}: the newest arrival advances over {} lines: {earliest:?} then {:?}",
			feedback.len(),
			read.arrival
		);
	}
	assert!(
		feedback.iter().any(|line| line.feedback.transport.rtt.is_some()),
		"the export reports its connection"
	);
}
