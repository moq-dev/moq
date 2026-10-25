//! Native TLS session keys go only to the explicitly configured destination.

/// Complete a raw QUIC handshake with the given key logging on each side, and
/// return the key log lines both sides wrote.
async fn handshake(server_keylog: bool, client_keylog: bool) -> Vec<String> {
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("keys.log");
	let mut server_quic = moq_tokio::quic::Config::default();
	server_quic.keylog = server_keylog.then(|| path.clone());
	let mut client_quic = moq_tokio::quic::Config::default();
	client_quic.keylog = client_keylog.then(|| path.clone());

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

/// A path that cannot be opened fails construction, before a peer can dial.
#[tokio::test]
async fn keylog_open_failure_stops_initialization() {
	let dir = tempfile::tempdir().unwrap();
	let mut quic = moq_tokio::quic::Config::default();
	quic.keylog = Some(dir.path().join("missing/keys.log"));
	let result = moq_tokio::connect::Config::default().init(quic);
	assert!(result.is_err(), "an unreadable log must fail initialization");
}

#[cfg(unix)]
#[test]
fn keylog_files_are_private_and_existing_permissions_are_not_changed() {
	use std::os::unix::fs::PermissionsExt;
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("keys.log");
	let _log = moq_tokio::tls::KeyLog::open(&path).unwrap();
	assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
	std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
	assert!(matches!(
		moq_tokio::tls::KeyLog::open(&path),
		Err(moq_tokio::tls::Error::KeyLog(_))
	));
	assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o644);
}

/// Independently configured endpoints can append to the same destination without splitting lines.
#[test]
fn independent_keylog_writers_append_whole_records() {
	use moq_tokio::rustls::KeyLog as _;
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("keys.log");
	let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
	std::thread::scope(|scope| {
		for writer in 0..8 {
			let log = moq_tokio::tls::KeyLog::open(&path).unwrap();
			let barrier = barrier.clone();
			scope.spawn(move || {
				barrier.wait();
				for record in 0..32 {
					log.log("CLIENT_TRAFFIC_SECRET_0", &[writer; 32], &[record; 48]);
				}
			});
		}
	});
	let actual: std::collections::BTreeSet<_> = std::fs::read_to_string(&path)
		.unwrap()
		.lines()
		.map(str::to_owned)
		.collect();
	let expected: std::collections::BTreeSet<_> = (0..8)
		.flat_map(|writer| {
			(0..32).map(move |record| {
				format!(
					"CLIENT_TRAFFIC_SECRET_0 {} {}",
					hex::encode([writer; 32]),
					hex::encode([record; 48])
				)
			})
		})
		.collect();
	assert_eq!(
		actual.len(),
		expected.len(),
		"independent append handles must preserve every record"
	);
	assert!(
		expected.is_subset(&actual),
		"missing record: {:?}",
		expected.difference(&actual).next()
	);
}
