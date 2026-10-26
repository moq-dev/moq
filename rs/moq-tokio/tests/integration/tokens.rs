//! Configured tokens reach the server as the connection credential: in SETUP where the
//! negotiated version carries one, and in the URL query where it might not. A refused
//! credential is dialed again only once the client has a new one.

#![cfg(feature = "quic")]

use std::{sync::Arc, time::Duration};

use moq_tokio::moq_net;

/// What the server saw on one handshake.
#[derive(Debug, PartialEq)]
struct Seen {
	query: Option<String>,
	token: Option<String>,
}

struct Server {
	port: u16,
	seen: tokio::sync::mpsc::UnboundedReceiver<Seen>,
	/// Releases a held `refused` handshake to be rejected.
	release: Arc<tokio::sync::Notify>,
}

/// Serve `version`, rejecting as unauthorized any handshake presenting `refused`, once
/// [`Server::release`] lets it go.
async fn serve(version: &str, refused: &'static str) -> Server {
	let mut config = moq_tokio::server::Config::default();
	config.listen.bind = Some("127.0.0.1:0".parse().unwrap());
	config.listen.tls.generate = vec!["localhost".into()];
	config.listen.version = vec![version.parse().unwrap()];
	let mut listener = config
		.init()
		.expect("failed to init server")
		.listen()
		.await
		.expect("failed to listen");
	let port = listener.local_addr().expect("no quic addr").port();
	let (sender, seen) = tokio::sync::mpsc::unbounded_channel();
	let release = Arc::new(tokio::sync::Notify::new());

	let notify = release.clone();
	tokio::spawn(async move {
		while let Some(request) = listener.accept().await {
			let seen = Seen {
				query: request.query().map(str::to_string),
				token: request
					.token()
					.map(|token| String::from_utf8(token.value.clone()).unwrap()),
			};
			let rejected = seen.token.as_deref() == Some(refused)
				|| seen
					.query
					.as_deref()
					.is_some_and(|query| query.contains(&format!("jwt={refused}")));
			sender.send(seen).ok();
			let notify = notify.clone();
			tokio::spawn(async move {
				if rejected {
					notify.notified().await;
					request.reject(moq_net::SessionError::Unauthorized).await.ok();
					return;
				}
				let Ok(_session) = request.ok().await else {
					return;
				};
				// Hold the session until the test's runtime shuts down.
				std::future::pending::<()>().await;
			});
		}
	});

	Server { port, seen, release }
}

fn client(version: &str, tokens: &[&str]) -> moq_tokio::Client {
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	config.version = vec![version.parse().unwrap()];
	config.tokens = tokens.iter().map(|token| token.parse().unwrap()).collect();
	config.init(Default::default()).expect("failed to init client")
}

async fn next(server: &mut Server) -> Seen {
	tokio::time::timeout(Duration::from_secs(10), server.seen.recv())
		.await
		.expect("no handshake arrived")
		.unwrap()
}

/// lite-07 carries the credential in SETUP, so the URL stays as the caller wrote it.
#[tokio::test]
async fn lite_07_presents_the_token_in_setup() {
	let mut server = serve("moq-lite-07-wip", "refused").await;
	let client = client("moq-lite-07-wip", &["credential"]);
	let url: url::Url = format!("moqt://localhost:{}/room", server.port).parse().unwrap();
	let _connection = client.connect(url);
	assert_eq!(
		next(&mut server).await,
		Seen {
			query: None,
			token: Some("credential".into()),
		}
	);
}

/// lite-05 has no SETUP token, so the credential rides the request target's query.
#[tokio::test]
async fn lite_05_presents_the_token_in_the_url() {
	let mut server = serve("moq-lite-05", "refused").await;
	let client = client("moq-lite-05", &["credential"]);
	let url: url::Url = format!("moqt://localhost:{}/room", server.port).parse().unwrap();
	let _connection = client.connect(url);
	assert_eq!(
		next(&mut server).await,
		Seen {
			query: Some("jwt=credential".into()),
			token: None,
		}
	);
}

/// A refused credential the client has since replaced is dialed again with the new one.
#[tokio::test]
async fn a_refused_credential_redials_with_its_replacement() {
	let mut server = serve("moq-lite-07-wip", "old").await;
	let client = client("moq-lite-07-wip", &["old"]);
	let url: url::Url = format!("moqt://localhost:{}/", server.port).parse().unwrap();
	let connection = client.connect(url);
	assert_eq!(next(&mut server).await.token.as_deref(), Some("old"));

	let tokens = connection.tokens();
	tokens.set(vec!["new".parse().unwrap()]);
	server.release.notify_one();
	assert_eq!(next(&mut server).await.token.as_deref(), Some("new"));
	tokio::time::timeout(Duration::from_secs(10), tokens.answered())
		.await
		.expect("the replacement was never answered")
		.expect("the replacement was refused");
	assert!(connection.connected());
}

/// Without a new credential, a refusal ends the connection rather than retrying it.
#[tokio::test]
async fn an_unchanged_refused_credential_is_terminal() {
	let mut server = serve("moq-lite-07-wip", "old").await;
	let client = client("moq-lite-07-wip", &["old"]);
	let url: url::Url = format!("moqt://localhost:{}/", server.port).parse().unwrap();
	let connection = client.connect(url);
	let tokens = connection.tokens();
	assert_eq!(next(&mut server).await.token.as_deref(), Some("old"));
	server.release.notify_one();
	// A lite client is live before the server's answer, so the refusal may close it after.
	let err = tokio::time::timeout(Duration::from_secs(10), async {
		match connection.established().await {
			Ok(connection) => connection.closed().await.expect_err("closed without an error"),
			Err(err) => err,
		}
	})
	.await
	.expect("the refusal never ended the connection");
	let unauthorized =
		|err: &moq_tokio::Error| matches!(err, moq_tokio::Error::MoqNet(moq_net::Error::Session(moq_net::SessionError::Unauthorized)));
	assert!(unauthorized(&err), "{err:?}");
	// The refusal is the answer to the tokens that dial presented.
	let answer = tokens.answered().await.expect_err("the refused credential was answered");
	assert!(unauthorized(&answer), "{answer:?}");
}
