//! `moq-bench host` runs beside a relay, in an environment full of the relay's settings.

/// The relay's `MOQ_*` variables, and generator ones the sampler never reads, must not stop it.
#[cfg(target_os = "linux")]
#[test]
fn host_ignores_the_environment() {
	let dir = tempfile::tempdir().unwrap();
	let output = dir.path().join("host.jsonl");
	let status = std::process::Command::new(env!("CARGO_BIN_EXE_moq-bench"))
		// The guard on host timers is for tests, not the binaries they run.
		.env_remove("KIO_DENY_HOST_TIMERS")
		.args(["host", "--pid", &std::process::id().to_string(), "--duration", "10ms"])
		.arg("--output")
		.arg(&output)
		.env("MOQ_AUTH_KEY", "relay.jwk")
		.env("MOQ_BENCH_FPS", "invalid")
		.env("MOQ_CLIENT_CONNECT", "https://relay.example")
		.status()
		.unwrap();
	assert!(status.success());
	assert!(!std::fs::read_to_string(&output).unwrap().is_empty());
}
