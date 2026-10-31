//! AUTH admission bursts across sessions and tokens per session, without sockets.
//!
//! Measures the pending queue and complete grant/withdrawal lifecycle. Run the same
//! matrix before and after queue changes so a per-session scan appears on its own axis.
//!
//! `renewal` times swapping the auth of every announced route on one origin, until
//! the session that presented both auths has renewed each announcement and its peer
//! accepted every token. `renewal_one` changes one route while the others remain idle.
//! Both sweep the routes and the sessions publishing that origin:
//! the others carry no auth for those routes, so their cost is waking on a change that
//! sends nothing.

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

struct Origins<'a> {
	publish: &'a origin::Producer,
	subscribe: origin::Producer,
}

async fn pair(version: Version, origins: Option<Origins<'_>>) -> Pair {
	let mut mock = moq_net::transport::memory::Config::default();
	mock.protocol = Some(version.alpn().to_string());
	let (client, server) = create_mock_session_pair(mock);
	let client = async {
		let mut config = moq_net::Client::new().with_versions(version.into());
		if let Some(origins) = &origins {
			config = config.with_publisher(origins.publish);
		}
		let (session, driver) = config.connect(harness::now(), client).await.unwrap();
		harness::spawn(driver);
		session
	};
	let server = async {
		let mut config = moq_net::Server::new().with_versions(version.into());
		if let Some(origins) = &origins {
			config = config.with_subscriber(origins.subscribe.clone());
		}
		let mut request = config.accept_request(harness::now(), server).await.unwrap();
		let requests = request.requests();
		let (session, driver) = request.ok().await.unwrap();
		harness::spawn(driver);
		(session, requests)
	};
	let (client, (server, mut requests)) = futures::join!(client, server);
	let initial = requests.next().await.unwrap().accept(match origins {
		Some(_) => auth::Grant::default(),
		None => auth::Grant::all(),
	});
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
							runtime.block_on(join_all((0..sessions).map(|_| pair(version.parse().unwrap(), None))));
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
		let mut request = moq_net::Server::new()
			.with_versions(version.into())
			.with_extensions(server_extensions)
			.with_subscriber(subscribe)
			.accept_request(harness::now(), server)
			.await
			.unwrap();
		let requests = request.requests();
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
	for (name, single) in [("auth/renewal", false), ("auth/renewal_one", true)] {
		let mut group = c.benchmark_group(name);
		group.sample_size(10);
		group.warm_up_time(Duration::from_millis(100));
		group.measurement_time(Duration::from_millis(300));
		for sessions in [1, 16, 64] {
			for routes in [1, 16, 128] {
				let touched = if single { 1 } else { routes };
				group.throughput(Throughput::Elements(touched as u64));
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
						let auths = runtime
							.block_on(async { [handle.add("a").await.unwrap().0, handle.add("b").await.unwrap().0] });
						let broadcasts: Vec<_> = (0..routes)
							.map(|route| origin.create_broadcast(format!("room/{route}").as_str()).unwrap())
							.collect();
						let mut swap = |auth: &auth::Credential, touched: usize| {
							for broadcast in broadcasts.iter().take(touched) {
								broadcast
									.announce(origin::Route::default().with_auth(auth.clone()))
									.unwrap();
							}
							runtime.block_on(async {
								let mut issued = Vec::with_capacity(touched);
								for _ in 0..touched {
									issued.push(requests.next().await.unwrap().accept(auth::Grant::all()));
								}
								issued
							})
						};
						// Each round's grants replace the last round's, which drop once their
						// renewals are accepted.
						let mut held = swap(&auths[0], routes);
						let mut next = 1;
						b.iter(|| {
							let replacements = swap(&auths[next], touched);
							drop(held.splice(..touched, replacements));
							next ^= 1;
						});
					},
				);
			}
		}
		group.finish();
	}
}

/// Publications held behind unanswered tokens, sweeping both the token table and paths.
fn waiting(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	for version in ["moq-lite-07-wip", "moq-transport-22"] {
		let mut group = c.benchmark_group(format!("auth/waiting/{version}"));
		group.sample_size(10);
		group.warm_up_time(Duration::from_millis(100));
		group.measurement_time(Duration::from_millis(300));
		for tokens in [1, 16, 128] {
			for paths in [1, 16, 128] {
				group.throughput(Throughput::Elements(paths as u64));
				group.bench_with_input(
					BenchmarkId::new(tokens.to_string(), paths),
					&(tokens, paths),
					|b, &(tokens, paths)| {
						let _guard = runtime.enter();
						let mut config = origin::Config::new(Hop::new(2).unwrap());
						config.update_hold = Duration::ZERO;
						let (publish, driver) = origin::Producer::new(config);
						harness::spawn(driver);
						let (receive, driver) = origin::Producer::new(origin::Config::new(Hop::new(1).unwrap()));
						harness::spawn(driver);
						let mut pair = runtime.block_on(pair(
							version.parse().unwrap(),
							Some(Origins {
								publish: &publish,
								subscribe: receive.clone(),
							}),
						));
						let mut generation = 0;
						b.iter(|| {
							runtime.block_on(async {
								let handle = pair.client.auth();
								let mut presenting =
									std::pin::pin!(join_all((0..tokens).map(|_| handle.add("pending"))));
								assert!(futures::poll!(presenting.as_mut()).is_pending());
								let mut pending = Vec::with_capacity(tokens);
								for _ in 0..tokens {
									pending.push(pair.requests.next().await.unwrap());
								}
								let prefix = format!("wait/{generation}/");
								generation += 1;
								let broadcasts: Vec<_> = (0..paths)
									.map(|path| {
										let broadcast =
											publish.create_broadcast(format!("{prefix}{path}").as_str()).unwrap();
										broadcast.announce(Default::default()).unwrap();
										broadcast
									})
									.collect();
								// Let the session reach its admission wait before the final reply widens it.
								tokio::task::yield_now().await;
								let mut issued = Vec::with_capacity(tokens);
								for (index, request) in pending.into_iter().enumerate() {
									issued.push(request.accept(if index + 1 == tokens {
										auth::Grant::all()
									} else {
										auth::Grant::default()
									}));
								}
								let tokens: Vec<_> = presenting.await.into_iter().map(Result::unwrap).collect();
								let mut announced = receive.consume().announced();
								let mut seen = std::collections::HashSet::new();
								while seen.len() < paths {
									match announced.next().await.unwrap() {
										moq_net::announce::Event::Start(update)
										| moq_net::announce::Event::Update(update)
										| moq_net::announce::Event::Restart(update)
										| moq_net::announce::Event::Takeover { announce: update, .. }
											if update.prefix.as_str().starts_with(&prefix) =>
										{
											seen.insert(update.prefix);
										}
										_ => {}
									}
								}
								drop(broadcasts);
								drop(tokens);
								join_all(issued.iter().map(auth::Issued::closed)).await;
							})
						});
					},
				);
			}
		}
		group.finish();
	}
}

criterion_group!(benches, admission, renewal, waiting);
criterion_main!(benches);
