//! `moq import --report` over a real relay, read back through the `export report` sink:
//! each report counts what the import carried.
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
const CLIP: &[u8] = include_bytes!("../../../moq-mux/src/container/ts/test_data/bbb5s_cbr.ts");
/// Bytes fed every [`PACE`], which keeps [`CLIP`] near real time.
const CHUNK: usize = 188 * 40;
const PACE: Duration = Duration::from_millis(150);
/// Past the two seconds of PCR a live TS catalog waits for, so the broadcast is up by then.
const HALF: usize = CLIP.len() / 2;

/// Feed `clip` to `write` at about real time, never catching up in a burst.
async fn feed(clip: &[u8], mut write: impl AsyncFnMut(&[u8])) {
	let mut pace = kio::clock::interval(PACE);
	pace.set_missed_tick_behavior(kio::clock::MissedTickBehavior::Delay);
	for chunk in clip.chunks(CHUNK) {
		pace.tick().await;
		write(chunk).await;
	}
}

struct Relay {
	url: String,
	fingerprint: String,
}

async fn relay() -> Relay {
	let _ = moq_tokio::crypto::install_default();
	let fixture = moq_relay::test_relay().await.expect("test relay");
	let ready = fixture.relay.ready();
	tokio::spawn(fixture.relay.run());
	ready.wait().await.expect("relay ready");
	Relay {
		url: fixture.url.to_string(),
		fingerprint: fixture.fingerprint,
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
	let line = kio::clock::timeout(TIMEOUT, lines.next_line())
		.await
		.expect("the sink printed nothing")
		.expect("read the sink");
	line.map(|line| serde_json::from_str(&line).unwrap_or_else(|err| panic!("`{line}` is not a report: {err}")))
}

async fn wait(child: &mut Child) -> std::process::ExitStatus {
	kio::clock::timeout(TIMEOUT, child.wait())
		.await
		.expect("moq never exited")
		.expect("wait for moq")
}

/// What a TS import of `clip` writes to each rendition, and its MPEG-TS counters, from an
/// in-process import.
fn written(clip: &[u8]) -> (BTreeMap<String, hang::stats::Rendition>, ts::stats::Snapshot) {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let config =
		moq_mux::catalog::Config::default().with_catalog(moq_mux::catalog::hang::Catalog::<ts::Ext>::default());
	let catalog = moq_mux::catalog::Producer::new(&mut broadcast, config).unwrap();
	let mut import = ts::Import::new(broadcast, catalog.reserve());
	import.decode(clip).unwrap();
	import.finish().unwrap();
	(catalog.stats(), import.stats())
}

/// Each elementary stream's access units, by PID.
fn units(mpegts: &ts::stats::Snapshot) -> BTreeMap<u16, u64> {
	mpegts.streams.iter().map(|(pid, row)| (*pid, row.units)).collect()
}

/// Every TR 101 290 counter, PSI CRC error, and per-PID loss reads zero.
fn assert_clean(mpegts: &ts::stats::Snapshot) {
	let mut stream = mpegts.clone();
	stream.streams.clear();
	assert_eq!(
		stream,
		ts::stats::Snapshot::default(),
		"a stream-wide error was counted"
	);
	for (pid, row) in &mpegts.streams {
		let lost = [
			row.resyncs,
			row.discarded,
			row.unconfirmed,
			row.damaged,
			row.continuity_count_error,
			row.transport_error,
			row.pts_error,
		];
		assert_eq!(lost, [0; 7], "PID {pid} counted a loss: {row:?}");
	}
}

/// The PCR a packet's adaptation field carries, in 27 MHz ticks.
fn pcr(pkt: &[u8; 188]) -> Option<u64> {
	if pkt[3] & 0x20 == 0 || pkt[4] < 7 || pkt[5] & 0x10 == 0 {
		return None;
	}
	let base = (u64::from(pkt[6]) << 25)
		| (u64::from(pkt[7]) << 17)
		| (u64::from(pkt[8]) << 9)
		| (u64::from(pkt[9]) << 1)
		| (u64::from(pkt[10]) >> 7);
	Some(base * 300 + ((u64::from(pkt[10] & 0x01) << 8) | u64::from(pkt[11])))
}

/// `clip` with the PES on `pid` suppressed from its first PES start at or after `from` seconds
/// of program clock to its first at or after `to`, as an encoder whose one input died behind a
/// running mux emits it: a packet that carried the PCR keeps it, every other becomes null
/// stuffing, and the counters after the gap are renumbered so continuity stays legal. The
/// moq-mux import tests' stimulus.
fn suppress(clip: &[u8], pid: u16, from: f64, to: f64) -> Vec<u8> {
	let mut null = [0xff; 188];
	null[..4].copy_from_slice(&[0x47, 0x1f, 0xff, 0x10]);

	let mut out = Vec::with_capacity(clip.len());
	let (mut first, mut now) = (None, 0.0);
	let (mut active, mut done) = (false, false);
	let (mut last_cc, mut dropped) = (0, 0u8);
	for pkt in clip.as_chunks::<188>().0 {
		let mut pkt = *pkt;
		if let Some(pcr) = pcr(&pkt) {
			now = (pcr - *first.get_or_insert(pcr)) as f64 / 27e6;
		}
		if (u16::from(pkt[1] & 0x1f) << 8 | u16::from(pkt[2])) == pid {
			if pkt[1] & 0x40 != 0 {
				if !active && !done && now >= from {
					active = true;
				} else if active && now >= to {
					(active, done) = (false, true);
				}
			}
			let payload = pkt[3] & 0x10 != 0;
			if active {
				dropped = (dropped + u8::from(payload)) & 0x0f;
				pkt = match pcr(&pkt) {
					Some(_) => {
						let mut clock = [0xff; 188];
						clock[..6].copy_from_slice(&[0x47, pkt[1] & 0x1f, pkt[2], 0x20 | last_cc, 183, 0x10]);
						clock[6..12].copy_from_slice(&pkt[6..12]);
						clock
					}
					None => null,
				};
			} else {
				pkt[3] = (pkt[3] & 0xf0) | (pkt[3].wrapping_sub(dropped) & 0x0f);
				if payload {
					last_cc = pkt[3] & 0x0f;
				}
			}
		}
		out.extend_from_slice(&pkt);
	}
	assert!(done, "the clip resumes the PID before it ends");
	out
}

/// Each PID's access units in `first` fell short of `last`'s: the rows advance while it runs.
/// An SI row counts only the sections the program clock could not place, so it has none.
fn assert_advancing(first: &ts::stats::Snapshot, last: &ts::stats::Snapshot) {
	assert!(!last.streams.is_empty(), "no elementary stream reported");
	let first = units(first);
	for (pid, units) in units(last) {
		if last.streams[&pid].track == ".si" {
			continue;
		}
		let before = first.get(&pid).copied().unwrap_or(0);
		assert!(before < units, "PID {pid}: {before} access units, then {units}");
	}
}

/// The reports an import names in its catalog count each rendition and elementary stream while
/// it runs, and its last report holds what the import wrote in full, with every TR 101 290
/// counter at zero on a clean clip.
#[tokio::test]
async fn import_reports_count_what_each_rendition_wrote() {
	let relay = relay().await;
	let (expected, health) = written(CLIP);
	assert_eq!(expected.len(), 2, "the clip carries video and audio: {expected:?}");
	assert_clean(&health);

	let mut import = moq(&relay, "demo", &["import", "--report", "--linger", "2s", "ts"])
		.stdin(Stdio::piped())
		.spawn()
		.expect("spawn the import");
	let mut stdin = import.stdin.take().expect("stdin");
	let (mut sink, mut lines) = sink(&relay, "demo", "report");

	// Half the clip, then a report before the rest: the import reports while it runs, and a
	// stalled input keeps reporting until the sink has joined.
	feed(&CLIP[..HALF], async |chunk| {
		stdin.write_all(chunk).await.expect("write stdin")
	})
	.await;
	let mut reports: Vec<hang::stats::Snapshot<ts::stats::Report>> =
		vec![next(&mut lines).await.expect("a report mid-import")];
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

	let mpegts = last.ext.mpegts.as_ref().expect("a TS import reports its counters");
	assert_clean(mpegts);
	assert_eq!(units(mpegts), units(&health), "the last report holds every access unit");
	let first = reports.iter().find_map(|report| report.ext.mpegts.as_ref()).unwrap();
	assert_advancing(first, mpegts);
	assert!(
		reports.iter().all(|report| report.ext.release.is_none()),
		"an import has no release clock"
	);
}

/// A video PID whose PES stops while the mux keeps its PCR and continuity legal: the import's
/// reports show that row's `quiet` growing while every other row keeps delivering, the row
/// counting again once it returns, and no TR 101 290 priority 1 check firing.
#[tokio::test]
async fn import_reports_a_silent_video_pid() {
	let relay = relay().await;
	let (_, clean) = written(CLIP);
	let pid = |class| {
		let mut rows = clean.streams.iter().filter(|(_, row)| row.class == class);
		*rows.next().expect("the clip carries one").0
	};
	let (video, audio) = (pid(ts::stats::Class::Video), pid(ts::stats::Class::Audio));
	let stimulus = suppress(CLIP, video, 2.5, 4.5);
	let (_, health) = written(&stimulus);

	let mut import = moq(&relay, "demo", &["import", "--report", "--linger", "2s", "ts"])
		.stdin(Stdio::piped())
		.spawn()
		.expect("spawn the import");
	let mut stdin = import.stdin.take().expect("stdin");
	let (mut sink, mut lines) = sink(&relay, "demo", "report");

	// Up to the silence, then a report, so the sink is reading before the video stops.
	feed(&stimulus[..HALF], async |chunk| {
		stdin.write_all(chunk).await.expect("write stdin")
	})
	.await;
	let mut reports: Vec<hang::stats::Snapshot<ts::stats::Report>> =
		vec![next(&mut lines).await.expect("a report mid-import")];
	feed(&stimulus[HALF..], async |chunk| {
		stdin.write_all(chunk).await.expect("write stdin")
	})
	.await;
	drop(stdin);
	while let Some(report) = next(&mut lines).await {
		reports.push(report);
	}
	assert!(wait(&mut import).await.success());
	assert!(wait(&mut sink).await.success());

	let rows: Vec<&ts::stats::Snapshot> = reports.iter().filter_map(|report| report.ext.mpegts.as_ref()).collect();
	let quiet = |mpegts: &ts::stats::Snapshot, pid: u16| mpegts.streams[&pid].quiet.unwrap_or_default();
	let silent = rows.iter().map(|mpegts| quiet(mpegts, video)).max().unwrap();
	// Two seconds of silence, sampled about once a second.
	assert!(
		silent >= Duration::from_millis(500),
		"the video row was quiet for {silent:?} at most"
	);
	let busy = rows.iter().map(|mpegts| quiet(mpegts, audio)).max().unwrap();
	assert!(
		busy < Duration::from_millis(500),
		"the audio row was quiet for {busy:?}"
	);

	let last = rows.last().unwrap();
	assert_eq!(units(last), units(&health), "the last report holds every access unit");
	assert!(quiet(last, video) < silent, "the video row delivered again");
	let priority1 = [
		last.ts_sync_loss,
		last.sync_byte_error,
		last.pat_error,
		last.continuity_count_error,
		last.pmt_error,
	];
	assert_eq!(priority1, [0; 5], "the silence is legal on the wire");
}

/// A sink that joins once the import has finished, while it lingers, still reads its final
/// counts.
#[tokio::test]
async fn a_late_sink_reads_the_final_report() {
	let relay = relay().await;
	let (expected, health) = written(CLIP);

	let mut import = moq(&relay, "demo", &["import", "--report", "--linger", "10s", "ts"])
		// Readiness is an INFO event, independent of the invoking shell's filter.
		.env("RUST_LOG", "info")
		.stdin(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.expect("spawn the import");
	let mut stdin = import.stdin.take().expect("stdin");
	stdin.write_all(CLIP).await.expect("write stdin");
	drop(stdin);
	let mut stderr = BufReader::new(import.stderr.take().expect("stderr")).lines();
	let ended = kio::clock::timeout(TIMEOUT, async {
		while let Some(line) = stderr.next_line().await.expect("read the import's stderr") {
			if line.contains("input ended") {
				return;
			}
		}
		panic!("the import exited before its input ended");
	});
	ended.await.expect("the import never reached the end of its input");

	let (mut sink, mut lines) = sink(&relay, "demo", "report");
	let report: hang::stats::Snapshot<ts::stats::Report> = next(&mut lines).await.expect("the final report");
	assert_eq!(report.renditions, expected);
	assert_eq!(report.ext.mpegts.as_ref().map(units), Some(units(&health)));
	sink.kill().await.unwrap();
	import.kill().await.unwrap();
}
