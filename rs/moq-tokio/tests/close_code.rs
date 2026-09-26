//! A client sees the application close code its server sent, on every transport.
//!
//! Each case runs several fresh connections, since the losses this guards against were
//! races between the peer's close and whatever the transport reported next.

#![cfg(all(feature = "noq", feature = "websocket"))]

use std::time::Duration;

use moq_tokio::moq_net;

const CODE: u16 = 4011;
const RUNS: usize = 5;

#[derive(Clone, Copy, Debug)]
enum Case {
	/// `Session::abort` after accepting, keeping the session handle.
	Abort,
	/// `Session::abort` after accepting, then dropping the session handle at once.
	AbortThenDrop,
	/// `Request::reject` during the handshake.
	Reject,
}

struct Server {
	quic: u16,
	websocket: u16,
}

async fn serve(case: Case) -> Server {
	let mut config = moq_tokio::server::Config::default();
	config.listen.bind = Some("127.0.0.1:0".parse().unwrap());
	config.listen.tls.generate = vec!["localhost".into()];
	config.websocket = Some(
		moq_tokio::websocket::Listener::bind("127.0.0.1:0".parse().unwrap())
			.await
			.expect("failed to bind websocket"),
	);
	let mut listener = config
		.init()
		.expect("failed to init server")
		.listen()
		.await
		.expect("failed to listen");
	let server = Server {
		quic: listener.local_addr().expect("no quic addr").port(),
		websocket: listener.websocket_local_addr().expect("no websocket addr").port(),
	};

	tokio::spawn(async move {
		while let Some(request) = listener.accept().await {
			tokio::spawn(async move {
				match case {
					Case::Reject => {
						request.reject(moq_tokio::server::Reject::App(CODE)).await.ok();
					}
					Case::Abort | Case::AbortThenDrop => {
						let Ok(session) = request.ok().await else { return };
						session.abort(moq_net::Error::App(CODE));
						if let Case::Abort = case {
							tokio::time::sleep(Duration::from_secs(5)).await;
						}
						drop(session);
					}
				}
			});
		}
	});

	server
}

/// The client's terminal error, whether the connect or the established session failed.
async fn client_error(url: &str, websocket: bool) -> moq_tokio::Error {
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	config.websocket.enabled = Some(websocket);
	let client = config.init(Default::default()).expect("failed to init client");

	let connection = client.with_reconnect(false).connect(url.parse::<url::Url>().unwrap());
	tokio::time::timeout(Duration::from_secs(10), async {
		match connection.established().await {
			Ok(connection) => connection.closed().await.expect_err("closed without an error"),
			Err(err) => err,
		}
	})
	.await
	.expect("the connection did not close")
}

async fn check(case: Case) {
	let server = serve(case).await;
	let urls = [
		(format!("https://localhost:{}/", server.quic), false),
		(format!("moqt://localhost:{}/", server.quic), false),
		(format!("ws://127.0.0.1:{}/", server.websocket), true),
	];

	let mut failures = Vec::new();
	for (url, websocket) in &urls {
		for _ in 0..RUNS {
			let err = client_error(url, *websocket).await;
			if !matches!(
				err,
				moq_tokio::Error::MoqNet(moq_net::Error::Session(moq_net::SessionError::App(CODE)))
			) {
				failures.push(format!("{url}: {err:?}"));
			}
		}
	}
	assert!(
		failures.is_empty(),
		"{case:?} lost the close code:\n{}",
		failures.join("\n")
	);
}

#[tokio::test]
async fn abort() {
	check(Case::Abort).await;
}

#[tokio::test]
async fn abort_then_drop() {
	check(Case::AbortThenDrop).await;
}

#[tokio::test]
async fn reject() {
	check(Case::Reject).await;
}
