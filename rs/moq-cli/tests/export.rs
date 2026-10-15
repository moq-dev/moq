//! `moq export ts --linger` and `--stitch` over a real relay: the export rides out the same
//! publisher instance leaving and coming back, switches to a replacement only when asked, and
//! exits with the verdict of the broadcast's last end.
#![cfg(unix)]

use std::io::Cursor;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hang::moq_net;
use moq_mux::container::ts;
use mpeg2ts::ts::{ReadTsPacket, TsPacketReader, TsPayload};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin};
use tokio::task::JoinHandle;

const TIMEOUT: Duration = Duration::from_secs(30);
/// About three seconds of HEVC and Opus.
const CLIP: &[u8] = include_bytes!("../../moq-mux/src/container/ts/test_data/bbb_cbr.ts");
/// Fragmented AV1, which the TS export does not carry.
const AV1: &[u8] = include_bytes!("../../moq-mux/src/container/fmp4/test_data/av1.mp4");

type Output = Arc<Mutex<Vec<u8>>>;

struct Export {
	child: Child,
	output: Output,
	stdout: JoinHandle<std::io::Result<()>>,
	stderr: JoinHandle<String>,
}

impl Export {
	/// Wait for output, retaining the child verdict and stderr on every timeout.
	async fn output_past(&mut self, len: usize) {
		let arrived = tokio::time::timeout(TIMEOUT, async {
			while self.output.lock().unwrap().len() <= len {
				tokio::time::sleep(Duration::from_millis(50)).await;
			}
		})
		.await;
		if let Err(err) = arrived {
			let status = self.child.try_wait().expect("inspect export exit");
			let _ = self.child.kill().await;
			let errors = (&mut self.stderr).await.expect("collect exporter stderr");
			panic!("the export wrote nothing after {len} bytes: {err}; status {status:?}; exporter stderr: {errors}");
		}
	}

	/// Wait for the verdict and drain both pipes before inspecting their contents.
	async fn wait(mut self) -> (std::process::ExitStatus, String) {
		let status = match tokio::time::timeout(TIMEOUT, self.child.wait()).await {
			Ok(status) => status.expect("wait for export"),
			Err(err) => {
				let _ = self.child.kill().await;
				let errors = (&mut self.stderr).await.expect("collect exporter stderr");
				panic!("export never exited: {err}; exporter stderr: {errors}");
			}
		};
		self.stdout
			.await
			.expect("stdout reader panicked")
			.expect("read export stdout");
		let errors = self.stderr.await.expect("collect exporter stderr");
		(status, errors)
	}
}

struct Relay {
	url: String,
	origin: moq_net::origin::Consumer,
}

/// A public relay on ephemeral loopback ports that also accepts lite-07, the version that
/// carries epochs, for a client that opts in with `--connect-version`. Its generated
/// certificate is trusted blindly, since nothing but loopback reaches it.
async fn relay() -> Relay {
	let _ = moq_tokio::crypto::install_default();
	let mut config = moq_relay::Config::default();
	config.drain_timeout = Duration::ZERO;
	config.listen.bind = Some("127.0.0.1:0".parse().unwrap());
	config.listen.tls.generate = vec!["localhost".into()];
	config.listen.version = std::iter::once(LITE_07.parse().unwrap())
		.chain(hang::moq_net::Versions::all().iter().copied())
		.collect();
	config.auth.public = vec![moq_auth::Pattern::all()];
	let relay = moq_relay::Relay::load(config).await.expect("test relay");
	let url = format!("https://{}/", relay.quic_addr().expect("QUIC address"));
	let ready = relay.ready();
	let origin = relay.cluster().origin.consume();
	tokio::spawn(relay.run());
	ready.wait().await.expect("relay ready");
	Relay { url, origin }
}

const LITE_07: &str = "moq-lite-07-wip";

fn moq(relay: &Relay, args: &[&str]) -> tokio::process::Command {
	let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_moq"));
	// A `MOQ_*` variable in the developer's shell must not reconfigure the child.
	for (name, _) in std::env::vars_os() {
		if name.to_string_lossy().starts_with("MOQ_") {
			command.env_remove(name);
		}
	}
	command
		.args(["--connect", &relay.url, "--connect-tls-insecure", "--broadcast", "demo"])
		.args(args)
		.kill_on_drop(true);
	command
}

/// Start `moq export ts --linger <linger>`, collecting its output and diagnostics.
fn export(relay: &Relay, linger: &str) -> Export {
	export_with(relay, &[], &["--linger", linger])
}

/// Start `moq <global> export ts <flags>`, collecting both pipes until they close.
fn export_with(relay: &Relay, global: &[&str], flags: &[&str]) -> Export {
	let mut child = moq(relay, global)
		.args(["export", "ts"])
		.args(flags)
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.expect("spawn export");
	let (output, stdout) = collect(child.stdout.take().expect("stdout"));
	let mut stderr = child.stderr.take().expect("stderr");
	let stderr = tokio::spawn(async move {
		let mut errors = Vec::new();
		stderr.read_to_end(&mut errors).await.expect("read export stderr");
		String::from_utf8_lossy(&errors).into_owned()
	});
	Export {
		child,
		output,
		stdout,
		stderr,
	}
}

/// Collect everything `pipe` yields, retaining its completion before final assertions.
fn collect(mut pipe: impl tokio::io::AsyncRead + Unpin + Send + 'static) -> (Output, JoinHandle<std::io::Result<()>>) {
	let output = Output::default();
	let sink = output.clone();
	let task = tokio::spawn(async move {
		let mut buf = vec![0; 64 * 1024];
		loop {
			let n = pipe.read(&mut buf).await?;
			if n == 0 {
				return Ok(());
			}
			sink.lock().unwrap().extend_from_slice(&buf[..n]);
		}
	});
	(output, task)
}

/// Start `moq import ts` and feed it the clip at about real time, handing stdin back
/// once it is all written. Closing stdin then finishes the broadcast.
fn import(relay: &Relay) -> (Child, JoinHandle<ChildStdin>) {
	import_with(relay, &[])
}

/// [`import`] with `global` flags, such as the session version or the epoch.
fn import_with(relay: &Relay, global: &[&str]) -> (Child, JoinHandle<ChildStdin>) {
	let mut child = moq(relay, global)
		// A finished import lingers for its max-age; keep that short so the publisher exits promptly.
		.args(["import", "--max-age", "1s", "ts"])
		.stdin(Stdio::piped())
		.spawn()
		.expect("spawn import");
	let mut stdin = child.stdin.take().expect("stdin");
	let feeding = tokio::spawn(async move {
		for chunk in CLIP.chunks(188 * 40) {
			stdin.write_all(chunk).await.expect("write stdin");
			tokio::time::sleep(Duration::from_millis(150)).await;
		}
		stdin
	});
	(child, feeding)
}

/// End the publisher the way an operator does, which drops the broadcast unfinished.
async fn interrupt(mut child: Child, stdin: ChildStdin) {
	let pid = child.id().expect("running").to_string();
	let status = std::process::Command::new("kill")
		.args(["-INT", &pid])
		.status()
		.expect("kill");
	assert!(status.success());
	// The interrupted process exits once its stdin read returns; closing stdin any
	// sooner could land first and finish the broadcast instead.
	tokio::time::sleep(Duration::from_millis(500)).await;
	drop(stdin);
	wait(&mut child).await;
}

async fn wait(child: &mut Child) -> std::process::ExitStatus {
	tokio::time::timeout(TIMEOUT, child.wait())
		.await
		.expect("moq never exited")
		.expect("wait for moq")
}

/// The 188-byte packets of `ts`, checking they are aligned.
fn packets(ts: &[u8]) -> impl Iterator<Item = &[u8; 188]> {
	ts.as_chunks::<188>()
		.0
		.iter()
		.inspect(|packet| assert_eq!(packet[0], 0x47, "the output is packet aligned"))
}

fn pid(packet: &[u8]) -> u16 {
	u16::from(packet[1] & 0x1f) << 8 | u16::from(packet[2])
}

/// The adaptation field flags byte, when the packet carries a non-empty adaptation field.
fn adaptation_flags(packet: &[u8]) -> Option<u8> {
	(packet[3] & 0x20 != 0 && packet[4] > 0).then_some(packet[5])
}

#[tokio::test]
async fn a_clean_finish_exits_zero_once_the_linger_expires() {
	let relay = relay().await;
	let mut export = export(&relay, "1s");

	let (mut publisher, feeding) = import(&relay);
	let stdin = feeding.await.unwrap();
	export.output_past(0).await;
	// The linger can't start before stdin closes, but it can before the publisher exits.
	let closed = Instant::now();
	drop(stdin);
	assert!(wait(&mut publisher).await.success());

	let (status, errors) = export.wait().await;
	assert!(
		status.success(),
		"a clean finish exits 0, got {status}; exporter stderr: {errors}"
	);
	assert!(
		closed.elapsed() >= Duration::from_millis(900),
		"the export waited out its linger"
	);
}

#[tokio::test]
async fn a_drop_exits_one_once_the_linger_expires() {
	let relay = relay().await;
	let mut export = export(&relay, "1s");

	let (publisher, feeding) = import(&relay);
	let stdin = feeding.await.unwrap();
	export.output_past(0).await;
	interrupt(publisher, stdin).await;

	let (status, errors) = export.wait().await;
	assert_eq!(
		status.code(),
		Some(1),
		"a drop exits 1, got {status}; exporter stderr: {errors}"
	);
}

/// An export that fails on its own, with the broadcast still up, has no return to wait for.
#[tokio::test]
async fn an_export_failure_with_the_broadcast_up_exits_without_lingering() {
	let relay = relay().await;
	let export = export(&relay, "20s");

	// TS cannot carry AV1, so the export fails on the catalog while the publisher stays up.
	let mut publisher = moq(&relay, &["import", "--max-age", "1s", "fmp4"])
		.stdin(Stdio::piped())
		.spawn()
		.expect("spawn import");
	let mut stdin = publisher.stdin.take().expect("stdin");
	let published = Instant::now();
	stdin.write_all(AV1).await.expect("write stdin");

	let (status, errors) = export.wait().await;
	assert_eq!(
		status.code(),
		Some(1),
		"an export failure exits 1, got {status}; exporter stderr: {errors}"
	);
	assert!(
		published.elapsed() < Duration::from_secs(10),
		"exited after {:?}, as though the broadcast had ended",
		published.elapsed()
	);

	drop(stdin);
	wait(&mut publisher).await;
}

/// The `version_number` of a packet starting a PAT section.
fn pat_version(packet: &[u8]) -> Option<u8> {
	if pid(packet) != 0 || packet[1] & 0x40 == 0 {
		return None;
	}
	let start = 4 + adaptation_flags(packet).map_or(0, |_| 1 + usize::from(packet[4]));
	let section = start + 1 + usize::from(packet[start]);
	Some((packet[section + 5] >> 1) & 0x1f)
}

/// Each PID whose first packet in `ts` does not flag a discontinuity.
fn unflagged(ts: &[u8]) -> Vec<u16> {
	let mut seen = std::collections::HashSet::new();
	packets(ts)
		.filter(|packet| pid(*packet) != 0x1fff && seen.insert(pid(*packet)))
		.filter(|packet| adaptation_flags(*packet).is_none_or(|flags| flags & 0x80 == 0))
		.map(|packet| pid(packet))
		.collect()
}

/// Run `publisher` until it was interrupted, then mark the end of its output once the paced
/// tail has drained.
async fn interrupted(export: &mut Export, publisher: (Child, JoinHandle<ChildStdin>)) -> usize {
	let (publisher, feeding) = publisher;
	let stdin = feeding.await.unwrap();
	export.output_past(0).await;
	interrupt(publisher, stdin).await;
	tokio::time::sleep(Duration::from_secs(1)).await;
	export.output.lock().unwrap().len().next_multiple_of(188)
}

/// A restarted publisher is another instance, so the export does not splice it in: it exits 1
/// as soon as the replacement shows up, naming the flag that would follow it.
#[tokio::test]
async fn a_restarted_publisher_exits_one_without_stitch() {
	let relay = relay().await;
	let mut export = export_with(&relay, &[], &["--linger", "20s"]);
	let output = export.output.clone();
	let mark = interrupted(&mut export, import(&relay)).await;

	let (mut publisher, feeding) = import(&relay);
	let started = Instant::now();
	let (status, errors) = export.wait().await;
	assert_eq!(
		status.code(),
		Some(1),
		"a replacement exits 1, got {status}; exporter stderr: {errors}"
	);
	assert!(
		started.elapsed() < Duration::from_secs(10),
		"exited after {:?}, as though waiting out the linger",
		started.elapsed()
	);
	assert!(errors.contains("--stitch"), "the error names --stitch: {errors}");
	assert_eq!(
		output.lock().unwrap().len(),
		mark,
		"nothing of the replacement went out"
	);

	drop(feeding.await.unwrap());
	wait(&mut publisher).await;
}

/// With `--stitch`, a restarted publisher is a full program switch: a new PAT version, and
/// every PID's first packet flagging the break.
#[tokio::test]
async fn a_restarted_publisher_is_a_program_switch_with_stitch() {
	let relay = relay().await;
	let mut export = export_with(&relay, &[], &["--linger", "10s", "--stitch"]);
	let output = export.output.clone();
	let mark = interrupted(&mut export, import(&relay)).await;

	let (mut publisher, feeding) = import(&relay);
	let stdin = feeding.await.unwrap();
	export.output_past(mark).await;
	drop(stdin);
	assert!(wait(&mut publisher).await.success());

	let (status, errors) = export.wait().await;
	assert!(
		status.success(),
		"the last end was a clean finish, got {status}; exporter stderr: {errors}"
	);

	let output = output.lock().unwrap();
	let (before, after) = output.split_at(mark);
	let total = packets(after).count();
	assert!(total > 100, "the replacement went out: {total} packets");
	assert!(
		packets(before)
			.filter_map(|p| pat_version(p))
			.all(|version| version == 0)
	);
	let versions: Vec<u8> = packets(after).filter_map(|p| pat_version(p)).collect();
	assert!(
		!versions.is_empty() && versions.iter().all(|&version| version == 1),
		"the switch advances the PAT version: {versions:?}"
	);
	assert_eq!(unflagged(after), Vec::<u16>::new(), "every PID flags the break");
}

/// One publisher reconnects within the linger, continuing its capture under the same epoch
/// and program before delivering a clean final finish.
#[tokio::test]
async fn the_same_instance_within_the_linger_continues() {
	let relay = relay().await;
	let epoch = moq_net::Epoch::mint();
	let lite07 = ["--connect-version", LITE_07];
	let origin = moq_tokio::origin::spawn();
	let mut broadcast = origin.create_broadcast("demo").unwrap();
	let config = moq_mux::catalog::Config::default()
		.with_max_age(Some(Duration::from_secs(1)))
		.with_catalog(moq_mux::catalog::hang::Catalog::<ts::Ext>::default());
	let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, config).unwrap();
	let mut import = ts::Import::new(broadcast.clone(), catalog.reserve());
	broadcast
		.announce(moq_net::origin::Route::default().with_epoch(epoch.clone()))
		.unwrap();
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	config.version = vec![LITE_07.parse().unwrap()];
	let client = config
		.init(Default::default())
		.unwrap()
		.with_publisher(origin.consume())
		.with_reconnect(false);
	let url: url::Url = relay.url.parse().unwrap();
	let connection = client.connect(url.clone()).established().await.unwrap();
	let mut export = export_with(&relay, &lite07, &["--linger", "10s"]);
	let output = export.output.clone();

	// Continue one capture across a transport loss. Replaying its beginning in a fresh
	// importer would change the catalog clock and reuse groups for already sent content.
	// The TS catalog waits for its PCR-rate measurement; a fixed midpoint can still
	// be behind that gate. Publish to the measured rate, leaving the new tail unread.
	let mut split = 0;
	for chunk in CLIP.chunks(188 * 40) {
		feed(&mut import, chunk).await;
		split += chunk.len();
		if catalog.snapshot().ext.mpegts.mux_rate.is_some() {
			break;
		}
	}
	assert!(split < CLIP.len(), "the capture has a tail after its PCR-rate window");
	export.output_past(0).await;
	let old = relay.origin.request_broadcast("demo", None).await.unwrap();
	assert_eq!(old.info().epoch.as_ref(), Some(&epoch));
	let clock = catalog.snapshot().clock;
	connection.abort(moq_net::Error::Cancel);
	connection.closed().await.unwrap();
	tokio::time::timeout(TIMEOUT, old.closed())
		.await
		.expect("the relay observes the lost publication");
	let mark = output.lock().unwrap().len().next_multiple_of(188);
	let connection = client.connect(url).established().await.unwrap();
	let back = tokio::time::timeout(TIMEOUT, relay.origin.routed_broadcast("demo"))
		.await
		.expect("the relay observes the returning publication")
		.unwrap();
	assert_eq!(back.info().epoch.as_ref(), Some(&epoch));
	assert!(!back.is_clone(&old), "the old relay copy was replaced");
	feed(&mut import, &CLIP[split..]).await;
	assert_eq!(catalog.snapshot().clock, clock, "the capture keeps its catalog clock");
	import.finish().unwrap();
	catalog.finish().unwrap();
	export.output_past(mark).await;
	let (status, errors) = export.wait().await;
	assert!(
		status.success(),
		"the last end was a clean finish, got {status}; exporter stderr: {errors}"
	);
	connection.close().await.unwrap();

	let output = output.lock().unwrap();
	let after = &output[mark..];
	// The rate-measurement prefix leaves less than a second of this capture. Its
	// packet count depends on the buffered prefix; only new capture timestamps
	// prove the return continued instead of replaying already served content.
	assert!(
		latest_pts(&output) > latest_pts(&CLIP[..split]),
		"the returned output carries the capture's new tail: {} packets after {mark} bytes, capture split {split}/{}; exporter stderr: {errors}",
		packets(after).count(),
		CLIP.len()
	);
	let versions: Vec<u8> = packets(&output).filter_map(|p| pat_version(p)).collect();
	assert!(
		versions.iter().all(|&version| version == 0),
		"the same instance keeps the PSI version: {versions:?}"
	);
}

/// Feed the capture at the same pace as the stdin importer fixture.
async fn feed(import: &mut ts::Import<ts::Ext>, data: &[u8]) {
	for chunk in data.chunks(188 * 40) {
		import.decode(chunk).unwrap();
		tokio::time::sleep(Duration::from_millis(150)).await;
	}
}

/// The latest PES timestamp, so replayed content cannot masquerade as a continuation.
fn latest_pts(ts: &[u8]) -> u64 {
	let mut reader = TsPacketReader::new(Cursor::new(ts));
	let mut latest = None;
	while let Some(packet) = reader.read_ts_packet().unwrap() {
		if let Some(TsPayload::PesStart(pes)) = packet.payload
			&& let Some(pts) = pes.header.pts
		{
			latest = Some(latest.map_or(pts.as_u64(), |old: u64| old.max(pts.as_u64())));
		}
	}
	latest.expect("the capture carries PES timestamps")
}

/// Subscriptions are sticky: a replacement announced while the old publisher stays up leaves
/// the export on the old one until it ends, and only then exits 1.
#[tokio::test]
async fn an_old_publisher_that_stays_up_keeps_the_export() {
	let relay = relay().await;
	let lite07 = ["--connect-version", LITE_07];
	let mut export = export_with(&relay, &lite07, &["--linger", "10s"]);
	let output = export.output.clone();

	let (mut old, feeding) = import_with(&relay, &lite07);
	export.output_past(0).await;
	let (mut new, replacing) = import_with(&relay, &lite07);
	let old_stdin = feeding.await.unwrap();
	let new_stdin = replacing.await.unwrap();
	tokio::time::sleep(Duration::from_secs(1)).await;
	assert!(
		export.child.try_wait().unwrap().is_none(),
		"the export stays on the old publisher while it is up"
	);
	let written = output.lock().unwrap().len();
	assert!(
		packets(&output.lock().unwrap())
			.filter_map(|p| pat_version(p))
			.all(|v| v == 0)
	);

	drop(old_stdin);
	assert!(wait(&mut old).await.success());
	let (status, errors) = export.wait().await;
	assert_eq!(
		status.code(),
		Some(1),
		"the replacement exits 1 once the old one ends, got {status}; exporter stderr: {errors}"
	);
	assert!(errors.contains("--stitch"), "the error names --stitch: {errors}");
	let output = output.lock().unwrap().clone();
	assert!(
		packets(&output[written.next_multiple_of(188).min(output.len())..])
			.filter_map(|p| pat_version(p))
			.all(|version| version == 0),
		"nothing of the replacement went out"
	);

	drop(new_stdin);
	wait(&mut new).await;
}
