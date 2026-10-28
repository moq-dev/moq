//! Integration test: a caller outside any tokio runtime, such as a C binding, runs
//! sessions on the fallback runtime moq-tokio starts on first use.
//!
//! Every future here is polled by `futures::executor::block_on`, which enters no
//! runtime, so each poll that needs tokio has to find the fallback itself.
//!
//! Unix with the default transports, since it reads one broadcast over each of them.
#![cfg(all(unix, feature = "quic", feature = "websocket", feature = "uds"))]

use futures::executor::block_on;
use moq_tokio::moq_net;

/// Serve one broadcast over every default transport and read it back over each.
#[test]
fn sessions_without_a_runtime() {
	assert!(tokio::runtime::Handle::try_current().is_err());

	let publisher = moq_tokio::origin::spawn();
	let broadcast = publisher.create_broadcast("test").expect("failed to create broadcast");
	broadcast.announce(Default::default()).expect("failed to announce");
	let track = broadcast
		.create_track(
			"video",
			moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI),
		)
		.expect("failed to create track");
	let mut group = track.append_group().expect("failed to append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("failed to write frame");
	group.finish().expect("failed to finish group");

	let dir = tempfile::tempdir().unwrap();
	let socket = dir.path().join("moq.sock");
	let mut server = moq_tokio::server::Config::default();
	server.listen.bind = Some("[::]:0".parse().unwrap());
	server.listen.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
	server.listen.unix.bind = Some(socket.clone());
	server.listen.tls.generate = vec!["localhost".into()];
	let websocket = block_on(moq_tokio::websocket::Listener::bind("127.0.0.1:0".parse().unwrap()))
		.expect("failed to bind websocket");
	let websocket_port = websocket.local_addr().unwrap().port();
	server.websocket = Some(websocket);
	let mut listener = block_on(server.init().expect("failed to init server").listen()).expect("failed to listen");
	let quic_port = listener.local_addr().expect("no quic listener").port();
	let tcp_port = listener.tcp_local_addr().expect("no tcp listener").port();

	let urls = [
		format!("moqt://localhost:{quic_port}"),
		format!("https://localhost:{quic_port}"),
		format!("ws://127.0.0.1:{websocket_port}"),
		format!("tcp://127.0.0.1:{tcp_port}"),
		format!("unix://{}", socket.display()),
	];
	let sessions = urls.len();

	let serve = std::thread::spawn(move || {
		block_on(async move {
			for _ in 0..sessions {
				let request = listener.accept().await.expect("no incoming connection");
				let session = request.with_publisher(&publisher).ok().await.expect("failed to accept");
				let _ = session.closed().await;
			}
			drop((broadcast, track));
			listener.close().await;
		})
	});

	let mut client = moq_tokio::connect::Config::default();
	client.tls.insecure = Some(true);
	let client = client.init(Default::default()).expect("failed to init client");

	for url in urls {
		let subscriber = moq_tokio::origin::spawn();
		let consumer = subscriber.consume();
		let mut announced = consumer.announced();
		block_on(async {
			let connection = client
				.clone()
				.with_subscriber(subscriber)
				.with_reconnect(false)
				.connect(url.parse::<url::Url>().unwrap())
				.established()
				.await
				.unwrap_or_else(|err| panic!("failed to connect to {url}: {err}"));

			announced.next().await.expect("origin closed");
			let broadcast = consumer
				.request_broadcast("test", None)
				.await
				.expect("announced broadcast resolves");
			let mut track = broadcast
				.track("video")
				.unwrap()
				.subscribe(None)
				.await
				.expect("failed to subscribe");
			let mut group = track
				.recv_group()
				.await
				.expect("recv_group failed")
				.expect("track closed");
			let frame = group
				.read_frame()
				.await
				.expect("read_frame failed")
				.expect("group closed");
			assert_eq!(&frame.payload[..], b"hello", "{url}");

			connection.close().await.expect("failed to close");
		});
	}

	block_on(client.close());
	serve.join().expect("server thread panicked");
}
