//! `moq export ts --linger` over a real relay: the export rides out a publisher that
//! leaves and comes back, and exits with the verdict of the broadcast's last end.
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

type Output = Arc<Mutex<Vec<u8>>>;

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

fn moq(relay: &Relay, args: &[&str]) -> tokio::process::Command {
	let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_moq"));
	// A `MOQ_*` variable in the developer's shell must not reconfigure the child.
	for (name, _) in std::env::vars_os() {
		if name.to_string_lossy().starts_with("MOQ_") {
			command.env_remove(name);
		}
	}
	command
		.args([
			"--connect",
			&relay.url,
			"--connect-tls-fingerprint",
			&relay.fingerprint,
			"--broadcast",
			"demo",
		])
		.args(args)
		.kill_on_drop(true);
	command
}

/// Start `moq export ts --linger <linger>`, collecting its stdout.
fn export(relay: &Relay, linger: &str) -> (Child, Output) {
	let mut child = moq(relay, &["export", "ts", "--linger", linger])
		.stdout(Stdio::piped())
		.spawn()
		.expect("spawn export");
	let mut stdout = child.stdout.take().expect("stdout");
	let output = Output::default();
	let sink = output.clone();
	tokio::spawn(async move {
		let mut buf = vec![0; 64 * 1024];
		while let Ok(n) = stdout.read(&mut buf).await
			&& n > 0
		{
			sink.lock().unwrap().extend_from_slice(&buf[..n]);
		}
	});
	(child, output)
}

/// Start `moq import ts` and feed it the clip at about real time, handing stdin back
/// once it is all written. Closing stdin then finishes the broadcast.
fn import(relay: &Relay) -> (Child, JoinHandle<ChildStdin>) {
	let mut child = moq(relay, &["import", "ts"])
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
	drop(stdin);
	assert!(wait(&mut publisher).await.success());
	let finished = Instant::now();

	let status = wait(&mut export).await;
	assert!(status.success(), "a clean finish exits 0, got {status}");
	assert!(
		finished.elapsed() >= Duration::from_millis(900),
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

#[tokio::test]
async fn a_publisher_restarted_within_the_linger_resumes_the_output() {
	let relay = relay().await;
	let (mut export, output) = export(&relay, "10s");

	let (publisher, feeding) = import(&relay);
	let stdin = feeding.await.unwrap();
	output_past(&output, 0).await;
	interrupt(publisher, stdin).await;
	// Let the paced tail of the first broadcast drain before marking where it ended.
	tokio::time::sleep(Duration::from_secs(1)).await;
	let mark = output.lock().unwrap().len().next_multiple_of(188);

	let (mut publisher, feeding) = import(&relay);
	let stdin = feeding.await.unwrap();
	output_past(&output, mark).await;
	drop(stdin);
	assert!(wait(&mut publisher).await.success());

	let status = wait(&mut export).await;
	assert!(status.success(), "the last end was a clean finish, got {status}");

	let output = output.lock().unwrap();
	let after = &output[mark..];
	let total = packets(after).count();
	assert!(total > 100, "the returned broadcast went out: {total} packets");
	assert!(packets(after).any(|p| pid(p) == 0), "PAT re-emitted after the restart");
	// The PCR the returned broadcast opens with flags the break.
	let first_pcr = packets(after)
		.find_map(|p| adaptation_flags(p).filter(|flags| flags & 0x10 != 0))
		.expect("a PCR after the restart");
	assert_ne!(first_pcr & 0x80, 0, "the restart is flagged as a break");
}
