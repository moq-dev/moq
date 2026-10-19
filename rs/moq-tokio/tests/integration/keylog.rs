//! TLS session keys reach `SSLKEYLOGFILE` only when `quic.keylog` asks for them.
//!
//! Each test points the variable at its own file. nextest runs every test in its
//! own process, so no other test reads the environment while one writes it.

/// Complete a raw QUIC handshake with the given key logging on each side, and
/// return the key log lines both sides wrote.
async fn handshake(server_keylog: bool, client_keylog: bool) -> Vec<String> {
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("keys.log");
	// SAFETY: set before anything spawns a thread, in a process running this test alone.
	unsafe { std::env::set_var("SSLKEYLOGFILE", &path) };

	let mut server_quic = moq_tokio::quic::Config::default();
	server_quic.keylog = Some(server_keylog);
	let mut client_quic = moq_tokio::quic::Config::default();
	client_quic.keylog = Some(client_keylog);

	let mut server_config = moq_tokio::listen::Config::default();
	server_config.bind = Some("[::]:0".parse().unwrap());
	server_config.tls.generate = vec!["localhost".into()];
	let server = server_config.init(server_quic).expect("failed to init server");
	let mut server = server.listen().await.expect("failed to listen");
	let addr = server.local_addr().expect("failed to get local addr");

	let mut client_config = moq_tokio::connect::Config::default();
	client_config.tls.insecure = Some(true);
	let client = client_config.init(client_quic).expect("failed to init client");

	let origin = moq_tokio::origin::spawn();
	let url: url::Url = format!("moqt://localhost:{}", addr.port()).parse().unwrap();
	let server_origin = origin.clone();
	let accept = async {
		let request = server.accept().await.expect("no incoming connection");
		request.with_publisher(&server_origin).ok().await
	};
	let client = client.with_publisher(&origin);
	let connect = client.with_reconnect(false).connect(url).established();
	let (accepted, connected) = tokio::join!(accept, connect);
	accepted.expect("server handshake failed");
	let _session = connected.expect("client handshake failed");

	match std::fs::read_to_string(&path) {
		Ok(log) => log.lines().map(str::to_owned).collect(),
		Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
		Err(err) => panic!("failed to read the key log: {err}"),
	}
}

#[tokio::test]
async fn keys_are_not_logged_by_default() {
	let lines = handshake(false, false).await;
	assert!(lines.is_empty(), "wrote session keys nobody asked for: {lines:?}");
}

/// One side logging writes each secret once; both sides would write it twice.
#[tokio::test]
async fn server_keylog_logs_only_the_server() {
	let lines = handshake(true, false).await;
	assert!(!lines.is_empty(), "quic.keylog wrote no session keys");
	let unique: std::collections::HashSet<_> = lines.iter().collect();
	assert_eq!(unique.len(), lines.len(), "the client logged keys too: {lines:?}");
}

#[tokio::test]
async fn client_keylog_logs_only_the_client() {
	let lines = handshake(false, true).await;
	assert!(!lines.is_empty(), "quic.keylog wrote no session keys");
	let unique: std::collections::HashSet<_> = lines.iter().collect();
	assert_eq!(unique.len(), lines.len(), "the server logged keys too: {lines:?}");
}
