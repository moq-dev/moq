//! Listening imports and exports drain accepted sessions on SIGTERM.
#![cfg(unix)]

use std::{process::Stdio, time::Duration};

use hang::moq_net;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Child;

const TIMEOUT: Duration = Duration::from_secs(10);

struct Listening {
	child: Child,
	// Child::wait closes its own stdin; keep this writer open to expose blocked reads.
	_stdin: tokio::process::ChildStdin,
	url: url::Url,
	errors: tokio::task::JoinHandle<String>,
	_directory: tempfile::TempDir,
}

async fn listen(stage: &str, flags: &[&str]) -> Listening {
	let directory = tempfile::tempdir().unwrap();
	let socket = directory.path().join("moq.sock");
	let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_moq"));
	for (name, _) in std::env::vars_os() {
		if name.to_string_lossy().starts_with("MOQ_") {
			command.env_remove(name);
		}
	}
	let mut child = command
		.args([
			"--listen-unix-bind",
			socket.to_str().unwrap(),
			"--auth-public",
			"**",
			"--broadcast",
			"demo.hang",
		])
		.args(flags)
		.args([stage, "ts"])
		.env("RUST_LOG", "info")
		.stdin(Stdio::piped())
		.stdout(Stdio::null())
		.stderr(Stdio::piped())
		.kill_on_drop(true)
		.spawn()
		.unwrap();
	let stdin = child.stdin.take().unwrap();
	let mut stderr = BufReader::new(child.stderr.take().unwrap());
	kio::clock::timeout(TIMEOUT, async {
		let mut output = String::new();
		loop {
			let mut line = String::new();
			assert_ne!(
				stderr.read_line(&mut line).await.unwrap(),
				0,
				"moq stopped before listening: {output}"
			);
			output.push_str(&line);
			if line.contains("listening (unix)") {
				break;
			}
		}
	})
	.await
	.expect("listener never bound");
	let errors = tokio::spawn(async move {
		let mut output = String::new();
		stderr.read_to_string(&mut output).await.unwrap();
		output
	});
	Listening {
		child,
		_stdin: stdin,
		url: format!("unix://{}", socket.display()).parse().unwrap(),
		errors,
		_directory: directory,
	}
}

impl Listening {
	fn signal(&self) {
		let pid = self.child.id().unwrap() as libc::pid_t;
		// SAFETY: this is our live child and kill has no memory-safety preconditions.
		assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
	}

	async fn stopped(mut self) {
		let status = kio::clock::timeout(TIMEOUT, self.child.wait())
			.await
			.expect("listening CLI did not stop after its sessions left")
			.unwrap();
		let errors = self.errors.await.unwrap();
		assert!(status.success(), "CLI exited with {status}: {errors}");
	}
}

async fn connects(listening: &Listening, stage: &str) -> (moq_tokio::Connection, moq_net::origin::Consumer) {
	let _ = moq_tokio::crypto::install_default();
	let origin = moq_tokio::origin::spawn();
	let consumer = origin.consume().with_hidden(true);
	let client = moq_tokio::connect::Config::default()
		.init(Default::default())
		.unwrap()
		.with_reconnect(false);
	let client = if stage == "import" {
		client.with_subscriber(origin)
	} else {
		client.with_publisher(origin)
	};
	let connection = kio::clock::timeout(TIMEOUT, client.connect(listening.url.clone()).established())
		.await
		.unwrap()
		.unwrap();
	(connection, consumer)
}

async fn drains(stage: &str) {
	let listening = listen(stage, &[]).await;
	let (connection, _origin) = connects(&listening, stage).await;
	let draining = connection.draining().unwrap();
	listening.signal();
	let goaway = kio::clock::timeout(TIMEOUT, moq_net::kio::wait(|waiter| draining.poll(waiter)))
		.await
		.expect("no GOAWAY after SIGTERM")
		.expect("CLI closed the session without GOAWAY");
	assert_eq!(goaway.uri(), "");
	connection.closed().await.unwrap();
	listening.stopped().await;
}

#[tokio::test]
async fn listening_import_drains_viewers_on_sigterm() {
	drains("import").await;
}

#[tokio::test]
async fn listening_export_drains_publishers_on_sigterm() {
	drains("export").await;
}

/// A publisher leaving on GOAWAY can finish the export stage while another
/// accepted peer is still draining; the stage must not cut that peer off.
#[tokio::test]
async fn an_export_stage_ending_during_drain_waits_for_every_session() {
	let window = Duration::from_secs(3);
	let listening = listen("export", &["--drain-timeout", "3s"]).await;
	let _ = moq_tokio::crypto::install_default();
	let origin = moq_tokio::origin::spawn();
	let source = origin.create_broadcast("demo.hang").unwrap();
	let catalog = source.create_track("catalog.json", None).unwrap();
	source.announce(Default::default()).unwrap();
	let publisher = moq_tokio::connect::Config::default()
		.init(Default::default())
		.unwrap()
		.with_reconnect(false)
		.with_publisher(origin)
		.connect(listening.url.clone())
		.established()
		.await
		.unwrap();
	let mut demand = catalog.demand();
	let wanted = async {
		while demand.wanted_changed().await?.is_none() {}
		moq_net::Result::Ok(())
	};
	kio::clock::timeout(TIMEOUT, wanted).await.unwrap().unwrap();
	let mut legacy = moq_tokio::connect::Config::default();
	legacy.version = vec!["moq-lite-03".parse().unwrap()];
	let legacy = legacy
		.init(Default::default())
		.unwrap()
		.with_reconnect(false)
		.connect(listening.url.clone())
		.established()
		.await
		.unwrap();
	let draining = publisher.draining().unwrap();
	let signalled = kio::clock::now();
	listening.signal();
	kio::clock::timeout(TIMEOUT, moq_net::kio::wait(|waiter| draining.poll(waiter)))
		.await
		.unwrap()
		.unwrap();
	publisher.closed().await.unwrap();
	kio::clock::timeout(TIMEOUT, legacy.closed())
		.await
		.unwrap()
		.expect_err("legacy peer was force-closed");
	listening.stopped().await;
	assert!(signalled.elapsed() >= window, "stage completion cut the drain short");
}
