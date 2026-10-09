//! `moq export ts --linger` and `--stitch` over a real relay: the export rides out the same
//! publisher instance leaving and coming back, switches to a replacement only when asked, and
//! exits with the verdict of the broadcast's last end.
#![cfg(unix)]

use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin};
use tokio::task::JoinHandle;

const TIMEOUT: Duration = Duration::from_secs(30);
/// About three seconds of HEVC and Opus.
const CLIP: &[u8] = include_bytes!("../../moq-mux/src/container/ts/test_data/bbb_cbr.ts");
/// Fragmented AV1, which the TS export does not carry.
const AV1: &[u8] = include_bytes!("../../moq-mux/src/container/fmp4/test_data/av1.mp4");

type Output = Arc<Mutex<Vec<u8>>>;

struct Relay {
	url: String,
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
	tokio::spawn(relay.run());
	ready.wait().await.expect("relay ready");
	Relay { url }
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

/// Start `moq export ts --linger <linger>`, collecting its stdout.
fn export(relay: &Relay, linger: &str) -> (Child, Output) {
	let (child, output, _) = export_with(relay, &[], &["--linger", linger]);
	(child, output)
}

/// Start `moq <global> export ts <flags>`, collecting its stdout and stderr.
fn export_with(relay: &Relay, global: &[&str], flags: &[&str]) -> (Child, Output, Output) {
	let mut child = moq(relay, global)
		.args(["export", "ts"])
		.args(flags)
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.expect("spawn export");
	let output = collect(child.stdout.take().expect("stdout"));
	let errors = collect(child.stderr.take().expect("stderr"));
	(child, output, errors)
}

/// Collect everything `pipe` yields.
fn collect(mut pipe: impl tokio::io::AsyncRead + Unpin + Send + 'static) -> Output {
	let output = Output::default();
	let sink = output.clone();
	tokio::spawn(async move {
		let mut buf = vec![0; 64 * 1024];
		while let Ok(n) = pipe.read(&mut buf).await
			&& n > 0
		{
			sink.lock().unwrap().extend_from_slice(&buf[..n]);
		}
	});
	output
}

/// Start `moq import ts` and feed it the clip at about real time, handing stdin back
/// once it is all written. Closing stdin then finishes the broadcast.
fn import(relay: &Relay) -> (Child, JoinHandle<ChildStdin>) {
	import_with(relay, &[])
}

/// [`import`] with `global` flags, such as the session version or the epoch.
fn import_with(relay: &Relay, global: &[&str]) -> (Child, JoinHandle<ChildStdin>) {
	let mut child = moq(relay, global)
		.args(["import", "ts"])
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

/// Wait until the export has written more than `len` bytes.
async fn output_past(output: &Output, len: usize) {
	tokio::time::timeout(TIMEOUT, async {
		while output.lock().unwrap().len() <= len {
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	})
	.await
	.expect("the export wrote nothing");
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
	let (mut export, output) = export(&relay, "1s");

	let (mut publisher, feeding) = import(&relay);
	let stdin = feeding.await.unwrap();
	output_past(&output, 0).await;
	// The linger can't start before stdin closes, but it can before the publisher exits.
	let closed = Instant::now();
	drop(stdin);
	assert!(wait(&mut publisher).await.success());

	let status = wait(&mut export).await;
	assert!(status.success(), "a clean finish exits 0, got {status}");
	assert!(
		closed.elapsed() >= Duration::from_millis(900),
		"the export waited out its linger"
	);
}

#[tokio::test]
async fn a_drop_exits_one_once_the_linger_expires() {
	let relay = relay().await;
	let (mut export, output) = export(&relay, "1s");

	let (publisher, feeding) = import(&relay);
	let stdin = feeding.await.unwrap();
	output_past(&output, 0).await;
	interrupt(publisher, stdin).await;

	let status = wait(&mut export).await;
	assert_eq!(status.code(), Some(1), "a drop exits 1, got {status}");
}

/// An export that fails on its own, with the broadcast still up, has no return to wait for.
#[tokio::test]
async fn an_export_failure_with_the_broadcast_up_exits_without_lingering() {
	let relay = relay().await;
	let (mut export, _output) = export(&relay, "20s");

	// TS cannot carry AV1, so the export fails on the catalog while the publisher stays up.
	let mut publisher = moq(&relay, &["import", "fmp4"])
		.stdin(Stdio::piped())
		.spawn()
		.expect("spawn import");
	let mut stdin = publisher.stdin.take().expect("stdin");
	let published = Instant::now();
	stdin.write_all(AV1).await.expect("write stdin");

	let status = wait(&mut export).await;
	assert_eq!(status.code(), Some(1), "an export failure exits 1, got {status}");
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
async fn interrupted(output: &Output, publisher: (Child, JoinHandle<ChildStdin>)) -> usize {
	let (publisher, feeding) = publisher;
	let stdin = feeding.await.unwrap();
	output_past(output, 0).await;
	interrupt(publisher, stdin).await;
	tokio::time::sleep(Duration::from_secs(1)).await;
	output.lock().unwrap().len().next_multiple_of(188)
}

/// A restarted publisher is another instance, so the export does not splice it in: it exits 1
/// as soon as the replacement shows up, naming the flag that would follow it.
#[tokio::test]
async fn a_restarted_publisher_exits_one_without_stitch() {
	let relay = relay().await;
	let (mut export, output, errors) = export_with(&relay, &[], &["--linger", "20s"]);
	let mark = interrupted(&output, import(&relay)).await;

	let (mut publisher, feeding) = import(&relay);
	let started = Instant::now();
	let status = wait(&mut export).await;
	assert_eq!(status.code(), Some(1), "a replacement exits 1, got {status}");
	assert!(
		started.elapsed() < Duration::from_secs(10),
		"exited after {:?}, as though waiting out the linger",
		started.elapsed()
	);
	let errors = String::from_utf8_lossy(&errors.lock().unwrap()).into_owned();
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
	let (mut export, output, _) = export_with(&relay, &[], &["--linger", "10s", "--stitch"]);
	let mark = interrupted(&output, import(&relay)).await;

	let (mut publisher, feeding) = import(&relay);
	let stdin = feeding.await.unwrap();
	output_past(&output, mark).await;
	drop(stdin);
	assert!(wait(&mut publisher).await.success());

	let status = wait(&mut export).await;
	assert!(status.success(), "the last end was a clean finish, got {status}");

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

/// The same instance (a shared `--epoch` on a version that carries it) back within the linger
/// continues the stream, under the program already announced.
#[tokio::test]
async fn the_same_instance_within_the_linger_continues() {
	let relay = relay().await;
	let epoch = hang::moq_net::Epoch::mint().to_string();
	let lite07 = ["--connect-version", LITE_07];
	let publish = [lite07[0], lite07[1], "--epoch", epoch.as_str()];
	let (mut export, output, _) = export_with(&relay, &lite07, &["--linger", "10s"]);
	let mark = interrupted(&output, import_with(&relay, &publish)).await;

	let (mut publisher, feeding) = import_with(&relay, &publish);
	let stdin = feeding.await.unwrap();
	output_past(&output, mark).await;
	drop(stdin);
	assert!(wait(&mut publisher).await.success());

	let status = wait(&mut export).await;
	assert!(status.success(), "the last end was a clean finish, got {status}");

	let output = output.lock().unwrap();
	let after = &output[mark..];
	assert!(packets(after).count() > 100, "the returned broadcast went out");
	let versions: Vec<u8> = packets(&output).filter_map(|p| pat_version(p)).collect();
	assert!(
		versions.iter().all(|&version| version == 0),
		"the same instance keeps the PSI version: {versions:?}"
	);
}

/// Subscriptions are sticky: a replacement announced while the old publisher stays up leaves
/// the export on the old one until it ends, and only then exits 1.
#[tokio::test]
async fn an_old_publisher_that_stays_up_keeps_the_export() {
	let relay = relay().await;
	let lite07 = ["--connect-version", LITE_07];
	let (mut export, output, errors) = export_with(&relay, &lite07, &["--linger", "10s"]);

	let (mut old, feeding) = import_with(&relay, &lite07);
	output_past(&output, 0).await;
	let (mut new, replacing) = import_with(&relay, &lite07);
	let old_stdin = feeding.await.unwrap();
	let new_stdin = replacing.await.unwrap();
	tokio::time::sleep(Duration::from_secs(1)).await;
	assert!(
		export.try_wait().unwrap().is_none(),
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
	let status = wait(&mut export).await;
	assert_eq!(
		status.code(),
		Some(1),
		"the replacement exits 1 once the old one ends, got {status}"
	);
	let errors = String::from_utf8_lossy(&errors.lock().unwrap()).into_owned();
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
