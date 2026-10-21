//! `moq auth serve` stops on SIGTERM or SIGINT and exits cleanly.
//!
//! The release image runs `moq` as PID 1, where the kernel drops a signal that has
//! no handler, so a server that relies on the default action ignores `podman stop`
//! and pod termination until SIGKILL. Outside PID 1 the same gap shows as the
//! process dying by the signal instead of exiting 0.
#![cfg(unix)]

use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};

const TIMEOUT: Duration = Duration::from_secs(10);

async fn stops_on(signal: libc::c_int) {
	let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_moq"));
	// A `MOQ_*` variable in the developer's shell must not reconfigure the child.
	for (name, _) in std::env::vars_os() {
		if name.to_string_lossy().starts_with("MOQ_") {
			command.env_remove(name);
		}
	}
	let mut child = command
		.args(["auth", "serve", "--listen", "127.0.0.1:0"])
		.env("RUST_LOG", "info")
		.stderr(Stdio::piped())
		.kill_on_drop(true)
		.spawn()
		.expect("spawn moq");

	// Signal only once the handler can be installed: before that, the default action
	// would kill the process whether or not the fix is in.
	let mut lines = BufReader::new(child.stderr.take().expect("stderr")).lines();
	tokio::time::timeout(TIMEOUT, async {
		while let Some(line) = lines.next_line().await.expect("read stderr") {
			if line.contains("auth server listening") {
				return;
			}
		}
		panic!("moq exited before listening");
	})
	.await
	.expect("auth server never listened");

	let pid = child.id().expect("moq still running") as libc::pid_t;
	// SAFETY: `kill` has no memory-safety preconditions; the pid is our live child.
	assert_eq!(unsafe { libc::kill(pid, signal) }, 0, "failed to signal moq");

	let status = tokio::time::timeout(TIMEOUT, child.wait())
		.await
		.expect("moq ignored the signal")
		.expect("wait for moq");
	assert!(status.success(), "moq did not exit cleanly: {status}");
}

#[tokio::test]
async fn auth_serve_stops_on_sigterm() {
	stops_on(libc::SIGTERM).await;
}

#[tokio::test]
async fn auth_serve_stops_on_sigint() {
	stops_on(libc::SIGINT).await;
}
