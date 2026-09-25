//! In-band AUTH over the in-memory mock transport: both sides learn their grant,
//! tokens union, and a publication outside the grant fails loud. Every case runs on
//! moq-lite-06 and on moq-transport with the MoQ Auth extension.

mod support;

use std::time::Duration;

use moq_net::{
	Client, Error, Hop, Pattern, Patterns, Server, Session, SessionError, StreamError, Version,
	auth::{self, Grant},
	origin,
};
use support::harness::{now, run};
use support::mock::{MockSession, create_mock_session_pair};

/// Maximum time any single test may run before being treated as a deadlock.
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

const LITE_06: &str = "moq-lite-06";
/// The first draft that negotiates MoQ Auth, and the newest.
const MOQT_17: &str = "moq-transport-17";
const MOQT_22: &str = "moq-transport-22";

/// Run each case on every version that exchanges AUTH.
macro_rules! cases {
	($($case:ident),* $(,)?) => {
		mod lite_06 {
			$(#[tokio::test] async fn $case() { super::$case(super::LITE_06).await })*
		}
		mod moqt_17 {
			$(#[tokio::test] async fn $case() { super::$case(super::MOQT_17).await })*
		}
		mod moqt_22 {
			$(#[tokio::test] async fn $case() { super::$case(super::MOQT_22).await })*
		}
	};
}

cases!(
	both_sides_learn_their_grant_from_scoped_origins,
	a_publish_only_session_grants_no_subscribe,
	an_out_of_scope_announce_aborts_with_the_path,
	a_broadcast_published_before_the_grant_is_checked,
	an_unanswered_token_does_not_suspend_the_check,
	tokens_union_and_withdrawing_one_shrinks_it,
	an_update_replaces_one_tokens_grant,
	a_revoked_grant_withdraws_and_can_be_restored,
	a_refused_token_reports_the_code,
	a_refused_setup_token_grants_nothing,
	dropping_the_requests_refuses_queued_tokens,
	a_closed_session_holds_no_grant,
	a_reset_auth_stream_reports_unsupported,
	a_revoked_grant_cancels_its_subscriptions,
	an_unrepresentable_grant_is_unsupported,
	an_unrepresentable_update_revokes_only_its_token,
	nothing_outside_the_grant_reaches_the_peer,
);

#[tokio::test]
async fn lite_05_has_no_grant() {
	older_versions_have_no_grant("moq-lite-05").await
}

#[tokio::test]
async fn moqt_16_has_no_grant() {
	older_versions_have_no_grant("moq-transport-16").await
}

/// Build an origin producer, spawning its driver on the ambient runtime.
fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	tokio::spawn(run(driver));
	producer
}

fn patterns(prefixes: &[&str]) -> Patterns {
	prefixes
		.iter()
		.map(|prefix| Pattern::subtree(prefix).unwrap())
		.collect()
}

fn grant(publish: &[&str], subscribe: &[&str]) -> Grant {
	Grant {
		publish: patterns(publish),
		subscribe: patterns(subscribe),
		expires: None,
	}
}

/// Wait for a watch to hold a grant satisfying `f`.
async fn wait_for(mut watch: auth::Watch, f: impl Fn(&Option<Grant>) -> bool) -> Option<Grant> {
	loop {
		let current = watch.peek();
		if f(&current) {
			return current;
		}
		watch.changed().await.expect("grant watch ended");
	}
}

/// The union, once the peer has answered anything.
async fn granted(session: &Session) -> Grant {
	wait_for(session.auth().grant(), Option::is_some).await.unwrap()
}

/// Wait until `path` is announced (`true`) or retracted (`false`) in `origin`.
async fn wait_announced(origin: &origin::Consumer, path: &str, active: bool) {
	let mut announced = origin.announced();
	let mut live = std::collections::HashSet::new();
	loop {
		if live.contains(path) == active {
			return;
		}
		let update = announced.next().await.expect("origin closed");
		match update.kind.is_active() {
			true => live.insert(update.prefix.to_string()),
			false => live.remove(update.prefix.as_str()),
		};
	}
}

#[derive(Default)]
struct Options {
	client_publish: Option<origin::Producer>,
	client_subscribe: Option<origin::Producer>,
	server_publish: Option<origin::Producer>,
	server_subscribe: Option<origin::Producer>,
	/// Take the server's AUTH requests before its driver runs.
	server_requests: bool,
	version: Option<&'static str>,
}

struct Pair {
	client: Session,
	server: Session,
	client_transport: MockSession,
	requests: Option<auth::Requests>,
}

async fn connect(opts: Options) -> Pair {
	let version: Version = opts.version.unwrap_or(LITE_06).parse().unwrap();
	let (client_transport, server_transport) = create_mock_session_pair(Some(version.alpn()));

	let mut client = Client::new().with_versions(version.into());
	if let Some(publish) = &opts.client_publish {
		client = client.with_publisher(publish);
	}
	if let Some(subscribe) = opts.client_subscribe {
		client = client.with_subscriber(subscribe);
	}

	let mut server = Server::new().with_versions(version.into());
	if let Some(publish) = &opts.server_publish {
		server = server.with_publisher(publish);
	}
	if let Some(subscribe) = opts.server_subscribe {
		server = server.with_subscriber(subscribe);
	}

	let observe = client_transport.clone();
	let client_fut = async {
		let (session, driver) = client.connect(now(), client_transport).await.expect("client handshake");
		tokio::spawn(run(driver));
		session
	};
	let server_fut = async {
		let handshake = server
			.accept_request(now(), server_transport)
			.await
			.expect("server handshake");
		// Taken before the session starts, the way a relay verifying tokens would.
		let requests = opts
			.server_requests
			.then(|| handshake.auth().requests().expect("requests available before ok()"));
		let (session, driver) = handshake.ok().await.expect("server accept");
		tokio::spawn(run(driver));
		(session, requests)
	};
	let (client, (server, requests)) = tokio::join!(client_fut, server_fut);

	Pair {
		client,
		server,
		client_transport: observe,
		requests,
	}
}

/// Answer the peer's tokens from a table, holding every grant (and any request the
/// table leaves unanswered) for as long as the returned task lives.
fn serve(
	mut requests: auth::Requests,
	answer: impl Fn(&[u8]) -> Option<Grant> + Send + 'static,
) -> tokio::sync::mpsc::UnboundedReceiver<(Vec<u8>, auth::Issued)> {
	let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
	tokio::spawn(async move {
		let mut unanswered = Vec::new();
		while let Some(request) = requests.next().await {
			let token = request.token().to_vec();
			match answer(&token) {
				Some(grant) => {
					let _ = tx.send((token, request.accept(grant)));
				}
				None => unanswered.push(request),
			}
		}
	});
	rx
}

fn within<F: std::future::Future>(f: F) -> tokio::time::Timeout<F> {
	tokio::time::timeout(TEST_TIMEOUT, f)
}

/// Each side's default grant is what the other side's origin handles allow:
/// its subscribe half bounds what we may publish, its publish half what we may
/// subscribe to.
async fn both_sides_learn_their_grant_from_scoped_origins(version: &'static str) {
	within(async {
		let relay = produce_origin(1);
		let pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			client_subscribe: Some(produce_origin(3)),
			server_publish: Some(relay.scope("", &patterns(&["room"])).unwrap()),
			server_subscribe: Some(relay.scope("", &patterns(&["room/alice"])).unwrap()),
			..Default::default()
		})
		.await;

		assert_eq!(granted(&pair.client).await, grant(&["room/alice"], &["room"]));
		// The empty prefix: an unscoped origin grants everything.
		assert_eq!(granted(&pair.server).await, grant(&[""], &[""]));
	})
	.await
	.expect("timed out");
}

/// A missing half grants nothing: the empty list, distinct from the empty prefix.
async fn a_publish_only_session_grants_no_subscribe(version: &'static str) {
	within(async {
		let pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_subscribe: Some(produce_origin(1)),
			..Default::default()
		})
		.await;

		// The server may subscribe to what the client publishes, but publish nothing to
		// a client that never reads.
		assert_eq!(granted(&pair.server).await, grant(&[], &[""]));
		assert_eq!(granted(&pair.client).await, grant(&[""], &[]));
	})
	.await
	.expect("timed out");
}

/// A broadcast outside the grant aborts the session and names the path, where it
/// used to wait forever for a solicitation that never comes.
async fn an_out_of_scope_announce_aborts_with_the_path(version: &'static str) {
	within(async {
		let publisher = produce_origin(2);
		let relay = produce_origin(1);
		let pair = connect(Options {
			version: Some(version),
			client_publish: Some(publisher.clone()),
			server_subscribe: Some(relay.scope("", &patterns(&["baz"])).unwrap()),
			..Default::default()
		})
		.await;

		// In scope: served, and nothing aborts.
		let ok = publisher.create_broadcast("baz/ok").unwrap();
		ok.announce(Default::default()).unwrap();
		wait_announced(&relay.consume(), "baz/ok", true).await;
		assert_eq!(pair.client_transport.close_reason(), None);

		let bad = publisher.create_broadcast("foo/bar").unwrap();
		bad.announce(Default::default()).unwrap();
		let err = pair.server.closed().await;
		assert!(
			matches!(err, Error::Session(SessionError::Unauthorized)),
			"server saw {err:?}"
		);
		let (code, reason) = pair.client_transport.close_reason().expect("closed");
		assert_eq!(code, SessionError::Unauthorized.to_code());
		assert_eq!(reason, "unauthorized: foo/bar");
	})
	.await
	.expect("timed out");
}

/// A broadcast published before the grant arrives is checked at admission too.
async fn a_broadcast_published_before_the_grant_is_checked(version: &'static str) {
	within(async {
		let publisher = produce_origin(2);
		let early = publisher.create_broadcast("foo/bar").unwrap();
		early.announce(Default::default()).unwrap();

		let pair = connect(Options {
			version: Some(version),
			client_publish: Some(publisher.clone()),
			server_subscribe: Some(produce_origin(1).scope("", &patterns(&["baz"])).unwrap()),
			..Default::default()
		})
		.await;

		assert!(matches!(
			pair.server.closed().await,
			Error::Session(SessionError::Unauthorized)
		));
		assert_eq!(
			pair.client_transport.close_reason().map(|(_, reason)| reason),
			Some("unauthorized: foo/bar".to_string())
		);
	})
	.await
	.expect("timed out");
}

/// Enforcement waits only for the tokens the session presented itself: a peer that
/// never answers an app-added token cannot suspend it.
async fn an_unanswered_token_does_not_suspend_the_check(version: &'static str) {
	within(async {
		let publisher = produce_origin(2);
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(publisher.clone()),
			server_subscribe: Some(produce_origin(1)),
			server_requests: true,
			..Default::default()
		})
		.await;
		let _issued = serve(pair.requests.take().unwrap(), |token| {
			token.is_empty().then(|| grant(&["baz"], &[]))
		});

		let auth = pair.client.auth();
		let pending = tokio::spawn(async move { auth.add("never answered").await.map(|_| ()) });
		assert_eq!(granted(&pair.client).await, grant(&["baz"], &[]));

		let bad = publisher.create_broadcast("foo/bar").unwrap();
		bad.announce(Default::default()).unwrap();
		assert!(matches!(
			pair.server.closed().await,
			Error::Session(SessionError::Unauthorized)
		));
		// The session's close fails the token still waiting on its answer.
		assert!(pending.await.unwrap().is_err());
	})
	.await
	.expect("timed out");
}

/// Two tokens union; closing one shrinks the union and withdraws only what it alone
/// covered, without disconnecting.
async fn tokens_union_and_withdrawing_one_shrinks_it(version: &'static str) {
	within(async {
		let publisher = produce_origin(2);
		let relay = produce_origin(1);
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(publisher.clone()),
			server_subscribe: Some(relay.clone()),
			server_requests: true,
			..Default::default()
		})
		.await;
		let mut issued = serve(pair.requests.take().unwrap(), |token| match token {
			b"" => Some(grant(&["a"], &[])),
			b"t1" => Some(grant(&["b"], &[])),
			_ => None,
		});
		let (_, _setup) = issued.recv().await.unwrap();

		let t1 = pair.client.auth().add("t1").await.expect("t1 granted");
		let (_, t1_issued) = issued.recv().await.unwrap();
		assert_eq!(t1.grant().peek(), Some(grant(&["b"], &[])));
		assert_eq!(granted(&pair.client).await, grant(&["a", "b"], &[]));

		let a = publisher.create_broadcast("a/x").unwrap();
		a.announce(Default::default()).unwrap();
		let b = publisher.create_broadcast("b/y").unwrap();
		b.announce(Default::default()).unwrap();
		let served = relay.consume();
		wait_announced(&served, "a/x", true).await;
		wait_announced(&served, "b/y", true).await;

		drop(t1);
		// The acceptor learns the token is gone, and the presenter withdraws b/y.
		assert!(matches!(t1_issued.closed().await, Error::Cancel));
		wait_for(pair.client.auth().grant(), |g| g == &Some(grant(&["a"], &[]))).await;
		wait_announced(&served, "b/y", false).await;
		wait_announced(&served, "a/x", true).await;
		assert_eq!(pair.client_transport.close_reason(), None);
	})
	.await
	.expect("timed out");
}

/// An update replaces one token's grant and leaves the other alone.
async fn an_update_replaces_one_tokens_grant(version: &'static str) {
	within(async {
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_subscribe: Some(produce_origin(1)),
			server_requests: true,
			..Default::default()
		})
		.await;
		let mut issued = serve(pair.requests.take().unwrap(), |token| match token {
			b"" => Some(grant(&["a"], &[])),
			b"t1" => Some(grant(&["b"], &[])),
			_ => None,
		});
		let (_, _setup) = issued.recv().await.unwrap();
		let t1 = pair.client.auth().add("t1").await.unwrap();
		let (_, t1_issued) = issued.recv().await.unwrap();

		t1_issued.update(grant(&["c"], &["c"]));
		wait_for(t1.grant(), |g| g == &Some(grant(&["c"], &["c"]))).await;
		assert_eq!(granted(&pair.client).await, grant(&["a", "c"], &["c"]));
	})
	.await
	.expect("timed out");
}

/// A revocation withdraws what the grant covered, even though the broadcast stays in
/// the shared origin, and an empty union can be authorized again.
async fn a_revoked_grant_withdraws_and_can_be_restored(version: &'static str) {
	within(async {
		let publisher = produce_origin(2);
		let relay = produce_origin(1);
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(publisher.clone()),
			server_subscribe: Some(relay.clone()),
			server_requests: true,
			..Default::default()
		})
		.await;
		let mut issued = serve(pair.requests.take().unwrap(), |token| match token {
			b"" | b"again" => Some(grant(&["a"], &[])),
			_ => None,
		});
		let (_, setup) = issued.recv().await.unwrap();

		let a = publisher.create_broadcast("a/x").unwrap();
		a.announce(Default::default()).unwrap();
		let served = relay.consume();
		wait_announced(&served, "a/x", true).await;

		setup.revoke(SessionError::Unauthorized, "expired");
		wait_for(pair.client.auth().grant(), |g| g == &Some(Grant::default())).await;
		wait_announced(&served, "a/x", false).await;
		// Still published locally, and the session survives the empty union.
		assert!(publisher.consume().routed("a/x").await.is_some());
		assert_eq!(pair.client_transport.close_reason(), None);

		let _again = pair.client.auth().add("again").await.expect("re-authorized");
		wait_announced(&served, "a/x", true).await;
	})
	.await
	.expect("timed out");
}

/// A refused token surfaces the acceptor's code.
async fn a_refused_token_reports_the_code(version: &'static str) {
	within(async {
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_requests: true,
			..Default::default()
		})
		.await;
		let mut requests = pair.requests.take().unwrap();
		tokio::spawn(async move {
			while let Some(request) = requests.next().await {
				match request.token().is_empty() {
					true => {
						std::mem::forget(request.accept(Grant::default()));
					}
					false => request.reject(SessionError::Unauthorized, "bad signature"),
				}
			}
		});

		let err = pair.client.auth().add("forged").await.err().expect("refused");
		assert!(matches!(err, Error::Session(SessionError::Unauthorized)), "{err:?}");
	})
	.await
	.expect("timed out");
}

/// Refusing the setup token grants nothing: the union becomes empty rather than
/// staying unknown, which the gates would read as unrestricted.
async fn a_refused_setup_token_grants_nothing(version: &'static str) {
	within(async {
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_requests: true,
			..Default::default()
		})
		.await;
		let mut requests = pair.requests.take().unwrap();
		tokio::spawn(async move {
			while let Some(request) = requests.next().await {
				request.reject(SessionError::Unauthorized, "bad credential");
			}
		});

		assert_eq!(granted(&pair.client).await, Grant::default());
	})
	.await
	.expect("timed out");
}

/// Dropping the requests refuses tokens already queued, not only later ones.
async fn dropping_the_requests_refuses_queued_tokens(version: &'static str) {
	within(async {
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_requests: true,
			..Default::default()
		})
		.await;
		let mut requests = pair.requests.take().unwrap();
		// Wait for the setup token to be queued, then hand it back to the queue's owner.
		let first = requests.next().await.expect("setup token");
		drop(first.accept(Grant::default()));
		let auth = pair.client.auth();
		let pending = tokio::spawn(async move { auth.add("queued").await.map(drop) });
		// Give the second token time to reach the queue before dropping it.
		tokio::time::sleep(Duration::from_millis(50)).await;
		drop(requests);

		let err = pending.await.unwrap().expect_err("refused");
		assert!(matches!(err, Error::Session(SessionError::Unauthorized)), "{err:?}");
	})
	.await
	.expect("timed out");
}

/// A closed session holds no grant: every token ended with it.
async fn a_closed_session_holds_no_grant(version: &'static str) {
	within(async {
		let pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_subscribe: Some(produce_origin(1)),
			..Default::default()
		})
		.await;
		assert_ne!(granted(&pair.client).await, Grant::default());
		let token = pair.client.auth().grant();

		pair.server.abort(Error::Cancel);
		wait_for(token, |g| g == &Some(Grant::default())).await;
	})
	.await
	.expect("timed out");
}

/// A peer that takes no tokens in band says so (lite resets the stream, moq-transport
/// answers NOT_SUPPORTED), which reads as unsupported rather than a refusal: the same as
/// a peer that predates AUTH.
async fn a_reset_auth_stream_reports_unsupported(version: &'static str) {
	within(async {
		let pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_subscribe: Some(produce_origin(1)),
			..Default::default()
		})
		.await;
		granted(&pair.client).await;

		let err = pair.client.auth().add("token").await.err().expect("no acceptor");
		assert!(matches!(err, Error::Unsupported), "{err:?}");
		assert_eq!(pair.client_transport.close_reason(), None);
	})
	.await
	.expect("timed out");
}

/// Versions without AUTH never open the stream: no grant, and no way to add a token.
async fn older_versions_have_no_grant(version: &'static str) {
	within(async {
		let pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_subscribe: Some(produce_origin(1)),
			..Default::default()
		})
		.await;
		assert_eq!(pair.client.auth().grant().peek(), None);
		assert!(matches!(pair.client.auth().add("token").await, Err(Error::Unsupported)));
	})
	.await
	.expect("timed out");
}

/// Losing a grant cancels the subscriptions it covered, in both directions, and
/// leaves the session up.
async fn a_revoked_grant_cancels_its_subscriptions(version: &'static str) {
	within(async {
		let ts = |ms| moq_net::Timestamp::from_millis(ms).unwrap();
		let prefs = || moq_net::track::Subscription::default().with_max_age(Duration::from_secs(10));

		// The server publishes room/x to the client; the client publishes up/y to the server.
		let server_origin = produce_origin(1);
		let down = server_origin.create_broadcast("room/x").unwrap();
		let down_track = down.create_track("video", None).unwrap();
		down.announce(Default::default()).unwrap();

		let client_origin = produce_origin(2);
		let up = client_origin.create_broadcast("up/y").unwrap();
		let up_track = up.create_track("video", None).unwrap();
		up.announce(Default::default()).unwrap();

		let received = produce_origin(3);
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(client_origin.clone()),
			client_subscribe: Some(received.clone()),
			server_publish: Some(server_origin.clone()),
			server_subscribe: Some(server_origin.clone()),
			server_requests: true,
		})
		.await;
		let mut issued = serve(pair.requests.take().unwrap(), |token| {
			token.is_empty().then(|| grant(&["up"], &["room"]))
		});
		let (_, setup) = issued.recv().await.unwrap();

		let mut group = down_track.append_group().unwrap();
		group.write_frame(ts(0), b"down".as_ref()).unwrap();
		let mut group = up_track.append_group().unwrap();
		group.write_frame(ts(0), b"up".as_ref()).unwrap();

		let remote = received.consume().routed_broadcast("room/x").await.unwrap();
		let mut down_sub = remote.track("video").unwrap().subscribe(prefs()).await.unwrap();
		down_sub.recv_group().await.unwrap().unwrap();

		let remote = server_origin.consume().routed_broadcast("up/y").await.unwrap();
		let mut up_sub = remote.track("video").unwrap().subscribe(prefs()).await.unwrap();
		up_sub.recv_group().await.unwrap().unwrap();

		setup.revoke(SessionError::Unauthorized, "expired");
		let err = down_sub
			.recv_group()
			.await
			.err()
			.expect("subscription outlived its grant");
		assert!(matches!(err, Error::Unauthorized), "{err:?}");
		// The served side ends too: the client stops serving what it may no longer publish.
		loop {
			match up_sub.recv_group().await {
				Ok(Some(_)) => continue,
				Ok(None) => panic!("served subscription finished instead of ending"),
				Err(_) => break,
			}
		}
		assert_eq!(pair.client_transport.close_reason(), None);

		// Neither stream claims the session closed. moq-lite resets both with UNAUTHORIZED;
		// moq-transport has no such stream code and reports it on the request instead.
		let resets = pair.client_transport.resets();
		let closed = StreamError::Session(SessionError::Unauthorized).to_code();
		assert!(!resets.contains(&closed), "{resets:x?}");
		if version == LITE_06 {
			let unauthorized = StreamError::Unauthorized.to_code();
			let count = resets.iter().filter(|&&code| code == unauthorized).count();
			assert_eq!(count, 2, "both subscriptions reset with UNAUTHORIZED: {resets:x?}");
		}
	})
	.await
	.expect("timed out");
}

/// A grant the wire cannot carry as prefixes is never widened: the token is refused as
/// unsupported, promptly, and the union stays unknown rather than empty.
async fn an_unrepresentable_grant_is_unsupported(version: &'static str) {
	within(async {
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_subscribe: Some(produce_origin(1)),
			server_requests: true,
			..Default::default()
		})
		.await;
		let mut issued = serve(pair.requests.take().unwrap(), |token| match token {
			b"" => Some(grant(&["a"], &[])),
			b"exact" => Some(Grant {
				publish: Patterns::from(Pattern::try_from("room/alice").unwrap()),
				subscribe: Patterns::new(),
				expires: None,
			}),
			b"mixed" => Some(Grant {
				publish: ["room/**", "lobby"]
					.into_iter()
					.map(|p| Pattern::try_from(p).unwrap())
					.collect(),
				subscribe: Patterns::new(),
				expires: None,
			}),
			b"wildcard" => Some(Grant {
				publish: Patterns::from(Pattern::try_from("room/*/cam").unwrap()),
				subscribe: Patterns::new(),
				expires: None,
			}),
			_ => None,
		});
		let (_, _setup) = issued.recv().await.unwrap();
		assert_eq!(granted(&pair.client).await, grant(&["a"], &[]));

		for token in ["exact", "mixed", "wildcard"] {
			let err = pair.client.auth().add(token).await.err().expect("not representable");
			assert!(matches!(err, Error::Unsupported), "{token}: {err:?}");
		}
		// The other token is untouched, and so is the session.
		assert_eq!(pair.client.auth().grant().peek(), Some(grant(&["a"], &[])));
		assert_eq!(pair.client_transport.close_reason(), None);
	})
	.await
	.expect("timed out");
}

/// An update the wire cannot carry revokes that token's earlier grant, and only that
/// token's: the rest of the union and the session stay.
async fn an_unrepresentable_update_revokes_only_its_token(version: &'static str) {
	within(async {
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_subscribe: Some(produce_origin(1)),
			server_requests: true,
			..Default::default()
		})
		.await;
		let mut issued = serve(pair.requests.take().unwrap(), |token| match token {
			b"" => Some(grant(&["a"], &[])),
			b"t1" => Some(grant(&["b"], &[])),
			_ => None,
		});
		let (_, _setup) = issued.recv().await.unwrap();
		let t1 = pair.client.auth().add("t1").await.unwrap();
		let (_, t1_issued) = issued.recv().await.unwrap();
		assert_eq!(granted(&pair.client).await, grant(&["a", "b"], &[]));

		t1_issued.update(Grant {
			publish: Patterns::from(Pattern::try_from("b/exact").unwrap()),
			subscribe: Patterns::new(),
			expires: None,
		});
		// Lite resets the stream and moq-transport answers NOT_SUPPORTED; either ends it.
		t1.closed().await;
		assert_eq!(t1.grant().peek(), None);
		wait_for(pair.client.auth().grant(), |g| g == &Some(grant(&["a"], &[]))).await;
		assert_eq!(pair.client_transport.close_reason(), None);
	})
	.await
	.expect("timed out");
}

/// The session closes before the peer ever hears of a broadcast outside the grant,
/// even one published before the grant arrived: nothing is advertised until the
/// setup token is answered.
async fn nothing_outside_the_grant_reaches_the_peer(version: &'static str) {
	within(async {
		let publisher = produce_origin(2);
		let early = publisher.create_broadcast("foo/bar").unwrap();
		early.announce(Default::default()).unwrap();

		// The relay accepts anything; only the grant it tells the client is narrow.
		let relay = produce_origin(1);
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(publisher.clone()),
			server_subscribe: Some(relay.clone()),
			server_requests: true,
			..Default::default()
		})
		.await;

		// Hold the answer, so the peer's discovery request is in long before the grant.
		let mut requests = pair.requests.take().unwrap();
		let setup = requests.next().await.expect("setup token");
		let leaked = tokio::time::timeout(
			Duration::from_millis(100),
			wait_announced(&relay.consume(), "foo/bar", true),
		)
		.await;
		assert!(leaked.is_err(), "advertised before the grant was known");

		let _issued = setup.accept(grant(&["baz"], &[]));
		assert!(matches!(
			pair.server.closed().await,
			Error::Session(SessionError::Unauthorized)
		));
	})
	.await
	.expect("timed out");
}
