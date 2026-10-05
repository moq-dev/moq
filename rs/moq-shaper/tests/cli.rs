//! The binary end to end: a profile by name and the
//! counters it reports while it runs and when it stops.
#![cfg(unix)]

use std::{
	io::{BufRead, BufReader},
	net::{SocketAddr, UdpSocket},
	process::{Command, Stdio},
	time::Duration,
};

#[test]
fn a_profile_run_reports_its_counters() {
	let target = UdpSocket::bind("127.0.0.1:0").unwrap();
	let target_addr = target.local_addr().unwrap();

	let report = std::env::temp_dir().join(format!("moq-shaper-report-{}.json", std::process::id()));
	let mut shaper = Command::new(env!("CARGO_BIN_EXE_moq-shaper"))
		.args(["--listen", "127.0.0.1:0", "--target", &target_addr.to_string()])
		.args(["--profile", "mild", "--seed", "7"])
		.args(["--report-interval", "100ms", "--report", report.to_str().unwrap()])
		.stdout(Stdio::piped())
		.spawn()
		.unwrap();
	let mut stdout = BufReader::new(shaper.stdout.take().unwrap());

	// The first line says where it listens, and under what.
	let mut line = String::new();
	stdout.read_line(&mut line).unwrap();
	assert!(line.ends_with("seed 7, profile mild\n"), "{line}");
	let addr: SocketAddr = line.split_whitespace().nth(1).unwrap().parse().unwrap();

	let client = UdpSocket::bind("127.0.0.1:0").unwrap();
	for id in 0..50u32 {
		client.send_to(&id.to_be_bytes(), addr).unwrap();
	}
	target.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
	let mut buf = [0u8; 4];
	for _ in 0..50 {
		target.recv_from(&mut buf).expect("a datagram never arrived");
	}

	// A line each interval, once the traffic has been counted.
	let interim = loop {
		line.clear();
		stdout.read_line(&mut line).unwrap();
		let json: serde_json::Value = serde_json::from_str(&line).unwrap();
		if json["up"]["packets"] == 50 {
			break json;
		}
	};
	assert_eq!(interim["profile"], "mild");

	let status = Command::new("kill")
		.args(["-TERM", &shaper.id().to_string()])
		.status()
		.unwrap();
	assert!(status.success());
	assert!(shaper.wait().unwrap().success(), "the profile never acted");

	let written = std::fs::read(&report).expect("no report at exit");
	std::fs::remove_file(&report).unwrap();
	let json: serde_json::Value = serde_json::from_slice(&written).unwrap();
	assert_eq!(json["profile"], "mild");
	assert_eq!(json["seed"], 7);
	assert_eq!(json["up"]["packets"], 50);
	assert_eq!(json["up"]["delayed"], 50);
	assert_eq!(json["up"]["lost"], 0);
}

#[test]
fn a_zero_report_interval_is_refused_before_binding() {
	let result = Command::new(env!("CARGO_BIN_EXE_moq-shaper"))
		.args([
			"--listen",
			"127.0.0.1:0",
			"--target",
			"127.0.0.1:9",
			"--report-interval",
			"0s",
		])
		.output()
		.unwrap();
	assert!(!result.status.success());
	assert!(result.stdout.is_empty(), "bound before refusing the interval");
	assert!(String::from_utf8_lossy(&result.stderr).contains("report interval must be positive"));
}
