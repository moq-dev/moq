//! AUTH admission bursts across sessions and tokens per session, without sockets.
//!
//! Measures the pending queue and complete grant/withdrawal lifecycle. Run the same
//! matrix before and after queue changes so a per-session scan appears on its own axis.
//!
//! `renewal` times swapping the auth of every announced route on one origin, until
//! the session that presented both auths has renewed each announcement and its peer
//! accepted every token. It sweeps the routes and the sessions publishing that origin:
//! the others carry no auth for those routes, so their cost is waking on a change that
//! sends nothing.
//!
//! `named` times narrowing one of the peer's lite-07 tokens off the one route that names it,
//! and widening it back, until the subscriber has withheld and restored that route. It
//! sweeps the tokens the peer holds and the routes naming the others, which the change
//! leaves alone: a cost growing with either is a scan of the whole table.

#[path = "../tests/integration/support/mod.rs"]
mod support;

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use futures::future::join_all;
use moq_net::{Hop, Session, Version, auth, origin};
use std::time::Duration;
use support::{harness, mock::create_mock_session_pair};

struct Pair {
	client: Session,
	_server: Session,
	requests: auth::Requests,
	_initial: auth::Issued,
}

async fn pair(version: Version) -> Pair {
	let mut mock = moq_net::transport::memory::Config::default();
	mock.protocol = Some(version.alpn().to_string());
	let (client, server) = create_mock_session_pair(mock);
	let client = async {
		let (session, driver) = moq_net::Client::new()
			.with_versions(version.into())
			.connect(harness::now(), client)
			.await
			.unwrap();
		harness::spawn(driver);
		session
	};
	let server = async {
		let request = moq_net::Server::new()
			.with_versions(version.into())
			.accept_request(harness::now(), server)
			.await
			.unwrap();
		let requests = request.auth().requests().unwrap();
		let (session, driver) = request.ok().await.unwrap();
		harness::spawn(driver);
		(session, requests)
	};
	let (client, (server, mut requests)) = futures::join!(client, server);
	let initial = requests.next().await.unwrap().accept(auth::Grant::all());
	Pair {
		client,
		_server: server,
		requests,
		_initial: initial,
	}
}

impl Pair {
	async fn round(&mut self, count: usize) {
		let handle = self.client.auth();
		let present = join_all((0..count).map(|_| handle.add(Bytes::from_static(b"token"))));
		let accept = async {
			let mut issued = Vec::with_capacity(count);
			for _ in 0..count {
				issued.push(self.requests.next().await.unwrap().accept(auth::Grant::all()));
			}
			issued
		};
		let (tokens, issued) = futures::join!(present, accept);
		let tokens: Vec<_> = tokens.into_iter().map(Result::unwrap).collect();
		drop(tokens);
		join_all(issued.iter().map(auth::Issued::closed)).await;
	}
}

fn admission(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	for version in ["moq-lite-07-wip", "moq-transport-22"] {
		let mut group = c.benchmark_group(format!("auth/{version}"));
		group.sample_size(10);
		group.warm_up_time(Duration::from_millis(100));
		group.measurement_time(Duration::from_millis(300));
		for sessions in [1, 16, 64] {
			for tokens in [1, 16, 128] {
				group.throughput(Throughput::Elements((sessions * tokens) as u64));
				group.bench_with_input(
					BenchmarkId::new(sessions.to_string(), tokens),
					&(sessions, tokens),
					|b, &(sessions, tokens)| {
						let mut pairs =
							runtime.block_on(join_all((0..sessions).map(|_| pair(version.parse().unwrap()))));
						b.iter(|| runtime.block_on(join_all(pairs.iter_mut().map(|pair| pair.round(tokens)))));
					},
				);
			}
		}
		group.finish();
	}
}

/// A client publishing `origin` to its own server, whose session and token requests follow.
async fn publisher(origin: &origin::Producer) -> (Session, Session, auth::Requests) {
	let version: Version = "moq-transport-22".parse().unwrap();
	let mut mock = moq_net::transport::memory::Config::default();
	mock.protocol = Some(version.alpn().to_string());
	let (client, server) = create_mock_session_pair(mock);
	// Tokens ride requests alone, and the client announces with PUBLISH_NAMESPACE.
	let mut client_extensions = moq_net::setup::Extensions::default();
	client_extensions.auth = false;
	let mut server_extensions = moq_net::setup::Extensions::default();
	server_extensions.solicit = false;
	let (subscribe, driver) = origin::Producer::new(origin::Config::new(Hop::new(1).unwrap()));
	harness::spawn(driver);
	let client = async {
		let (session, driver) = moq_net::Client::new()
			.with_versions(version.into())
			.with_extensions(client_extensions)
			.with_publisher(origin.consume())
			.connect(harness::now(), client)
			.await
			.unwrap();
		harness::spawn(driver);
		session
	};
	let server = async {
		let request = moq_net::Server::new()
			.with_versions(version.into())
			.with_extensions(server_extensions)
			.with_subscriber(subscribe)
			.accept_request(harness::now(), server)
			.await
			.unwrap();
		let requests = request.auth().requests().unwrap();
		let (session, driver) = request.ok().await.unwrap();
		harness::spawn(driver);
		(session, requests)
	};
	let (client, (server, requests)) = futures::join!(client, server);
	(client, server, requests)
}

fn renewal(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let mut group = c.benchmark_group("auth/renewal");
	group.sample_size(10);
	group.warm_up_time(Duration::from_millis(100));
	group.measurement_time(Duration::from_millis(300));
	for sessions in [1, 16, 64] {
		for routes in [1, 16, 128] {
			group.throughput(Throughput::Elements(routes as u64));
			group.bench_with_input(
				BenchmarkId::new(sessions.to_string(), routes),
				&(sessions, routes),
				|b, &(sessions, routes)| {
					let _guard = runtime.enter();
					// No hold, so an iteration times the renewal rather than the origin batching it.
					let mut config = origin::Config::new(Hop::new(2).unwrap());
					config.update_hold = Duration::ZERO;
					let (origin, driver) = origin::Producer::new(config);
					harness::spawn(driver);
					let mut publishers = runtime.block_on(join_all((0..sessions).map(|_| publisher(&origin))));
					let (session, _, requests) = &mut publishers[0];
					let handle = session.auth();
					let auths =
						runtime.block_on(async { [handle.add("a").await.unwrap(), handle.add("b").await.unwrap()] });
					let broadcasts: Vec<_> = (0..routes)
						.map(|route| origin.create_broadcast(format!("room/{route}").as_str()).unwrap())
						.collect();
					let mut swap = |auth: &auth::Auth| {
						for broadcast in &broadcasts {
							broadcast
								.announce(origin::Route::default().with_auth(auth.clone()))
								.unwrap();
						}
						runtime.block_on(async {
							let mut issued = Vec::with_capacity(routes);
							for _ in 0..routes {
								issued.push(requests.next().await.unwrap().accept(auth::Grant::all()));
							}
							issued
						})
					};
					// Each round's grants replace the last round's, which drop once their
					// renewals are accepted.
					let mut _held = swap(&auths[0]);
					let mut next = 1;
					b.iter(|| {
						_held = swap(&auths[next]);
						next ^= 1;
					});
				},
			);
		}
	}
	group.finish();
}

/// A lite-07 client publishing `origin` to a server subscribing into `relay`, with the
/// server's token requests.
async fn named_pair(origin: &origin::Producer, relay: origin::Producer) -> (Session, Session, auth::Requests) {
	let version: Version = "moq-lite-07-wip".parse().unwrap();
	let mut mock = moq_net::transport::memory::Config::default();
	mock.protocol = Some(version.alpn().to_string());
	let (client, server) = create_mock_session_pair(mock);
	let client = async {
		let (session, driver) = moq_net::Client::new()
			.with_versions(version.into())
			.with_publisher(origin.consume())
			.connect(harness::now(), client)
			.await
			.unwrap();
		harness::spawn(driver);
		session
	};
	let server = async {
		let request = moq_net::Server::new()
			.with_versions(version.into())
			.with_subscriber(relay)
			.accept_request(harness::now(), server)
			.await
			.unwrap();
		let requests = request.auth().requests().unwrap();
		let (session, driver) = request.ok().await.unwrap();
		harness::spawn(driver);
		(session, requests)
	};
	let (client, (server, requests)) = futures::join!(client, server);
	(client, server, requests)
}

/// Read `announced` until `path` starts (`true`) or ends (`false`).
async fn announced(announced: &mut moq_net::announce::Consumer, path: &str, active: bool) {
	loop {
		match announced.next().await.unwrap() {
			moq_net::announce::Event::Start(update) | moq_net::announce::Event::Restart(update)
				if active && update.prefix.as_str() == path =>
			{
				return;
			}
			moq_net::announce::Event::End(update) if !active && update.prefix.as_str() == path => return,
			_ => {}
		}
	}
}

fn named(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let mut group = c.benchmark_group("auth/named");
	group.sample_size(10);
	group.warm_up_time(Duration::from_millis(100));
	group.measurement_time(Duration::from_millis(300));
	for tokens in [1, 16, 128] {
		for routes in [1, 16, 128] {
			group.throughput(Throughput::Elements(1));
			group.bench_with_input(
				BenchmarkId::new(tokens.to_string(), routes),
				&(tokens, routes),
				|b, &(tokens, routes)| {
					let _guard = runtime.enter();
					let (origin, driver) = origin::Producer::new(origin::Config::new(Hop::new(2).unwrap()));
					harness::spawn(driver);
					// No hold, so an iteration times the change rather than the origin batching it.
					let mut config = origin::Config::new(Hop::new(1).unwrap());
					config.update_hold = Duration::ZERO;
					let (relay, driver) = origin::Producer::new(config);
					harness::spawn(driver);
					let mut watch = relay.consume().announced();
					let (client, _server, mut requests) = runtime.block_on(named_pair(&origin, relay));
					let handle = client.auth();
					let (auths, token, _issued) = runtime.block_on(async {
						let initial = requests.next().await.unwrap().accept(auth::Grant::all());
						// Token 0 first and alone, so its grant is the one answered next.
						let (first, token) = futures::join!(handle.add(Bytes::from_static(b"token")), async {
							requests.next().await.unwrap().accept(auth::Grant::all())
						});
						let present = join_all((1..tokens).map(|_| handle.add(Bytes::from_static(b"token"))));
						let accept = async {
							let mut issued = vec![initial];
							for _ in 1..tokens {
								issued.push(requests.next().await.unwrap().accept(auth::Grant::all()));
							}
							issued
						};
						let (rest, issued) = futures::join!(present, accept);
						let auths: Vec<_> = std::iter::once(first).chain(rest).map(Result::unwrap).collect();
						(auths, token, issued)
					});
					// Route 0 alone names token 0; the rest share the other tokens, or the
					// session's scope when there are none.
					let broadcasts: Vec<_> = (0..routes)
						.map(|route| {
							let broadcast = origin.create_broadcast(format!("room/{route}").as_str()).unwrap();
							let auth = match (route, tokens) {
								(0, _) => Some(auths[0].clone()),
								(_, 1) => None,
								(route, tokens) => Some(auths[1 + route % (tokens - 1)].clone()),
							};
							let route = match auth {
								Some(auth) => origin::Route::default().with_auth(auth),
								None => origin::Route::default(),
							};
							broadcast.announce(route).unwrap();
							broadcast
						})
						.collect();
					runtime.block_on(async {
						let mut pending: std::collections::HashSet<_> =
							(0..routes).map(|route| format!("room/{route}")).collect();
						while !pending.is_empty() {
							if let moq_net::announce::Event::Start(update) = watch.next().await.unwrap() {
								pending.remove(update.prefix.as_str());
							}
						}
					});
					b.iter(|| {
						runtime.block_on(async {
							token.update(auth::Grant::default());
							announced(&mut watch, "room/0", false).await;
							token.update(auth::Grant::all());
							announced(&mut watch, "room/0", true).await;
						})
					});
					drop(broadcasts);
				},
			);
		}
	}
	group.finish();
}

criterion_group!(benches, admission, renewal, named);
criterion_main!(benches);
