//! qlog capture from the worker: a real handshake writes a trace the qlog
//! tooling can read.
//!
//! The point of the test is the two halves the relay depends on: the sink
//! actually produces a file for the connection (per connection on noq and
//! quiche, per endpoint on quinn-proto, which takes one sink per config), and
//! every record in it is JSON, so a reader is not handed a truncated or
//! interleaved trace.
//!
//! Kernel-gated: skips loudly below the Linux 6.12 floor (GitHub-hosted CI),
//! and runs everywhere else.

#![cfg(all(
	target_os = "linux",
	feature = "qlog",
	any(feature = "noq", feature = "quiche", feature = "quinn")
))]

#[path = "support/quiche.rs"]
mod support;

use std::net::UdpSocket;

use moq_uring::{Config, Error, Worker, quic, udp};

fn worker() -> Option<Worker> {
	match Worker::new(Config::default()) {
		Ok(worker) => Some(worker),
		Err(Error::Unsupported(reason)) => {
			eprintln!("skipping io_uring qlog test: {reason}");
			None
		}
		Err(err) => panic!("worker setup failed: {err}"),
	}
}

const ALPN: &str = "moq-uring-test";

/// Every record a trace holds, as parsed JSON. JSON-SEQ separates records with
/// a newline, and some encoders lead each one with a record separator.
fn records(path: &std::path::Path) -> Vec<serde_json::Value> {
	let raw = std::fs::read_to_string(path).expect("read trace");
	raw.split('\n')
		.map(|line| line.trim_matches(|c: char| c == '\u{1e}' || c.is_whitespace()))
		.filter(|line| !line.is_empty())
		.map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{}: {err}: {line}", path.display())))
		.collect()
}

/// A handshake with a sink configured leaves a parseable trace behind.
#[test]
fn a_connection_writes_a_trace() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");
	let dir = tempfile::tempdir().expect("temp dir");

	let sink = quic::qlog::Sink::directory(dir.path()).expect("qlog sink");

	let mut server = quic::server::Config::new(quic::Identity::open(&certs.cert, &certs.key).expect("identity"));
	server.alpn = vec![ALPN.to_string()];
	server.transport.qlog = Some(sink.clone());

	let server_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("server socket");
	let server_addr = server_sock.local_addr().expect("server addr");
	let client_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("client socket");

	let mut dial = quic::client::Config::new(server_addr, "localhost");
	dial.alpn = vec![ALPN.to_string()];
	dial.verify = false;
	dial.transport.qlog = Some(sink.clone());

	let accepting = handle.clone();
	handle.spawn(async move {
		let conn = quic::server::accept(&accepting, server_sock, &server)
			.await
			.expect("quic accept");
		// Hold the connection open until the worker is dropped, so the trace
		// covers a live connection rather than a torn-down one.
		std::future::pending::<()>().await;
		drop(conn);
	});

	worker
		.block_on(async move {
			let conn = quic::client::connect(&handle, client_sock, &dial)
				.await
				.expect("quic connect");
			assert_eq!(
				web_transport_trait::poll::Session::protocol(&conn),
				Some(ALPN),
				"negotiated ALPN"
			);
		})
		.expect("worker");

	// Every trace's tail reaches the disk when the last sink handle goes.
	drop(worker);
	drop(sink);

	let mut traces: Vec<_> = std::fs::read_dir(dir.path())
		.expect("read dir")
		.map(|entry| entry.expect("dir entry").path())
		.filter(|path| path.extension().is_some_and(|ext| ext == "qlog"))
		.collect();
	traces.sort();
	assert!(
		!traces.is_empty(),
		"no qlog traces were written to {}",
		dir.path().display()
	);

	let mut sides = std::collections::BTreeSet::new();
	for trace in &traces {
		let name = trace.file_name().expect("file name").to_string_lossy().into_owned();
		let events = records(trace);
		assert!(!events.is_empty(), "{name} holds no records");
		// The first record is the trace header, which is what names the file
		// as qlog rather than arbitrary JSON. Which key carries the schema
		// depends on the encoder the backend links (`qlog` writes
		// `qlog_format`, `n0-qlog` the newer `file_schema`), so only the
		// trace itself is asserted by name.
		assert!(
			events[0].get("trace").is_some(),
			"{name} does not open with a qlog header: {}",
			events[0]
		);
		sides.insert(match name.contains("-client.qlog") {
			true => "client",
			false => "server",
		});
	}
	assert_eq!(
		sides,
		["client", "server"].into_iter().collect(),
		"both ends should have written a trace: {traces:?}"
	);
}

/// A sink pointed at a directory that does not exist fails where it is
/// configured, rather than silently writing nothing for the whole run.
#[test]
fn a_missing_directory_is_refused() {
	let dir = tempfile::tempdir().expect("temp dir");
	let err = quic::qlog::Sink::directory(dir.path().join("nope")).expect_err("missing directory");
	assert!(matches!(err, quic::Error::Qlog(_)), "{err}");
}
